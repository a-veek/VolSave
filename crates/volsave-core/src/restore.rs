use std::io::{self, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use volsave_format::ImageWriter;

use crate::{
    AlignedBuf, BackupError, BackupOptions, BackupOutput, BlockSource, BlockTarget, DiskLayout,
    ImageView, LayoutError, Progress, RangeSet, backup,
};

/// Largest single write during a restore.
const RESTORE_CHUNK: u32 = 1 << 20;

#[derive(Debug, Clone, Copy, Default)]
pub struct RestoreStats {
    pub bytes_written: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("the image needs {needed} bytes but the target has only {available}")]
    TargetTooSmall { needed: u64, available: u64 },
    #[error("write error at target offset {offset}: {source}")]
    Write {
        offset: u64,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Format(#[from] volsave_format::Error),
    #[error(transparent)]
    Layout(#[from] LayoutError),
    #[error("restore cancelled")]
    Cancelled,
}

/// Writes everything the image stores to the same offsets on `target`.
/// Areas the image does not contain are left untouched.
pub fn restore(
    image: &mut dyn ImageView,
    target: &mut dyn BlockTarget,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&Progress),
) -> Result<RestoreStats, RestoreError> {
    let regions = image.regions();
    let needed = regions.ranges().last().map_or(0, |r| r.end);
    if needed > target.size() {
        return Err(RestoreError::TargetTooSmall {
            needed,
            available: target.size(),
        });
    }
    let mut report = Progress {
        bytes_total: regions.total_len(),
        ..Progress::default()
    };
    copy_regions(image, target, &regions, cancel, &mut report, &mut progress)?;
    flush(target)?;
    Ok(RestoreStats {
        bytes_written: report.bytes_written,
    })
}

/// Restores a disk image onto a whole disk, which may be larger or smaller
/// than the original as long as the partitions fit; a GPT is moved to the
/// end of the target.
///
/// The partition table and everything before the first partition are
/// written last, so the target only shows the new layout once the
/// partitions hold their data.
pub fn restore_disk(
    image: &mut dyn ImageView,
    target: &mut dyn BlockTarget,
    layout: &DiskLayout,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&Progress),
) -> Result<RestoreStats, RestoreError> {
    let plan = layout.retarget(target.size())?;
    let regions = image
        .regions()
        .subtract(&RangeSet::from_ranges(plan.skip.iter().cloned()))
        .clamp(0..target.size());
    let head = RangeSet::from_ranges([layout.head_range()]);
    let body = regions.subtract(&head);
    let head = regions.clamp(layout.head_range());
    let extra: u64 = plan.writes.iter().map(|(_, d)| d.len() as u64).sum::<u64>()
        + plan.zero.iter().map(|r| r.end - r.start).sum::<u64>();

    let mut report = Progress {
        bytes_total: regions.total_len() + extra,
        ..Progress::default()
    };
    copy_regions(image, target, &body, cancel, &mut report, &mut progress)?;
    for range in &plan.zero {
        let mut pos = range.start;
        while pos < range.end {
            let len = (range.end - pos).min(u64::from(RESTORE_CHUNK));
            write(target, pos, &AlignedBuf::zeroed(len as usize))?;
            pos += len;
            report.add(len);
            progress(&report);
        }
    }
    for (offset, data) in &plan.writes {
        write_aligned(target, *offset, data)?;
        report.add(data.len() as u64);
        progress(&report);
    }
    copy_regions(image, target, &head, cancel, &mut report, &mut progress)?;
    for (offset, data) in &plan.patches {
        write_aligned(target, *offset, data)?;
    }
    flush(target)?;
    Ok(RestoreStats {
        bytes_written: report.bytes_written,
    })
}

fn copy_regions(
    image: &mut dyn ImageView,
    target: &mut dyn BlockTarget,
    regions: &RangeSet,
    cancel: &AtomicBool,
    report: &mut Progress,
    progress: &mut impl FnMut(&Progress),
) -> Result<(), RestoreError> {
    for block in regions.plan(RESTORE_CHUNK) {
        for part in &block.parts {
            if cancel.load(Ordering::Relaxed) {
                return Err(RestoreError::Cancelled);
            }
            // Aligned, because devices are written unbuffered.
            let mut data = AlignedBuf::zeroed((part.end - part.start) as usize);
            image.read_at(part.start, &mut data)?;
            write(target, part.start, &data)?;
            report.add(data.len() as u64);
            progress(report);
        }
    }
    Ok(())
}

fn write(target: &mut dyn BlockTarget, offset: u64, data: &[u8]) -> Result<(), RestoreError> {
    target
        .write_at(offset, data)
        .map_err(|source| RestoreError::Write { offset, source })
}

fn write_aligned(
    target: &mut dyn BlockTarget,
    offset: u64,
    data: &[u8],
) -> Result<(), RestoreError> {
    let mut buf = AlignedBuf::zeroed(data.len());
    buf.copy_from_slice(data);
    write(target, offset, &buf)
}

fn flush(target: &mut dyn BlockTarget) -> Result<(), RestoreError> {
    target.flush().map_err(|source| RestoreError::Write {
        offset: target.size(),
        source,
    })
}

impl Progress {
    fn add(&mut self, bytes: u64) {
        self.bytes_read += bytes;
        self.bytes_written = self.bytes_read;
    }
}

/// Writes the combined content of `image` (typically full + differential)
/// as a new full image, including its hash list.
pub fn merge<W: Write>(
    image: Box<dyn ImageView>,
    writer: ImageWriter<W>,
    options: &BackupOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(&Progress),
) -> Result<BackupOutput<W>, BackupError> {
    let regions = image.regions();
    let source = ViewSource {
        size: image.source_size(),
        view: Mutex::new(image),
    };
    backup(&source, &regions, writer, None, options, cancel, progress)
}

/// Lets the backup pipeline read from an image.
struct ViewSource {
    size: u64,
    view: Mutex<Box<dyn ImageView>>,
}

impl BlockSource for ViewSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut view = self.view.lock().expect("image view lock poisoned");
        view.read_at(offset, buf).map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use volsave_format::{FileHeader, ImageMetadata, ImageReader};

    use super::*;
    use crate::partition::tests::{gpt_disk, layout_of, mbr_disk};
    use crate::{PartitionView as View, SingleImage};

    struct Disk(Vec<u8>);

    impl BlockSource for Disk {
        fn size(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            buf.copy_from_slice(&self.0[offset as usize..offset as usize + buf.len()]);
            Ok(())
        }
    }

    impl BlockTarget for Disk {
        fn size(&self) -> u64 {
            self.0.len() as u64
        }
        fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
            self.0[offset as usize..offset as usize + data.len()].copy_from_slice(data);
            Ok(())
        }
    }

    /// Fills every partition with a recognisable pattern and images the
    /// tables plus the partitions.
    fn image_of(mut disk: Vec<u8>) -> (Vec<u8>, Vec<u8>, DiskLayout) {
        let layout = layout_of(&disk);
        for p in &layout.partitions {
            for (i, b) in disk[p.offset as usize..p.end() as usize]
                .iter_mut()
                .enumerate()
            {
                *b = (i % 253) as u8 ^ p.number as u8;
            }
        }
        let used = layout.table_ranges().union(&RangeSet::from_ranges(
            layout.partitions.iter().map(|p| p.range()),
        ));
        let writer =
            ImageWriter::new(Vec::new(), FileHeader::new(ImageMetadata::default())).unwrap();
        let output = backup(
            &Disk(disk.clone()),
            &used,
            writer,
            None,
            &BackupOptions::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        (disk, output.sink, layout)
    }

    fn restore_onto(image: &[u8], layout: &DiskLayout, size: usize) -> Vec<u8> {
        let mut view = SingleImage::new(ImageReader::open(Cursor::new(image.to_vec())).unwrap());
        let mut target = Disk(vec![0xEE; size]);
        restore_disk(
            &mut view,
            &mut target,
            layout,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        target.0
    }

    fn assert_partitions_equal(original: &[u8], restored: &[u8], layout: &DiskLayout) {
        for p in &layout.partitions {
            assert!(
                original[p.offset as usize..p.end() as usize]
                    == restored[p.offset as usize..p.end() as usize],
                "partition {} differs",
                p.number
            );
        }
    }

    #[test]
    fn disk_restore_onto_same_size_is_an_exact_copy_of_saved_areas() {
        let (disk, image, layout) = image_of(gpt_disk(32 << 20));
        let restored = restore_onto(&image, &layout, disk.len());
        for range in layout.table_ranges().ranges() {
            assert!(
                disk[range.start as usize..range.end as usize]
                    == restored[range.start as usize..range.end as usize]
            );
        }
        assert_partitions_equal(&disk, &restored, &layout);
        assert_eq!(layout_of(&restored), layout);
    }

    #[test]
    fn disk_restore_onto_larger_disk_moves_the_backup_gpt() {
        let (disk, image, layout) = image_of(gpt_disk(32 << 20));
        let restored = restore_onto(&image, &layout, 40 << 20);
        let moved = layout_of(&restored);
        assert_eq!(moved.partitions, layout.partitions);
        assert_eq!(moved.gpt.unwrap().alternate_lba, (40 << 20) / 512 - 1);
        assert_partitions_equal(&disk, &restored, &layout);
    }

    #[test]
    fn disk_restore_of_mbr_keeps_logical_partitions() {
        let (disk, image, layout) = image_of(mbr_disk());
        let restored = restore_onto(&image, &layout, 80 << 20);
        assert_eq!(layout_of(&restored).partitions, layout.partitions);
        assert_partitions_equal(&disk, &restored, &layout);
        // Old content at the end of the larger target is cleared.
        assert!(restored[(79 << 20)..].iter().all(|&b| b == 0));
    }

    #[test]
    fn disk_restore_refuses_too_small_targets() {
        let (_, image, layout) = image_of(gpt_disk(32 << 20));
        let mut view = SingleImage::new(ImageReader::open(Cursor::new(image)).unwrap());
        let mut target = Disk(vec![0; 16 << 20]);
        let result = restore_disk(
            &mut view,
            &mut target,
            &layout,
            &AtomicBool::new(false),
            |_| {},
        );
        assert!(matches!(result, Err(RestoreError::Layout(_))));
        assert!(target.0.iter().all(|&b| b == 0), "nothing written");
    }

    #[test]
    fn partition_view_restores_one_partition() {
        let (disk, image, layout) = image_of(mbr_disk());
        let view = SingleImage::new(ImageReader::open(Cursor::new(image)).unwrap());
        let p = &layout.partitions[2];
        let mut part = View::new(Box::new(view), p.range());
        let mut target = Disk(vec![0; p.length as usize]);
        restore(&mut part, &mut target, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(target.0 == disk[p.offset as usize..p.end() as usize]);
    }
}
