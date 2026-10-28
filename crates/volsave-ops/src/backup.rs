//! Backing up a volume or, via [`crate::disk`], a whole disk.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use volsave_core::{AlignedBuf, BlockSource, PatchedSource, RangeSet, backup as run_backup};
use volsave_format::{
    DiskSummary, FileHeader, HashList, IMAGE_EXTENSION, ImageMetadata, ImageWriter, SourceInfo,
    SourceKind, part_path,
};
use volsave_win::{
    ExclusionMode, RawDevice, SnapshotSet, VolumeInfo, enable_backup_privileges, excluded_files,
    is_winpe, resolve_volume, system_file_extents, used_ranges,
};

use crate::disk::{backup_disk, parse_disk};
use crate::images::{hash_path, read_hash_list, write_hash_list};
use crate::{Reporter, Stage, WriteOptions, human, now_unix, tool_name};

/// What to back up where, and how.
#[derive(Debug, Clone)]
pub struct BackupRequest {
    /// A volume (`C:`, a folder mount point, `\\?\Volume{GUID}\`) or a whole
    /// disk (`HD0`, `PhysicalDrive0`, ...).
    pub source: String,
    pub image: PathBuf,
    /// Hash list of a full image: makes a differential image against it.
    pub diff: Option<PathBuf>,
    /// With `diff`: make a full backup if the hash list does not exist.
    pub full_if_hash_missing: bool,
    /// Where to write the hash list (default for full images: next to the image).
    pub hash: Option<PathBuf>,
    pub no_hash: bool,
    pub write: WriteOptions,
    /// Read the live volume instead of a VSS snapshot.
    pub no_vss: bool,
    /// Also save pagefile.sys, swapfile.sys and hiberfil.sys.
    pub keep_system_files: bool,
    /// Which entries of the Windows list FilesNotToBackup to leave out.
    pub files_not_to_backup: ExclusionMode,
    pub comment: Option<String>,
    /// Disks only: copy every sector instead of the used blocks.
    pub all_sectors: bool,
}

impl BackupRequest {
    /// A full backup of `source` into `image` with default settings.
    pub fn new(source: impl Into<String>, image: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            image: image.into(),
            diff: None,
            full_if_hash_missing: false,
            hash: None,
            no_hash: false,
            write: WriteOptions::default(),
            no_vss: false,
            keep_system_files: false,
            files_not_to_backup: ExclusionMode::Safe,
            comment: None,
            all_sectors: false,
        }
    }
}

