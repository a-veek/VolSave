use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use volsave_core::{
    AlignedBuf, BlockSource, ImageView, PatchedSource, RangeSet, backup as run_backup,
    fix_ntfs_hidden_sectors, restore as run_restore, verify_target,
};
use volsave_format::{
    DiskSummary, FileHeader, HashList, IMAGE_EXTENSION, ImageMetadata, ImageWriter, SourceInfo,
    SourceKind, part_path,
};
use volsave_win::{
    DiskLayout, ExclusionMode, RawDevice, SnapshotSet, VolumeInfo, enable_backup_privileges,
    excluded_files, list_disks, list_volumes, resolve_volume, system_file_extents, used_ranges,
};

use crate::disk_cmds::{backup_disk, parse_disk, restore_disk};
use crate::images::{hash_path, open_view, read_hash_list, write_hash_list};
use crate::{CANCEL, ExclusionChoice, ProgressPrinter, WriteOptions, human, now_unix, tool_name};

pub(crate) struct BackupArgs {
    pub source: String,
    pub image: PathBuf,
    pub diff: Option<PathBuf>,
    pub full_if_hash_missing: bool,
    pub hash: Option<PathBuf>,
    pub no_hash: bool,
    pub common: WriteOptions,
    pub no_vss: bool,
    pub keep_system_files: bool,
    pub files_not_to_backup: ExclusionChoice,
    pub comment: Option<String>,
    pub all_sectors: bool,
}

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

/// The hash list for a differential backup, or `None` for a full backup.
pub(crate) fn load_base(args: &BackupArgs) -> Result<Option<HashList>> {
    let Some(path) = &args.diff else {
        return Ok(None);
    };
    if args.full_if_hash_missing && !path.exists() {
        note!(
            "note: {} does not exist; making a full backup",
            path.display()
        );
        return Ok(None);
    }
    read_hash_list(path).map(Some)
}

