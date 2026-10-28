//! Backing up and restoring whole disks: partition table, boot code and
//! every partition.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use volsave_core::{
    AlignedBuf, BlockSource, BootSectorKind, DiskLayout, LayoutError, MappedSource, Partition,
    RangeSet, identify_boot_sector, restore_disk as restore_disk_data,
};
use volsave_format::{DiskSummary, PartitionSummary, SourceInfo, SourceKind};
use volsave_win::{
    DiskInfo, DiskLayout as WinDiskLayout, ExclusionMode, RawDevice, SnapshotSet, VolumeInfo,
    disk_attributes, enable_backup_privileges, excluded_files, list_disks, list_volumes,
    open_disk_for_writing, refresh_disk, set_disk_attributes, system_file_extents, used_ranges,
};

use crate::backup::{BackupRequest, ImageJob, VSS_HINT, load_base, write_image};
use crate::images::OpenedImage;
use crate::restore::verify_restored;
use crate::{Reporter, Stage, human};

/// Unpartitioned areas up to this size are saved, larger ones skipped.
/// Alignment gaps are 1 MiB; anything a boot loader hides there is small.
const MAX_SAVED_GAP: u64 = 64 << 20;

/// `HD1`, `disk1`, `PhysicalDrive1` or `\\.\PhysicalDrive1` -> 1.
pub fn parse_disk(spec: &str) -> Option<u32> {
    let spec = spec.trim().to_ascii_lowercase();
    let spec = spec.strip_prefix(r"\\.\").unwrap_or(&spec);
    ["hd", "disk", "physicaldrive"]
        .iter()
        .find_map(|prefix| spec.strip_prefix(prefix))
        .and_then(|n| n.trim().parse().ok())
}

pub fn find_disk(number: u32) -> Result<DiskInfo> {
    list_disks()
        .into_iter()
        .find(|d| d.number == number)
        .with_context(|| format!("disk HD{number} not found (see `volsave list`)"))
}

/// Volumes with at least one extent on the disk.
pub fn volumes_on(number: u32) -> Result<Vec<VolumeInfo>> {
    Ok(list_volumes()?
        .into_iter()
        .filter(|v| v.extents.iter().any(|e| e.disk_number == number))
        .collect())
}

/// The volume occupying exactly this partition.
fn volume_of<'a>(volumes: &'a [VolumeInfo], disk: u32, p: &Partition) -> Option<&'a VolumeInfo> {
    volumes.iter().find(
        |v| matches!(v.extents.as_slice(), [e] if e.disk_number == disk && e.offset == p.offset),
    )
}