/// Backs up a volume or a whole disk; a partly written image is deleted.
pub fn backup(request: &BackupRequest, cancel: &AtomicBool, rep: &mut dyn Reporter) -> Result<()> {
    // VSS does not work in Windows PE, and it is not needed there: the
    // installed Windows is not running and does not change its volumes.
    let without_vss;
    let request = if !request.no_vss && is_winpe() {
        note!(
            rep,
            "Windows PE: the installed Windows is not running; volumes are read directly without VSS snapshots"
        );
        without_vss = BackupRequest {
            no_vss: true,
            ..request.clone()
        };
        &without_vss
    } else {
        request
    };
    if let Some(number) = parse_disk(&request.source) {
        return backup_disk(request, number, cancel, rep);
    }
    anyhow::ensure!(
        !request.all_sectors,
        "--all-sectors is for disks (HD0, HD1, ...); volumes are always read by their used blocks"
    );
    enable_backup_privileges();
    let volume = resolve_volume(&request.source)
        .with_context(|| format!("volume {} not found", request.source))?;
    let base = load_base(request, rep)?;
    if let Some(base) = &base
        && base.volume_size != volume.size
    {
        bail!(
            "{} has changed size since the full backup ({} now, {} then); make a new full backup",
            volume.display_name(),
            human(volume.size),
            human(base.volume_size)
        );
    }

    let use_vss = !request.no_vss && volume.supports_vss();
    if !request.no_vss && !use_vss {
        note!(
            rep,
            "note: {} uses {}, which VSS cannot snapshot; reading the live volume",
            volume.display_name(),
            volume.filesystem
        );
    }
    let snapshot = if use_vss {
        note!(
            rep,
            "Creating VSS snapshot of {} ...",
            volume.display_name()
        );
        let set = SnapshotSet::create(&[&volume.guid_path]).context(VSS_HINT)?;
        for warning in set.warnings() {
            warn!(rep, "{warning}");
        }
        Some(set)
    } else {
        None
    };
    let (device_path, root) = match &snapshot {
        Some(set) => {
            let shot = set
                .snapshot_of(&volume.guid_path)
                .or_else(|| set.snapshots().first())
                .context("VSS returned no snapshot")?;
            (shot.device_object.clone(), shot.root())
        }
        None => (volume.device_path(), volume.guid_path.clone()),
    };

    let device = RawDevice::open_read(&device_path, volume.sector_size)
        .with_context(|| format!("cannot open {device_path} (run as administrator)"))?;
    let size = BlockSource::size(&device);
    let (mut used, clusters_end) =
        match used_ranges(device.file(), size, volume.cluster_size, volume.sector_size) {
            Ok(allocation) => (allocation.used, allocation.clusters_end),
            Err(error) => {
                warn!(
                    rep,
                    "cannot read the allocation bitmap ({error}); saving every sector"
                );
                (RangeSet::full(size), size)
            }
        };

    // Snapshots may not expose the sectors after the last cluster (the NTFS
    // backup boot sector). Those do not change while Windows runs, so they
    // are read from the live volume instead.
    let live = match &snapshot {
        Some(_)
            if clusters_end < size && !last_sector_readable(&device, size, volume.sector_size) =>
        {
            Some(
                RawDevice::open_read(&volume.device_path(), volume.sector_size)
                    .context("cannot open the live volume to read its last sectors")?,
            )
        }
        _ => None,
    };
    if live.is_some() {
        note!(
            rep,
            "note: the snapshot hides the last sectors of the volume; reading them from the live volume"
        );
    }
    let patched = live
        .as_ref()
        .map(|live| PatchedSource::new(&device, live, clusters_end));
    let source: &dyn BlockSource = match &patched {
        Some(patched) => patched,
        None => &device,
    };
    if !request.keep_system_files {
        let skipped = system_file_extents(&root, volume.cluster_size);
        if !skipped.is_empty() {
            note!(
                rep,
                "Skipping page/swap/hibernation files: {}",
                human(skipped.total_len())
            );
        }
        used = used.subtract(&skipped);
    }
    // File system metadata stays; only the content of the listed files is left out.
    if request.files_not_to_backup != ExclusionMode::None && volume.supports_vss() {
        let (excluded, entries) = excluded_files(&root, &volume, request.files_not_to_backup);
        let applied = entries.iter().filter(|entry| entry.applied).count();
        // Report only what comes on top of the page/swap/hibernation files.
        let before = used.total_len();
        used = used.subtract(&excluded);
        if applied > 0 {
            note!(
                rep,
                "Skipping files listed in FilesNotToBackup: {} more ({applied} entries; see `volsave exclusions`)",
                human(before - used.total_len())
            );
        }
    }

    write_image(
        request,
        ImageJob {
            source,
            used,
            info: source_info(&volume, size),
            disk: None,
            base,
            name: volume.display_name().to_string(),
        },
        cancel,
        rep,
    )?;
    if let Some(set) = snapshot {
        set.complete()?;
    }
    Ok(())
}

/// The hash list for a differential backup, or `None` for a full backup.
pub(crate) fn load_base(
    request: &BackupRequest,
    rep: &mut dyn Reporter,
) -> Result<Option<HashList>> {
    let Some(path) = &request.diff else {
        return Ok(None);
    };
    if request.full_if_hash_missing && !path.exists() {
        note!(
            rep,
            "note: {} does not exist; making a full backup",
            path.display()
        );
        return Ok(None);
    }
    // A list of a differential image would let the next backup build on it,
    // and such a chain cannot be restored: every --diff compares with the
    // full image.
    if request.hash.is_some() {
        bail!(
            "--hash is only for full backups; a differential image always builds on the hash list of its full image"
        );
    }
    read_hash_list(path).map(Some)
}

/// Added to VSS errors.
pub(crate) const VSS_HINT: &str = "VSS snapshot failed; if Windows on this disk is not running (rescue system) or VSS does not work, back up with --no-vss";

/// What [`write_image`] saves.
pub(crate) struct ImageJob<'a> {
    pub source: &'a dyn BlockSource,
    pub used: RangeSet,
    pub info: SourceInfo,
    pub disk: Option<DiskSummary>,
    /// Hash list of the full image for a differential backup.
    pub base: Option<HashList>,
    /// Volume or disk name for messages.
    pub name: String,
}

