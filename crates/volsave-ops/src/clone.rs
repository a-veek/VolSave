//! Cloning a whole disk onto another one, without an image in between.
//!
//! The source is read like for a disk image (VSS snapshots, only the used
//! blocks, BitLocker volumes decrypted) and written like a disk restore,
//! then read back and compared. Like a restore, a clone is planned first
//! ([`plan_clone`]) and run only after the caller has asked for
//! confirmation.

use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use volsave_core::{BlockSource, DiskLayout, LayoutError, Partition, SourceView};
use volsave_win::{
    DiskInfo, ExclusionMode, RawDevice, VolumeInfo, enable_backup_privileges, is_winpe,
};

use crate::backup::BackupRequest;
use crate::disk::{Purpose, WriteMode, find_disk, parse_disk, read_disk, volumes_on, write_disk};
use crate::{Reporter, human};

/// Which disk to copy onto which, and how.
#[derive(Debug, Clone)]
pub struct CloneRequest {
    /// The disk to copy (`HD0`, `PhysicalDrive0`, ...).
    pub source: String,
    /// The disk to overwrite.
    pub target: String,
    /// Read the target back and compare it with the source.
    pub verify: bool,
    /// Read the live volumes instead of VSS snapshots.
    pub no_vss: bool,
    /// Also copy pagefile.sys, swapfile.sys and hiberfil.sys.
    pub keep_system_files: bool,
    /// Which entries of the Windows list FilesNotToBackup to leave out.
    pub files_not_to_backup: ExclusionMode,
    /// Copy every sector instead of the used blocks.
    pub all_sectors: bool,
    /// Copy unlocked BitLocker volumes as encrypted sectors.
    pub bitlocker_raw: bool,
}

impl CloneRequest {
    /// A clone of `source` onto `target` with default settings.
    pub fn new(source: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            verify: true,
            no_vss: false,
            keep_system_files: false,
            files_not_to_backup: ExclusionMode::Safe,
            all_sectors: false,
            bitlocker_raw: false,
        }
    }
}

/// A checked clone, ready to run.
pub struct ClonePlan {
    request: BackupRequest,
    source: u32,
    target: DiskInfo,
    target_volumes: Vec<VolumeInfo>,
    target_empty: bool,
    verify: bool,
}

