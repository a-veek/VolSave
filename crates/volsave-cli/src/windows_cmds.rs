//! Commands that need Windows: listing, backing up and restoring.

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use volsave_ops::{BackupRequest, ExclusionMode, plan_restore};
use volsave_win::{
    DiskLayout, VolumeInfo, enable_backup_privileges, excluded_files, list_disks, list_volumes,
    resolve_volume,
};

use crate::console::Console;
use crate::{CANCEL, ExclusionChoice, human};

pub(crate) fn list() -> Result<()> {
    println!("Volumes");
    println!(
        "  {:<12} {:<16} {:<6} {:>11} {:>11}  Disk",
        "Name", "Label", "FS", "Size", "Free"
    );
    for volume in list_volumes()? {
        let disks = volume
            .extents
            .iter()
            .map(|e| format!("HD{}@{}", e.disk_number, human(e.offset)))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {:<12} {:<16} {:<6} {:>11} {:>11}  {}",
            short_name(&volume),
            volume.label.chars().take(16).collect::<String>(),
            volume.filesystem,
            human(volume.size),
            human(volume.free),
            disks
        );
        if volume.mount_points.is_empty() {
            println!("               {}", volume.guid_path);
        }
    }

    println!("\nDisks");
    for disk in list_disks() {
        let style = match &disk.layout {
            Some(DiskLayout::Gpt { .. }) => "GPT",
            Some(DiskLayout::Mbr { .. }) => "MBR",
            Some(DiskLayout::Raw) => "raw",
            None => "?",
        };
        println!(
            "  HD{:<3} {:>11}  {:<4} {}{}",
            disk.number,
            human(disk.size),
            style,
            disk.model,
            if disk.removable { " (removable)" } else { "" }
        );
        match &disk.layout {
            Some(layout) => {
                for part in layout.partitions() {
                    println!(
                        "         #{:<2} {:>11} at {:>11}  {}",
                        part.number,
                        human(part.length),
                        human(part.offset),
                        part.kind.description()
                    );
                }
            }
            None => println!("         (partition table needs administrator rights)"),
        }
    }
    Ok(())
}

fn short_name(volume: &VolumeInfo) -> String {
    match volume.mount_points.first() {
        Some(mount) => mount.clone(),
        None => format!("{}…", &volume.guid_path[..volume.guid_path.len().min(18)]),
    }
}

pub(crate) fn backup(request: BackupRequest) -> Result<()> {
    volsave_ops::backup(&request, &CANCEL, &mut Console::default())
}

pub(crate) fn restore(
    image: &Path,
    target: &str,
    base: Option<&Path>,
    partition: Option<u32>,
    yes: bool,
    verify: bool,
) -> Result<()> {
    let mut console = Console::default();
    let plan = plan_restore(image, target, base, partition, &mut console)?;
    if !yes
        && !confirm(&format!(
            "All data on {} will be overwritten. Type YES to continue: ",
            plan.target_name()
        ))?
    {
        bail!("restore cancelled");
    }
    plan.run(verify, &CANCEL, &mut console)
}

fn confirm(question: &str) -> Result<bool> {
    eprint!("{question}");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(answer.trim() == "YES")
}

impl From<ExclusionChoice> for ExclusionMode {
    fn from(choice: ExclusionChoice) -> Self {
        match choice {
            ExclusionChoice::Safe => ExclusionMode::Safe,
            ExclusionChoice::All => ExclusionMode::All,
            ExclusionChoice::None => ExclusionMode::None,
        }
    }
}

/// Lists which `FilesNotToBackup` entries match files on a volume right now
/// and whether a default backup leaves them out.
pub(crate) fn exclusions(target: &str) -> Result<()> {
    if !enable_backup_privileges() {
        eprintln!(
            "note: not running as administrator; protected folders such as System Volume Information are not searched"
        );
    }
    let volume = resolve_volume(target).with_context(|| format!("volume {target} not found"))?;
    if !volume.supports_vss() {
        println!(
            "{} uses {}; FilesNotToBackup is only applied to NTFS and ReFS volumes",
            volume.display_name(),
            volume.filesystem
        );
        return Ok(());
    }
    let (skipped, entries) = excluded_files(&volume.guid_path, &volume, ExclusionMode::Safe);
    if entries.is_empty() {
        println!(
            "Nothing on {} matches FilesNotToBackup",
            volume.display_name()
        );
        return Ok(());
    }
    println!(
        "{:<8} {:>11} {:>7}  {:<26} Entry",
        "Backup", "Size", "Files", "Name"
    );
    for entry in &entries {
        println!(
            "{:<8} {:>11} {:>7}  {:<26} {}",
            if entry.applied { "skipped" } else { "saved" },
            human(entry.bytes),
            entry.files,
            entry.name,
            entry.entry
        );
    }
    println!(
        "\nA default backup (--files-not-to-backup safe) skips {}; skipped files come back with \
         zero content after a restore. Entries marked \"saved\" hold databases or control files \
         and are only skipped with --files-not-to-backup all.",
        human(skipped.total_len())
    );
    Ok(())
}