/// Writes the image and its hash list; deletes a partly written image.
pub(crate) fn write_image(
    request: &BackupRequest,
    job: ImageJob<'_>,
    cancel: &AtomicBool,
    rep: &mut dyn Reporter,
) -> Result<()> {
    let ImageJob {
        source,
        used,
        info,
        disk,
        base,
        name,
    } = job;
    let size = info.size_bytes;
    let mut header = FileHeader::new(ImageMetadata {
        tool: tool_name(),
        created_unix: now_unix(),
        comment: request.comment.clone(),
        source: info,
        base_image_name: None,
        disk,
    });
    if let (Some(base), Some(hash_file)) = (&base, &request.diff) {
        header.base_image_id = Some(base.image_id);
        // The full image usually sits next to its hash list.
        let full_image = hash_file.with_extension(IMAGE_EXTENSION);
        header.metadata.base_image_name = full_image
            .file_name()
            .filter(|_| full_image.exists())
            .map(|name| name.to_string_lossy().into_owned());
    }

    let writer = ImageWriter::new(request.write.create_sink(&request.image)?, header)?;
    note!(
        rep,
        "{} backup of {} ({} of {} selected) to {}",
        if base.is_some() {
            "Differential"
        } else {
            "Full"
        },
        name,
        human(used.total_len()),
        human(size),
        request.image.display()
    );
    let started = Instant::now();
    let result = run_backup(
        source,
        &used,
        writer,
        base.as_ref(),
        &request.write.backup_options(),
        cancel,
        |p| rep.progress(Stage::Backup, p),
    )
    .map_err(anyhow::Error::from)
    .and_then(|output| {
        let parts = output.sink.finish()?;
        Ok((output.stats, output.hashes, parts))
    });
    rep.stage_done(Stage::Backup);
    let (stats, hashes, parts) = match result {
        Ok(done) => done,
        Err(error) => {
            remove_image(&request.image);
            return Err(error);
        }
    };

    // A differential backup keeps the full image's hash list and gets none of
    // its own. A full backup made because the --diff list was missing writes
    // its list there, so the next run finds it.
    let hash_file = match (&request.hash, &base, &request.diff) {
        _ if request.no_hash => None,
        (_, Some(_), _) => None,
        (Some(path), None, _) => Some(path.clone()),
        (None, None, Some(diff_list)) => Some(diff_list.clone()),
        (None, None, None) => Some(hash_path(&request.image)),
    };
    if let Some(path) = &hash_file {
        write_hash_list(path, &hashes)?;
    }

    let seconds = started.elapsed().as_secs();
    // Blocks are whole block-size pieces, so they also hold the free space
    // between used ranges; verify and restore count that size. For a
    // differential image it is what changed.
    say!(
        rep,
        "Done: {} read, {} {}, image {} ({:.0}%) in {} file(s), {}:{:02} min",
        human(stats.bytes_read),
        human(stats.bytes_stored),
        if base.is_some() {
            "changed"
        } else {
            "in blocks"
        },
        human(stats.bytes_written),
        stats.bytes_written as f64 * 100.0 / stats.bytes_stored.max(1) as f64,
        parts,
        seconds / 60,
        seconds % 60
    );
    if let Some(path) = hash_file {
        say!(
            rep,
            "Hash list: {} (use with --diff for differential backups)",
            path.display()
        );
    }
    Ok(())
}

fn last_sector_readable(device: &RawDevice, size: u64, sector_size: u32) -> bool {
    let mut sector = AlignedBuf::zeroed(sector_size as usize);
    device
        .read_at(size - u64::from(sector_size), &mut sector)
        .is_ok()
}

/// Deletes a partly written image with all its parts.
fn remove_image(path: &Path) {
    let mut part = 1;
    while std::fs::remove_file(part_path(path, part)).is_ok() {
        part += 1;
    }
}

fn source_info(volume: &VolumeInfo, size: u64) -> SourceInfo {
    SourceInfo {
        kind: SourceKind::Volume,
        device: volume.guid_path.clone(),
        size_bytes: size,
        sector_size: volume.sector_size,
        filesystem: Some(volume.filesystem.clone()),
        label: Some(volume.label.clone()),
        disk_number: volume.extents.first().map(|e| e.disk_number),
        partition_number: None,
        partition_offset: volume.extents.first().map(|e| e.offset),
        hostname: std::env::var("COMPUTERNAME").ok(),
    }
}