/// How a partition's content gets into the image.
enum Method<'a> {
    /// Used clusters, read from a VSS snapshot of the volume.
    Snapshot(&'a VolumeInfo),
    /// Used clusters per the live file system, read from the disk.
    Live(&'a VolumeInfo),
    /// Every sector, read from the disk.
    AllSectors(&'static str),
}

pub(crate) fn backup_disk(
    request: &BackupRequest,
    number: u32,
    cancel: &AtomicBool,
    rep: &mut dyn Reporter,
) -> Result<()> {
    enable_backup_privileges();
    let disk = find_disk(number)?;
    let device = RawDevice::open_read(&disk.path, disk.sector_size)
        .with_context(|| format!("cannot open {} (run as administrator)", disk.path))?;
    let size = BlockSource::size(&device);
    let base = load_base(request, rep)?;
    if let Some(base) = &base
        && base.volume_size != size
    {
        bail!(
            "HD{number} has changed size since the full backup ({} now, {} then); make a new full backup",
            human(size),
            human(base.volume_size)
        );
    }
    let name = format!("HD{number} ({}, {})", disk.model, human(size));

    let layout = match DiskLayout::read(
        &mut |offset, buf| device.read_at(offset, buf),
        size,
        disk.sector_size,
    ) {
        Ok(layout) => Some(layout),
        Err(error) if request.all_sectors => {
            note!(rep, "note: {error}; copying every sector");
            None
        }
        Err(LayoutError::NoPartitionTable) => {
            bail!("HD{number} has no partition table; use --all-sectors to copy it anyway")
        }
        Err(error) => bail!("HD{number}: {error}; use --all-sectors to copy it sector by sector"),
    };
    let layout = match layout {
        Some(layout) if !request.all_sectors => layout,
        layout => {
            // Sector-by-sector copy: no file systems involved, no snapshot.
            let summary = layout
                .as_ref()
                .map(|l| summary(l, &disk, &[], |p| ("all sectors", p.length)));
            note!(
                rep,
                "Copying every sector of HD{number}; partitions in use may change while it runs"
            );
            return write_image(
                request,
                ImageJob {
                    source: &device,
                    used: RangeSet::full(size),
                    info: disk_info(&disk, size),
                    disk: summary,
                    base,
                    name,
                },
                cancel,
                rep,
            );
        }
    };
    if let Some(p) = layout.partitions.iter().find(|p| p.is_pooled()) {
        bail!(
            "partition {} is a {} partition; such disks can only be copied with --all-sectors",
            p.number,
            p.type_name()
        );
    }

    let volumes = volumes_on(number)?;
    if let Some(spanned) = volumes.iter().find(|v| v.extents.len() > 1) {
        bail!(
            "volume {} spans several disks or partitions; use --all-sectors",
            spanned.display_name()
        );
    }

    // Decide per partition how to read it.
    let mut methods = Vec::new();
    for p in &layout.partitions {
        let mut first = AlignedBuf::zeroed(disk.sector_size as usize);
        device
            .read_at(p.offset, &mut first)
            .with_context(|| format!("cannot read partition {}", p.number))?;
        let volume = volume_of(&volumes, number, p);
        let method = match (identify_boot_sector(&first), volume) {
            // Through its volume the data would come out decrypted and no
            // longer match the BitLocker metadata on the disk.
            (BootSectorKind::BitLocker, v) => {
                if let Some(v) = v.filter(|v| !v.mount_points.is_empty()) {
                    warn!(
                        rep,
                        "{} is encrypted with BitLocker and is copied sector by sector without a \
                         snapshot; changes during the backup make the copy inconsistent. Suspend \
                         BitLocker or back up the volume itself for a consistent image",
                        v.display_name()
                    );
                }
                Method::AllSectors("BitLocker, encrypted sectors")
            }
            (_, Some(v)) if v.supports_vss() && !request.no_vss => Method::Snapshot(v),
            (_, Some(v)) if !v.filesystem.is_empty() && v.filesystem != "RAW" => Method::Live(v),
            (_, _) => Method::AllSectors("no file system known to Windows"),
        };
        methods.push(method);
    }

    let snapshot = create_snapshot(&mut methods, number, rep)?;

    // Open every snapshot first; the mapped source borrows them.
    let mut snapshot_devices = Vec::new();
    for (p, method) in layout.partitions.iter().zip(&methods) {
        if let Method::Snapshot(volume) = method {
            let set = snapshot
                .as_ref()
                .expect("snapshot exists for these volumes");
            let shot = set.snapshot_of(&volume.guid_path).with_context(|| {
                format!("VSS returned no snapshot of {}", volume.display_name())
            })?;
            let device = RawDevice::open_read(&shot.device_object, volume.sector_size)
                .with_context(|| format!("cannot open {}", shot.device_object))?;
            snapshot_devices.push((p.number, device, shot.root()));
        }
    }

    let gaps = layout.gaps();
    let (small, large): (Vec<_>, Vec<_>) = gaps
        .ranges()
        .iter()
        .cloned()
        .partition(|gap| gap.end - gap.start <= MAX_SAVED_GAP);
    let skipped_gaps: u64 = large.iter().map(|gap| gap.end - gap.start).sum();
    let mut used = layout.table_ranges().union(&RangeSet::from_ranges(small));
    let mut source = MappedSource::new(&device);
    let mut selected = Vec::new();
    for (p, method) in layout.partitions.iter().zip(&methods) {
        let part = match method {
            Method::Snapshot(volume) => {
                let (_, shot, root) = snapshot_devices
                    .iter()
                    .find(|(n, _, _)| *n == p.number)
                    .expect("opened above");
                let shot_size = BlockSource::size(shot).min(p.length);
                let (part, clusters_end) =
                    volume_ranges(request, volume, shot.file(), shot_size, root, rep);
                // The tail after the last cluster (NTFS backup boot sector)
                // may be hidden by the snapshot; it is read from the disk.
                source.map(p.offset..p.offset + clusters_end, shot);
                let mut part = part;
                part.push(clusters_end..p.length);
                part
            }
            Method::Live(volume) => {
                let live = RawDevice::open_read(&volume.device_path(), volume.sector_size)
                    .with_context(|| format!("cannot open {}", volume.display_name()))?;
                match used_ranges(
                    live.file(),
                    p.length,
                    volume.cluster_size,
                    volume.sector_size,
                ) {
                    Ok(allocation) => allocation.used,
                    Err(error) => {
                        warn!(
                            rep,
                            "cannot read the allocation of {} ({error}); saving every sector",
                            volume.display_name()
                        );
                        RangeSet::full(p.length)
                    }
                }
            }
            Method::AllSectors(_) => RangeSet::full(p.length),
        };
        let part = part.clamp(0..p.length);
        selected.push(part.total_len());
        used = used.union(&RangeSet::from_ranges(
            part.ranges()
                .iter()
                .map(|r| p.offset + r.start..p.offset + r.end),
        ));
    }

    let summary = summary(&layout, &disk, &volumes, |p| {
        let i = layout
            .partitions
            .iter()
            .position(|q| q == p)
            .expect("own partition");
        let method = match &methods[i] {
            Method::Snapshot(_) => "used blocks, VSS snapshot",
            Method::Live(_) => "used blocks, live volume",
            Method::AllSectors(why) => why,
        };
        (method, selected[i])
    });
    print_plan(&summary, rep);
    if skipped_gaps > 0 {
        note!(
            rep,
            "Unpartitioned space not saved: {} (only gaps up to {} are kept)",
            human(skipped_gaps),
            human(MAX_SAVED_GAP)
        );
    }

    write_image(
        request,
        ImageJob {
            source: &source,
            used,
            info: disk_info(&disk, size),
            disk: Some(summary),
            base,
            name,
        },
        cancel,
        rep,
    )?;
    if let Some(set) = snapshot {
        set.complete()?;
    }
    Ok(())
}

/// Volumes up to this size without a drive letter (System Reserved,
/// Recovery) are copied sector by sector if VSS cannot snapshot them.
const SMALL_SYSTEM_VOLUME: u64 = 2 << 30;

/// Snapshots every volume whose method is [`Method::Snapshot`].
///
/// VSS needs free space on each volume, which small, nearly full system
/// partitions often lack. If the snapshot fails, those are read sector by
/// sector instead (Windows hardly writes to them) and the rest is retried.
fn create_snapshot(
    methods: &mut [Method<'_>],
    number: u32,
    rep: &mut dyn Reporter,
) -> Result<Option<SnapshotSet>> {
    let volumes = |methods: &[Method<'_>]| -> Vec<String> {
        methods
            .iter()
            .filter_map(|m| match m {
                Method::Snapshot(v) => Some(v.guid_path.clone()),
                _ => None,
            })
            .collect()
    };
    let create = |guids: &[String], rep: &mut dyn Reporter| -> Result<Option<SnapshotSet>> {
        if guids.is_empty() {
            return Ok(None);
        }
        note!(
            rep,
            "Creating VSS snapshot of {} volume(s) on HD{number} ...",
            guids.len()
        );
        let paths: Vec<&str> = guids.iter().map(String::as_str).collect();
        let set = SnapshotSet::create(&paths)?;
        for warning in set.warnings() {
            warn!(rep, "{warning}");
        }
        Ok(Some(set))
    };

    let error = match create(&volumes(methods), rep) {
        Ok(set) => return Ok(set),
        Err(error) => error,
    };
    let mut fallback = false;
    for method in methods.iter_mut() {
        if let Method::Snapshot(v) = method
            && v.mount_points.is_empty()
            && v.size <= SMALL_SYSTEM_VOLUME
        {
            *method = Method::AllSectors("all sectors, VSS not possible");
            fallback = true;
        }
    }
    if !fallback {
        return Err(error.context(VSS_HINT));
    }
    warn!(
        rep,
        "VSS snapshot failed ({error:#}); copying the small volumes without drive letter sector by sector and retrying"
    );
    create(&volumes(methods), rep).context(VSS_HINT)
}

/// Used ranges of a snapshotted volume minus the files that are not saved,
/// and the end of its last cluster.
fn volume_ranges(
    request: &BackupRequest,
    volume: &VolumeInfo,
    file: &std::fs::File,
    size: u64,
    root: &str,
    rep: &mut dyn Reporter,
) -> (RangeSet, u64) {
    let (mut used, clusters_end) =
        match used_ranges(file, size, volume.cluster_size, volume.sector_size) {
            Ok(allocation) => (allocation.used, allocation.clusters_end),
            Err(error) => {
                warn!(
                    rep,
                    "cannot read the allocation bitmap of {} ({error}); saving every sector",
                    volume.display_name()
                );
                (RangeSet::full(size), size)
            }
        };
    if !request.keep_system_files {
        used = used.subtract(&system_file_extents(root, volume.cluster_size));
    }
    if request.files_not_to_backup != ExclusionMode::None {
        let (excluded, _) = excluded_files(root, volume, request.files_not_to_backup);
        used = used.subtract(&excluded);
    }
    (used, clusters_end)
}

fn disk_info(disk: &DiskInfo, size: u64) -> SourceInfo {
    SourceInfo {
        kind: SourceKind::Disk,
        device: disk.path.clone(),
        size_bytes: size,
        sector_size: disk.sector_size,
        filesystem: None,
        label: None,
        disk_number: Some(disk.number),
        partition_number: None,
        partition_offset: None,
        hostname: std::env::var("COMPUTERNAME").ok(),
    }
}

fn summary(
    layout: &DiskLayout,
    disk: &DiskInfo,
    volumes: &[VolumeInfo],
    mut method: impl FnMut(&Partition) -> (&'static str, u64),
) -> DiskSummary {
    DiskSummary {
        style: format!("{:?}", layout.style).to_uppercase(),
        disk_id: layout.disk_id(),
        model: Some(disk.model.clone()).filter(|m| !m.is_empty()),
        partitions: layout
            .partitions
            .iter()
            .map(|p| {
                let volume = volume_of(volumes, disk.number, p);
                let (method, bytes_selected) = method(p);
                PartitionSummary {
                    number: p.number,
                    offset: p.offset,
                    length: p.length,
                    kind: p.type_name(),
                    filesystem: volume.map(|v| v.filesystem.clone()),
                    label: volume.map(|v| v.label.clone()).filter(|l| !l.is_empty()),
                    mount_points: volume.map(|v| v.mount_points.clone()).unwrap_or_default(),
                    method: method.to_string(),
                    bytes_selected,
                }
            })
            .collect(),
    }
}

/// Reports how each partition of a disk image is (or was) saved.
pub fn print_plan(summary: &DiskSummary, rep: &mut dyn Reporter) {
    for p in &summary.partitions {
        let name = p
            .mount_points
            .first()
            .cloned()
            .or_else(|| p.label.clone())
            .unwrap_or_default();
        note!(
            rep,
            "  #{:<2} {:>11} {:<20} {:<6} {:<10} {}: {}",
            p.number,
            human(p.length),
            p.kind,
            p.filesystem.as_deref().unwrap_or("-"),
            name,
            p.method,
            human(p.bytes_selected)
        );
    }
}

/// Checks that a disk image fits onto disk `number` and reports what would
/// be overwritten.
pub(crate) fn plan_disk_restore(
    mut opened: OpenedImage,
    number: u32,
    image_files: &[PathBuf],
    rep: &mut dyn Reporter,
) -> Result<DiskRestore> {
    let disk = find_disk(number)?;
    let layout = opened.layout()?;
    let source = opened.header.metadata.source.clone();
    if disk.sector_size != layout.sector_size {
        bail!(
            "HD{number} has {}-byte sectors, the image {}-byte sectors; a disk image can only be restored onto a disk with the same sector size",
            disk.sector_size,
            layout.sector_size
        );
    }
    let required = layout.required_size();
    if disk.size < required {
        bail!(
            "HD{number} is too small: {} needed, {} available",
            human(required),
            human(disk.size)
        );
    }
    // Dynamic disks keep their database at a fixed place near the end.
    if layout.partitions.iter().any(Partition::is_pooled) && disk.size != source.size_bytes {
        bail!(
            "the image holds a dynamic disk or Storage Spaces member; it can only be restored onto a disk of exactly {}",
            human(source.size_bytes)
        );
    }

    let volumes = volumes_on(number)?;
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()) + "\\";
    for volume in &volumes {
        if volume
            .mount_points
            .iter()
            .any(|m| m.eq_ignore_ascii_case(&system_drive))
        {
            bail!(
                "HD{number} holds the running Windows ({system_drive}); restore it from WinPE instead"
            );
        }
        for file in image_files {
            let path = std::path::absolute(file)?;
            let path = path.to_string_lossy().to_ascii_lowercase();
            if volume
                .mount_points
                .iter()
                .any(|m| path.starts_with(&m.to_ascii_lowercase()))
            {
                bail!(
                    "{} is stored on HD{number} ({}), the disk to be overwritten",
                    file.display(),
                    volume.display_name()
                );
            }
        }
    }

    say!(
        rep,
        "Image:  disk {} {}, {}, {}",
        format!("{:?}", layout.style).to_uppercase(),
        layout.disk_id(),
        human(source.size_bytes),
        source.hostname.as_deref().unwrap_or("unknown computer")
    );
    if let Some(base) = &opened.base_path {
        say!(rep, "        differential, full image {}", base.display());
    }
    match &opened.header.metadata.disk {
        Some(summary) => print_plan(summary, rep),
        None => {
            for p in &layout.partitions {
                say!(
                    rep,
                    "  #{:<2} {:>11} {}",
                    p.number,
                    human(p.length),
                    p.type_name()
                );
            }
        }
    }
    say!(
        rep,
        "Target: HD{number} {}, {}",
        human(disk.size),
        if disk.model.is_empty() {
            "unknown model"
        } else {
            &disk.model
        }
    );
    for volume in &volumes {
        say!(
            rep,
            "        {} {} \"{}\" ({}) will be overwritten",
            volume.display_name(),
            volume.filesystem,
            volume.label,
            human(volume.size)
        );
    }
    if disk.size > source.size_bytes {
        note!(
            rep,
            "note: the target is {} larger; that space stays unallocated",
            human(disk.size - source.size_bytes)
        );
    } else if disk.size < source.size_bytes {
        note!(
            rep,
            "note: the target is {} smaller; all partitions still fit",
            human(source.size_bytes - disk.size)
        );
    }
    Ok(DiskRestore {
        opened,
        disk,
        layout,
        volumes,
    })
}

/// Restore of a disk image onto a whole disk.
pub struct DiskRestore {
    opened: OpenedImage,
    disk: DiskInfo,
    layout: DiskLayout,
    volumes: Vec<VolumeInfo>,
}

impl DiskRestore {
    pub(crate) fn target_name(&self) -> String {
        format!("HD{} ({})", self.disk.number, human(self.disk.size))
    }

    /// The volumes on the target are locked and dismounted for the whole
    /// run, which is what Windows requires for raw writes inside them. The
    /// partition data is written first and the partition table last.
    pub(crate) fn run(
        mut self,
        verify: bool,
        cancel: &AtomicBool,
        rep: &mut dyn Reporter,
    ) -> Result<()> {
        let (disk, layout) = (&self.disk, &self.layout);
        let number = disk.number;
        let control = open_disk_for_writing(number)
            .with_context(|| format!("cannot open HD{number} (run as administrator)"))?;
        let (offline, read_only) = disk_attributes(&control).unwrap_or((false, false));
        if read_only {
            set_disk_attributes(&control, offline, false, false)
                .with_context(|| format!("HD{number} is read-only and cannot be made writable"))?;
            note!(
                rep,
                "HD{number} was read-only; made it writable for this restore"
            );
        }
        // Handles stay open until the end; closing one would let Windows mount
        // the volume again.
        let mut locked = Vec::new();
        for volume in &self.volumes {
            let device = RawDevice::open_write(&volume.device_path(), volume.sector_size)
                .with_context(|| format!("cannot open {}", volume.display_name()))?;
            device.lock_and_dismount().with_context(|| {
                format!(
                    "{} is in use; close all programs and windows using it",
                    volume.display_name()
                )
            })?;
            locked.push(device);
        }
        let mut target = RawDevice::open_write(&disk.path, disk.sector_size)
            .with_context(|| format!("cannot open {} for writing", disk.path))?;

        let view = self.opened.view.as_mut();
        let result = restore_disk_data(view, &mut target, layout, cancel, |p| {
            rep.progress(Stage::Restore, p)
        });
        rep.stage_done(Stage::Restore);
        if let Ok(stats) = &result {
            say!(rep, "Done: {} written", human(stats.bytes_written));
        }
        // While the volumes are still locked, before Windows sees the new table.
        let verified = match &result {
            Ok(stats) if verify => verify_restored(view, &target, &stats.copied, cancel, rep),
            _ => Ok(()),
        };
        drop(target);
        drop(locked);
        if let Err(error) = refresh_disk(&control) {
            warn!(
                rep,
                "Windows could not re-read the partition table of HD{number} ({error}); restart before using the disk"
            );
        }
        result.with_context(|| {
            format!(
                "restore of HD{number} failed; the disk is incomplete and must be restored again"
            )
        })?;
        verified
            .with_context(|| format!("HD{number} does not match the image; restore it again"))?;

        // A second disk with the same ID makes Windows keep this one offline;
        // bringing it online would give it a new ID and break its boot entries.
        let disk_id = layout.disk_id().to_ascii_lowercase();
        let twin = list_disks().into_iter().find(|d| {
            d.number != number
                && d.layout.as_ref().is_some_and(|l| match l {
                    WinDiskLayout::Mbr { signature, .. } => format!("{signature:08x}") == disk_id,
                    WinDiskLayout::Gpt { disk_id: id, .. } => format!("{{{id}}}") == disk_id,
                    WinDiskLayout::Raw => false,
                })
        });
        match twin {
            Some(twin) => say!(
                rep,
                "note: HD{} has the same disk ID {}; Windows keeps HD{number} offline. Do NOT set it online \
                 here, that would change its ID and it would no longer start. Shut down, remove HD{} \
                 and start from HD{number}.",
                twin.number,
                layout.disk_id(),
                twin.number
            ),
            None if offline => say!(
                rep,
                "note: HD{number} was offline before and stays offline; set it online in Disk Management to use it"
            ),
            None => say!(rep, "HD{number} now has the partitions of the image"),
        }
        Ok(())
    }
}
