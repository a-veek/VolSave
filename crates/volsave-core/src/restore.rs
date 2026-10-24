use std::io::{self, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use volsave_format::ImageWriter;

use crate::{
    AlignedBuf, BackupError, BackupOptions, BackupOutput, BlockSource, BlockTarget, ImageView,
    Progress, backup,
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
    for block in regions.plan(RESTORE_CHUNK) {
        for part in &block.parts {
            if cancel.load(Ordering::Relaxed) {
                return Err(RestoreError::Cancelled);
            }
            // Aligned, because devices are written unbuffered.
            let mut data = AlignedBuf::zeroed((part.end - part.start) as usize);
            image.read_at(part.start, &mut data)?;
            target
                .write_at(part.start, &data)
                .map_err(|source| RestoreError::Write {
                    offset: part.start,
                    source,
                })?;
            report.bytes_read += data.len() as u64;
            report.bytes_written = report.bytes_read;
            progress(&report);
        }
    }
    target.flush().map_err(|source| RestoreError::Write {
        offset: needed,
        source,
    })?;
    Ok(RestoreStats {
        bytes_written: report.bytes_written,
    })
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