pub(crate) fn backup(args: BackupArgs) -> Result<()> {
    if let Some(number) = parse_disk(&args.source) {
        return backup_disk(args, number);
    }
    anyhow::ensure!(
        !args.all_sectors,
        "--all-sectors is for disks (HD0, HD1, ...); volumes are always read by their used blocks"
    );
    enable_backup_privileges();
    let volume = resolve_volume(&args.source)
        .with_context(|| format!("volume {} not found", args.source))?;
    let base = load_base(&args)?;
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

    let use_vss = !args.no_vss && volume.supports_vss();
    if !args.no_vss && !use_vss {
        note!(
            "note: {} uses {}, which VSS cannot snapshot; reading the live volume",
            volume.display_name(),
            volume.filesystem
        );
    }
    let snapshot = if use_vss {
        note!("Creating VSS snapshot of {} ...", volume.display_name());
        let set = SnapshotSet::create(&[&volume.guid_path])?;
        for warning in set.warnings() {
            warn!("{warning}");
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
    let size = volsave_core::BlockSource::size(&device);
    let (mut used, clusters_end) =
        match used_ranges(device.file(), size, volume.cluster_size, volume.sector_size) {
            Ok(allocation) => (allocation.used, allocation.clusters_end),
            Err(error) => {
                warn!("cannot read the allocation bitmap ({error}); saving every sector");
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
    if !args.keep_system_files {
        let skipped = system_file_extents(&root, volume.cluster_size);
        if !skipped.is_empty() {
            note!(
                "Skipping page/swap/hibernation files: {}",
                human(skipped.total_len())
            );
        }
        used = used.subtract(&skipped);
    }
    // File system metadata stays; only the content of the listed files is left out.
    if args.files_not_to_backup != ExclusionChoice::None && volume.supports_vss() {
        let (excluded, entries) = excluded_files(&root, &volume, mode(args.files_not_to_backup));
        let applied = entries.iter().filter(|entry| entry.applied).count();
        // Report only what comes on top of the page/swap/hibernation files.
        let before = used.total_len();
        used = used.subtract(&excluded);
        if applied > 0 {
            note!(
                "Skipping files listed in FilesNotToBackup: {} more ({applied} entries; see `volsave exclusions`)",
                human(before - used.total_len())
            );
        }
    }

    write_image(
        &args,
        ImageJob {
            source,
            used,
            info: source_info(&volume, size),
            disk: None,
            base,
            name: volume.display_name().to_string(),
        },
    )?;
    if let Some(set) = snapshot {
        set.complete()?;
    }
    Ok(())
}

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
pub(crate) fn write_image(args: &BackupArgs, job: ImageJob<'_>) -> Result<()> {
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
        comment: args.comment.clone(),
        source: info,
        base_image_name: None,
        disk,
    });
    if let (Some(base), Some(hash_file)) = (&base, &args.diff) {
        header.base_image_id = Some(base.image_id);
        // The full image usually sits next to its hash list.
        let full_image = hash_file.with_extension(IMAGE_EXTENSION);
        header.metadata.base_image_name = full_image
            .file_name()
            .filter(|_| full_image.exists())
            .map(|name| name.to_string_lossy().into_owned());
    }

    let writer = ImageWriter::new(args.common.create_sink(&args.image)?, header)?;
    note!(
        "{} backup of {} ({} of {} selected) to {}",
        if base.is_some() {
            "Differential"
        } else {
            "Full"
        },
        name,
        human(used.total_len()),
        human(size),
        args.image.display()
    );
    let mut printer = ProgressPrinter::new("Backup");
    let result = run_backup(
        source,
        &used,
        writer,
        base.as_ref(),
        &args.common.backup_options(),
        &CANCEL,
        |p| printer.update(p),
    )
    .map_err(anyhow::Error::from)
    .and_then(|output| {
        let parts = output.sink.finish()?;
        Ok((output.stats, output.hashes, parts))
    });
    printer.finish();
    let (stats, hashes, parts) = match result {
        Ok(done) => done,
        Err(error) => {
            remove_image(&args.image);
            return Err(error);
        }
    };

    // A differential backup keeps the full image's hash list; its own list
    // is only written on request. A full backup made because the --diff list
    // was missing writes its list there, so the next run finds it.
    let hash_file = match (&args.hash, &base, &args.diff) {
        _ if args.no_hash => None,
        (Some(path), _, _) => Some(path.clone()),
        (None, Some(_), _) => None,
        (None, None, Some(diff_list)) => Some(diff_list.clone()),
        (None, None, None) => Some(hash_path(&args.image)),
    };
    if let Some(path) = &hash_file {
        write_hash_list(path, &hashes)?;
    }

    let seconds = printer.elapsed().as_secs();
    // Blocks are whole block-size pieces, so they also hold the free space
    // between used ranges; verify and restore count that size. For a
    // differential image it is what changed.
    say!(
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

pub(crate) fn restore(
    image: &Path,
    target: &str,
    base: Option<&Path>,
    partition: Option<u32>,
    yes: bool,
    verify: bool,
) -> Result<()> {
    let opened = open_view(image, base)?;
    let mut image_files = vec![image.to_path_buf()];
    image_files.extend(opened.base_path.clone());
    match (parse_disk(target), opened.is_disk(), partition) {
        (Some(number), true, None) => {
            return restore_disk(opened, number, &image_files, yes, verify);
        }
        (Some(_), true, Some(_)) => {
            bail!("--partition restores one partition onto a volume (e.g. X:), not onto a disk")
        }
        (Some(_), false, _) => bail!("this is a volume image; restore it onto a volume such as X:"),
        (None, true, None) => bail!(
            "this is a disk image; restore it onto a whole disk (HD1, ... see `volsave list`) \
             or pick one partition with --partition N"
        ),
        (None, false, Some(_)) => bail!("--partition needs a disk image"),
        _ => {}
    }

    let volume = resolve_volume(target).with_context(|| format!("volume {target} not found"))?;
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()) + "\\";
    if volume
        .mount_points
        .iter()
        .any(|m| m.eq_ignore_ascii_case(&system_drive))
    {
        bail!(
            "{} is the running Windows volume; restore it from WinPE instead",
            volume.display_name()
        );
    }

    for file in &image_files {
        let root = std::path::absolute(file)?
            .to_string_lossy()
            .chars()
            .take(3)
            .collect::<String>();
        if volume
            .mount_points
            .iter()
            .any(|m| m.eq_ignore_ascii_case(&root))
        {
            bail!(
                "{} is stored on the target volume {}",
                file.display(),
                volume.display_name()
            );
        }
    }

    let mut source = opened.header.metadata.source.clone();
    let base_path = opened.base_path.clone();
    let summary = opened.header.metadata.disk.clone();
    let mut view = match partition {
        Some(number) => {
            let (view, p) = opened.into_partition(number)?;
            let listed = summary
                .iter()
                .flat_map(|s| &s.partitions)
                .find(|s| s.number == number);
            source.size_bytes = p.length;
            source.filesystem = listed.and_then(|s| s.filesystem.clone());
            source.label = listed
                .and_then(|s| s.label.clone())
                .or_else(|| Some(format!("partition {number} ({})", p.type_name())));
            view
        }
        None => opened.view,
    };
    say!(
        "Image:  {} {} ({}), {}",
        source.filesystem.as_deref().unwrap_or("?"),
        source.label.as_deref().unwrap_or(""),
        human(source.size_bytes),
        source.hostname.as_deref().unwrap_or("unknown computer")
    );
    if let Some(base_path) = &base_path {
        say!("        differential, full image {}", base_path.display());
    }
    say!(
        "Target: {} {} \"{}\" ({})",
        volume.display_name(),
        volume.filesystem,
        volume.label,
        human(volume.size)
    );
    if !yes
        && !confirm(&format!(
            "All data on {} will be overwritten. Type YES to continue: ",
            volume.display_name()
        ))?
    {
        bail!("restore cancelled");
    }

    let mut device = RawDevice::open_write(&volume.device_path(), volume.sector_size)
        .with_context(|| {
            format!(
                "cannot open {} (run as administrator)",
                volume.display_name()
            )
        })?;
    device
        .lock_and_dismount()
        .context("the target volume is in use; close all programs and windows using it")?;
    let mut printer = ProgressPrinter::new("Restore");
    let stats = run_restore(view.as_mut(), &mut device, &CANCEL, |p| printer.update(p))?;
    printer.finish();
    say!("Done: {} written", human(stats.bytes_written));
    // Before the boot sector fix, which changes what the image holds.
    if verify {
        verify_restored(view.as_mut(), &device, &stats.copied)?;
    }
    // The boot code finds BOOTMGR through the start sector stored in the boot
    // sector, which is wrong when the partition sits elsewhere than the source.
    if let [extent] = volume.extents.as_slice() {
        match fix_ntfs_hidden_sectors(&mut device, extent.offset) {
            Ok(Some(fix)) => say!(
                "Boot sector: start sector changed from {} to {}{}",
                fix.old,
                fix.new,
                if fix.backup_fixed {
                    ""
                } else {
                    " (backup boot sector not found)"
                }
            ),
            Ok(None) => {}
            Err(e) => warn!("could not update the boot sector: {e}"),
        }
    }
    if volume.size > source.size_bytes {
        say!(
            "note: the volume is {} larger than the image; extend the file system in Disk Management to use it",
            human(volume.size - source.size_bytes)
        );
    }
    Ok(())
}

/// Reads the restored ranges back from the target and compares them with
/// the image.
pub(crate) fn verify_restored(
    view: &mut dyn ImageView,
    target: &RawDevice,
    copied: &RangeSet,
) -> Result<()> {
    let mut printer = ProgressPrinter::new("Verifying target");
    let result = verify_target(view, target, copied, &CANCEL, |p| printer.update(p));
    printer.finish();
    let checked = result.context("verification of the restored data failed")?;
    say!(
        "Verified: {} read back, identical to the image",
        human(checked)
    );
    Ok(())
}

pub(crate) fn confirm(question: &str) -> Result<bool> {
    eprint!("{question}");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(answer.trim() == "YES")
}

pub(crate) fn mode(choice: ExclusionChoice) -> ExclusionMode {
    match choice {
        ExclusionChoice::Safe => ExclusionMode::Safe,
        ExclusionChoice::All => ExclusionMode::All,
        ExclusionChoice::None => ExclusionMode::None,
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
