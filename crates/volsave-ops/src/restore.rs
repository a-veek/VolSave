//! Restoring onto a volume or, via [`crate::disk`], a whole disk.
//!
//! A restore is planned first: [`plan_restore`] checks the target and
//! describes what will be overwritten. Only [`RestorePlan::run`] writes, so
//! the caller can ask for confirmation in between.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use volsave_core::{
    ImageView, RangeSet, fix_ntfs_hidden_sectors, restore as run_restore, verify_target,
};
use volsave_format::SourceInfo;
use volsave_win::{RawDevice, VolumeInfo, resolve_volume};

use crate::disk::{DiskRestore, parse_disk, plan_disk_restore};
use crate::images::open_view;
use crate::{Reporter, Stage, human};

/// A checked restore, ready to run.
// Created once per restore; the size difference does not matter.
#[allow(clippy::large_enum_variant)]
pub enum RestorePlan {
    Volume(VolumeRestore),
    Disk(DiskRestore),
}

impl RestorePlan {
    /// The target as named in a confirmation question, e.g. `X:\` or `HD1 (80.00 GiB)`.
    pub fn target_name(&self) -> String {
        match self {
            RestorePlan::Volume(plan) => plan.volume.display_name().to_string(),
            RestorePlan::Disk(plan) => plan.target_name(),
        }
    }

    /// Overwrites the target. With `verify` the written data is read back
    /// and compared with the image.
    pub fn run(self, verify: bool, cancel: &AtomicBool, rep: &mut dyn Reporter) -> Result<()> {
        match self {
            RestorePlan::Volume(plan) => plan.run(verify, cancel, rep),
            RestorePlan::Disk(plan) => plan.run(verify, cancel, rep),
        }
    }
}

/// Checks that `image` can be restored onto `target` (a volume, or a disk
/// such as `HD1` for disk images) and reports what would be overwritten.
/// `partition` picks one partition of a disk image for a volume target.
pub fn plan_restore(
    image: &Path,
    target: &str,
    base: Option<&Path>,
    partition: Option<u32>,
    rep: &mut dyn Reporter,
) -> Result<RestorePlan> {
    let opened = open_view(image, base, rep)?;
    let mut image_files = vec![image.to_path_buf()];
    image_files.extend(opened.base_path.clone());
    match (parse_disk(target), opened.is_disk(), partition) {
        (Some(number), true, None) => {
            return plan_disk_restore(opened, number, &image_files, rep).map(RestorePlan::Disk);
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
    let view = match partition {
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
        rep,
        "Image:  {} {} ({}), {}",
        source.filesystem.as_deref().unwrap_or("?"),
        source.label.as_deref().unwrap_or(""),
        human(source.size_bytes),
        source.hostname.as_deref().unwrap_or("unknown computer")
    );
    if let Some(base_path) = &base_path {
        say!(
            rep,
            "        differential, full image {}",
            base_path.display()
        );
    }
    say!(
        rep,
        "Target: {} {} \"{}\" ({})",
        volume.display_name(),
        volume.filesystem,
        volume.label,
        human(volume.size)
    );
    Ok(RestorePlan::Volume(VolumeRestore {
        view,
        volume,
        source,
    }))
}

/// Restore of a volume image (or one partition of a disk image) onto a volume.
pub struct VolumeRestore {
    view: Box<dyn ImageView>,
    volume: VolumeInfo,
    source: SourceInfo,
}

impl VolumeRestore {
    fn run(mut self, verify: bool, cancel: &AtomicBool, rep: &mut dyn Reporter) -> Result<()> {
        let volume = &self.volume;
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
        let result = run_restore(self.view.as_mut(), &mut device, cancel, |p| {
            rep.progress(Stage::Restore, p)
        });
        rep.stage_done(Stage::Restore);
        let stats = result?;
        say!(rep, "Done: {} written", human(stats.bytes_written));
        // Before the boot sector fix, which changes what the image holds.
        if verify {
            verify_restored(self.view.as_mut(), &device, &stats.copied, cancel, rep)?;
        }
        // The boot code finds BOOTMGR through the start sector stored in the boot
        // sector, which is wrong when the partition sits elsewhere than the source.
        if let [extent] = volume.extents.as_slice() {
            match fix_ntfs_hidden_sectors(&mut device, extent.offset) {
                Ok(Some(fix)) => say!(
                    rep,
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
                Err(e) => warn!(rep, "could not update the boot sector: {e}"),
            }
        }
        if volume.size > self.source.size_bytes {
            say!(
                rep,
                "note: the volume is {} larger than the image; extend the file system in Disk Management to use it",
                human(volume.size - self.source.size_bytes)
            );
        }
        Ok(())
    }
}

/// Reads the restored ranges back from the target and compares them with
/// the image.
pub(crate) fn verify_restored(
    view: &mut dyn ImageView,
    target: &RawDevice,
    copied: &RangeSet,
    cancel: &AtomicBool,
    rep: &mut dyn Reporter,
) -> Result<()> {
    let result = verify_target(view, target, copied, cancel, |p| {
        rep.progress(Stage::VerifyTarget, p)
    });
    rep.stage_done(Stage::VerifyTarget);
    let checked = result.context("verification of the restored data failed")?;
    say!(
        rep,
        "Verified: {} read back, identical to the image",
        human(checked)
    );
    Ok(())
}