/// Checks that disk `source` fits onto disk `target` and reports what
/// would be overwritten.
pub fn plan_clone(request: &CloneRequest, rep: &mut dyn Reporter) -> Result<ClonePlan> {
    let disk_number = |spec: &str| {
        parse_disk(spec).with_context(|| {
            format!("{spec} is not a disk; clone copies whole disks (HD0, HD1, ...)")
        })
    };
    let source = disk_number(&request.source)?;
    let target = disk_number(&request.target)?;
    if source == target {
        bail!("source and target are the same disk (HD{source})");
    }
    enable_backup_privileges();
    let source_disk = find_disk(source)?;
    let target_disk = find_disk(target)?;
    if source_disk.sector_size != target_disk.sector_size {
        bail!(
            "HD{source} has {}-byte sectors, HD{target} {}-byte sectors; a disk can only be cloned onto one with the same sector size",
            source_disk.sector_size,
            target_disk.sector_size
        );
    }

    let device = RawDevice::open_read(&source_disk.path, source_disk.sector_size)
        .with_context(|| format!("cannot open {} (run as administrator)", source_disk.path))?;
    let size = BlockSource::size(&device);
    let layout = match DiskLayout::read(
        &mut |offset, buf| device.read_at(offset, buf),
        size,
        source_disk.sector_size,
    ) {
        Ok(layout) => Some(layout),
        Err(_) if request.all_sectors => None,
        Err(LayoutError::NoPartitionTable) => {
            bail!("HD{source} has no partition table; use --all-sectors to copy it anyway")
        }
        Err(error) => {
            bail!("HD{source}: {error}; use --all-sectors to copy it sector by sector")
        }
    };
    let required = layout.as_ref().map_or(size, DiskLayout::required_size);
    if target_disk.size < required {
        bail!(
            "HD{target} is too small: {} needed, {} available",
            human(required),
            human(target_disk.size)
        );
    }
    // Dynamic disks keep their database at a fixed place near the end.
    if let Some(layout) = &layout
        && layout.partitions.iter().any(Partition::is_pooled)
        && target_disk.size != size
    {
        bail!(
            "HD{source} is a dynamic disk or Storage Spaces member; it can only be cloned onto a disk of exactly {}",
            human(size)
        );
    }

    let target_volumes = volumes_on(target)?;
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()) + "\\";
    if target_volumes.iter().any(|v| {
        v.mount_points
            .iter()
            .any(|m| m.eq_ignore_ascii_case(&system_drive))
    }) {
        bail!("HD{target} holds the running Windows ({system_drive}); it cannot be overwritten");
    }
    let target_partitions = target_disk.layout.as_ref().map(|l| l.partitions().len());
    let target_empty = target_partitions == Some(0);

    say!(
        rep,
        "Source: HD{source} {}, {}",
        human(size),
        model(&source_disk)
    );
    if let Some(layout) = &layout {
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
    say!(
        rep,
        "Target: HD{target} {}, {}",
        human(target_disk.size),
        model(&target_disk)
    );
    match target_partitions {
        Some(0) => say!(rep, "        empty"),
        Some(n) => warn!(
            rep,
            "HD{target} is not empty: its {n} partition(s) and all data on it will be overwritten"
        ),
        None => warn!(
            rep,
            "the partition table of HD{target} cannot be read; everything on it will be overwritten"
        ),
    }
    for volume in &target_volumes {
        say!(
            rep,
            "        {} {} \"{}\" ({}) will be overwritten",
            volume.display_name(),
            volume.filesystem,
            volume.label,
            human(volume.size)
        );
    }
    if target_disk.size > size {
        note!(
            rep,
            "note: the target is {} larger; that space stays unallocated",
            human(target_disk.size - size)
        );
    } else if target_disk.size < size {
        note!(
            rep,
            "note: the target is {} smaller; all partitions still fit",
            human(size - target_disk.size)
        );
    }
    note!(
        rep,
        "note: the clone gets the same disk ID as HD{source}, so HD{target} is taken offline and stays offline"
    );

    let mut backup = BackupRequest::new(&request.source, "");
    backup.no_vss = request.no_vss;
    backup.keep_system_files = request.keep_system_files;
    backup.files_not_to_backup = request.files_not_to_backup;
    backup.all_sectors = request.all_sectors;
    backup.bitlocker_raw = request.bitlocker_raw;
    Ok(ClonePlan {
        request: backup,
        source,
        target: target_disk,
        target_volumes,
        target_empty,
        verify: request.verify,
    })
}

fn model(disk: &DiskInfo) -> &str {
    if disk.model.is_empty() {
        "unknown model"
    } else {
        &disk.model
    }
}

impl ClonePlan {
    /// `HD1 (500 GB)`.
    pub fn target_name(&self) -> String {
        format!("HD{} ({})", self.target.number, human(self.target.size))
    }

    /// Whether the target has no partitions; front-ends warn otherwise.
    pub fn target_empty(&self) -> bool {
        self.target_empty
    }

    /// Copies the source onto the target and verifies it; the target is
    /// offline afterwards.
    pub fn run(mut self, cancel: &AtomicBool, rep: &mut dyn Reporter) -> Result<()> {
        // VSS does not work in Windows PE, and the installed Windows does not
        // change its volumes there.
        if !self.request.no_vss && is_winpe() {
            note!(
                rep,
                "Windows PE: the installed Windows is not running; volumes are read directly without VSS snapshots"
            );
            self.request.no_vss = true;
        }
        let (source, target) = (self.source, self.target.number);
        let mode = WriteMode {
            verify: self.verify,
            clone: true,
        };
        read_disk(&self.request, source, Purpose::Clone, rep, |job, rep| {
            let mut view = SourceView::new(job.source, job.used);
            write_disk(
                &mut view,
                &self.target,
                job.layout,
                &self.target_volumes,
                mode,
                cancel,
                rep,
            )
            .map(drop)
        })?;
        say!(
            rep,
            "HD{target} is now a copy of HD{source} and stays offline: both have the same disk ID. \
             Do NOT set it online while HD{source} is connected - Windows would give it a new ID \
             and it would no longer start. Shut down, remove HD{source} and start from HD{target}."
        );
        Ok(())
    }
}
