//! Files Windows asks backup programs to leave out (`FilesNotToBackup`).

use std::fs;
use std::ops::Range;
use std::path::Path;

use volsave_core::{PathRule, RangeSet};
use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_MULTI_SZ, REG_SZ, RegCloseKey,
    RegEnumValueW, RegOpenKeyExW,
};
use windows::core::{PWSTR, w};

use crate::VolumeInfo;
use crate::allocation::file_extents;
use crate::util::from_multi_wide;

/// Deepest folder level searched below a rule's folder.
const MAX_DEPTH: usize = 64;

/// Entries whose files may be stored as zeros: after a restore they come
/// back with zero content, which only harms files nobody relies on.
/// Databases and control files (Windows Update, BITS, VSS catalogs, MSDTC
/// log, BitLocker) would be damaged, so they are saved unless the user asks
/// for the whole list.
const SAFE_TO_ZERO: [&str; 7] = [
    "Temporary Files",
    "Kernel Dumps",
    "WER",
    "ETW",
    "Memory Page File",
    "Power Management",
    "VSS Default Provider",
];

/// Which `FilesNotToBackup` entries a backup applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionMode {
    /// Only entries whose files are harmless as zeros (the default).
    Safe,
    /// Every entry.
    All,
    /// None; save everything.
    None,
}

impl ExclusionMode {
    fn applies(self, name: &str) -> bool {
        match self {
            ExclusionMode::Safe => SAFE_TO_ZERO
                .iter()
                .any(|safe| safe.eq_ignore_ascii_case(name)),
            ExclusionMode::All => true,
            ExclusionMode::None => false,
        }
    }
}

/// One entry of the registry list with what it matched on a volume.
#[derive(Debug, Clone)]
pub struct ExcludedEntry {
    /// Name of the registry value, e.g. "Kernel Dumps".
    pub name: String,
    /// The entry as written in the registry.
    pub entry: String,
    pub files: usize,
    pub bytes: u64,
    /// Whether the backup leaves these files out in the chosen mode.
    pub applied: bool,
}

/// All entries of `FilesNotToBackup` as (value name, entry) pairs.
pub fn files_not_to_backup() -> Vec<(String, String)> {
    let mut key = HKEY::default();
    // SAFETY: `key` receives an open handle that is closed below.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!(r"SYSTEM\CurrentControlSet\Control\BackupRestore\FilesNotToBackup"),
            None,
            KEY_READ,
            &mut key,
        )
    };
    if opened.is_err() {
        return Vec::new();
    }

    let mut entries = Vec::new();
    let mut name = vec![0u16; 16_384];
    let mut data = vec![0u8; 64 << 10];
    let mut index = 0;
    loop {
        let mut name_len = name.len() as u32;
        let mut data_len = data.len() as u32;
        let mut kind = 0u32;
        // SAFETY: buffers and lengths describe live local allocations.
        let status = unsafe {
            RegEnumValueW(
                key,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            )
        };
        if status == ERROR_MORE_DATA {
            data.resize(data.len() * 4, 0);
            continue;
        }
        if status == ERROR_NO_MORE_ITEMS || status.is_err() {
            break;
        }
        index += 1;
        if ![REG_MULTI_SZ.0, REG_SZ.0, REG_EXPAND_SZ.0].contains(&kind) {
            continue;
        }
        let value_name = String::from_utf16_lossy(&name[..name_len as usize]);
        let text: Vec<u16> = data[..data_len as usize]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .chain([0, 0])
            .collect();
        for entry in from_multi_wide(&text) {
            entries.push((value_name.clone(), entry));
        }
    }
    // SAFETY: closing the handle opened above.
    let _ = unsafe { RegCloseKey(key) };
    entries
}

/// Byte ranges of the files under `root` (the volume root or a snapshot of
/// it, ending in `\`) that `FilesNotToBackup` excludes in `mode`, with a
/// report on every entry that matches files, applied or not.
pub fn excluded_files(
    root: &str,
    volume: &VolumeInfo,
    mode: ExclusionMode,
) -> (RangeSet, Vec<ExcludedEntry>) {
    let profiles = user_profiles();
    let mut all = Vec::new();
    let mut report = Vec::new();
    for (name, entry) in files_not_to_backup() {
        let rules = PathRule::parse(&entry, &volume.mount_points, &profiles, |var| {
            std::env::var(var).ok()
        });
        let mut found = Found::default();
        for rule in &rules {
            let folder = format!("{root}{}", rule.dir);
            collect(Path::new(&folder), rule, volume.cluster_size, 0, &mut found);
        }
        if found.files > 0 {
            let ranges = RangeSet::from_ranges(found.ranges);
            let applied = mode.applies(&name);
            if applied {
                all.extend(ranges.ranges().iter().cloned());
            }
            report.push(ExcludedEntry {
                name,
                entry,
                files: found.files,
                bytes: ranges.total_len(),
                applied,
            });
        }
    }
    (RangeSet::from_ranges(all), report)
}

#[derive(Default)]
struct Found {
    ranges: Vec<Range<u64>>,
    files: usize,
}

/// Collects the files in `folder` matching `rule`. Links and junctions are
/// not followed; unreadable folders and files are skipped.
fn collect(folder: &Path, rule: &PathRule, cluster_size: u32, depth: usize, found: &mut Found) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if kind.is_dir() {
            if rule.is_literal() && rule.matches(&name) {
                // A rule naming a folder covers everything inside it.
                let everything = PathRule {
                    dir: String::new(),
                    pattern: "*".into(),
                    recursive: true,
                };
                collect(&path, &everything, cluster_size, depth + 1, found);
            } else if rule.recursive && depth < MAX_DEPTH {
                collect(&path, rule, cluster_size, depth + 1, found);
            }
        } else if rule.matches(&name)
            && let Ok(extents) = file_extents(&path.to_string_lossy(), cluster_size, 0)
        {
            found.files += 1;
            found.ranges.extend(extents.ranges().iter().cloned());
        }
    }
}

/// Profile folders of all local users, for `$UserProfile$`.
fn user_profiles() -> Vec<String> {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let users = format!("{drive}\\Users");
    let Ok(entries) = fs::read_dir(&users) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        })
        .map(|entry| format!("{users}\\{}", entry.file_name().to_string_lossy()))
        .collect()
}
