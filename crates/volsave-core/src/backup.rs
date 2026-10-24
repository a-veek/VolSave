use std::collections::BTreeMap;
use std::io::{self, Write};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

use crossbeam_channel::bounded;
use volsave_format::{
    ChunkHash, DEFAULT_ZSTD_LEVEL, EncodedBlock, HashList, ImageWriter, UNUSED_CHUNK, chunk_hash,
    encode_block,
};

use crate::{AlignedBuf, BlockSource, RangeSet};

#[derive(Debug, Clone)]
pub struct BackupOptions {
    pub zstd_level: i32,
    /// How many threads compress blocks in parallel.
    pub threads: usize,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            zstd_level: DEFAULT_ZSTD_LEVEL,
            threads: thread::available_parallelism().map_or(2, |n| n.get()),
        }
    }
}

/// Reported after every block written.
#[derive(Debug, Clone, Copy, Default)]
pub struct Progress {
    /// Used bytes read from the source so far.
    pub bytes_read: u64,
    /// Used bytes to read in total.
    pub bytes_total: u64,
    /// Bytes of image written so far.
    pub bytes_written: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BackupStats {
    /// Image blocks written.
    pub blocks: usize,
    pub bytes_read: u64,
    /// Volume bytes stored in the image (for a differential image: changed data).
    pub bytes_stored: u64,
    /// Image size without index and footer.
    pub bytes_written: u64,
}

/// Result of a successful backup.
pub struct BackupOutput<W> {
    pub sink: W,
    pub stats: BackupStats,
    /// Chunk hashes of the volume, for later differential backups.
    pub hashes: HashList,
}

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("read error at source offset {offset}: {source}")]
    Read {
        offset: u64,
        #[source]
        source: io::Error,
    },
    #[error("the hash list does not fit this backup: {0}")]
    BaseMismatch(&'static str),
    #[error(transparent)]
    Format(#[from] volsave_format::Error),
    #[error("backup cancelled")]
    Cancelled,
}

struct Job {
    seq: u64,
    start: u64,
    parts: Vec<Range<u64>>,
    data: AlignedBuf,
}

/// A planned block after hashing and compression.
struct Encoded {
    blocks: Vec<EncodedBlock>,
    first_chunk: usize,
    hashes: Vec<ChunkHash>,
}

/// Copies the `used` parts of `source` into `writer` and finishes the image.
///
/// With `base` (the hash list of a full image) only chunks whose hash
/// differs from the list are stored, which makes a differential image; its
/// header must name the full image as base. Without `base` everything used
/// is stored. Either way the hashes of the current volume are returned.
///
/// One thread reads, `options.threads` threads hash and compress, and the
/// calling thread writes the blocks in order. Bytes outside `used` are
/// stored as zeros, which keeps free space cheap to store.
pub fn backup<W: Write>(
    source: &dyn BlockSource,
    used: &RangeSet,
    mut writer: ImageWriter<W>,
    base: Option<&HashList>,
    options: &BackupOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&Progress),
) -> Result<BackupOutput<W>, BackupError> {
    let header = writer.header().clone();
    let (block_size, chunk_size) = (header.block_size, header.hash_chunk_size);
    if block_size % chunk_size != 0 {
        return Err(BackupError::BaseMismatch(
            "block size must be a multiple of the hash chunk size",
        ));
    }
    if let Some(base) = base {
        if header.base_image_id != Some(base.image_id) {
            return Err(BackupError::BaseMismatch(
                "the image header names a different base image",
            ));
        }
        if base.chunk_size != chunk_size {
            return Err(BackupError::BaseMismatch("different hash chunk size"));
        }
        if base.volume_size != source.size() {
            return Err(BackupError::BaseMismatch(
                "the volume size changed since the full backup; make a new full backup",
            ));
        }
    }

    let used = used.clamp(0..source.size());
    let mut hashes = HashList::new(header.image_id, chunk_size, source.size())?;
    let threads = options.threads.max(1);
    let level = options.zstd_level;
    let bytes_read = AtomicU64::new(0);
    let mut report = Progress {
        bytes_total: used.total_len(),
        ..Progress::default()
    };
    let mut stats = BackupStats::default();

    let (job_tx, job_rx) = bounded::<Job>(threads * 2);
    let (done_tx, done_rx) = bounded::<(u64, volsave_format::Result<Encoded>)>(threads * 2);

    let outcome = thread::scope(|scope| {
        let (used, bytes_read) = (&used, &bytes_read);
        // `move` hands `job_tx` to the reader, so the workers stop once it is done.
        let reader = scope.spawn(move || -> Result<(), BackupError> {
            for (seq, block) in used.plan(block_size).enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    return Err(BackupError::Cancelled);
                }
                let mut data = AlignedBuf::zeroed(block.len as usize);
                for part in &block.parts {
                    let at = (part.start - block.start) as usize;
                    let len = (part.end - part.start) as usize;
                    source
                        .read_at(part.start, &mut data[at..at + len])
                        .map_err(|source| BackupError::Read {
                            offset: part.start,
                            source,
                        })?;
                    bytes_read.fetch_add(len as u64, Ordering::Relaxed);
                }
                let job = Job {
                    seq: seq as u64,
                    start: block.start,
                    parts: block.parts,
                    data,
                };
                if job_tx.send(job).is_err() {
                    break; // the writer gave up
                }
            }
            Ok(())
        });

        for _ in 0..threads {
            let job_rx = job_rx.clone();
            let done_tx = done_tx.clone();
            scope.spawn(move || {
                for job in job_rx {
                    let encoded = encode_job(&job, chunk_size, base, level);
                    if done_tx.send((job.seq, encoded)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(job_rx);
        drop(done_tx);

        // Workers finish out of order; write strictly by sequence number.
        let write_result = (|| -> Result<(), BackupError> {
            let mut pending = BTreeMap::new();
            let mut next = 0u64;
            for (seq, encoded) in &done_rx {
                pending.insert(seq, encoded);
                while let Some(encoded) = pending.remove(&next) {
                    let encoded = encoded?;
                    for block in &encoded.blocks {
                        writer.write_block(block)?;
                        stats.blocks += 1;
                        stats.bytes_stored += u64::from(block.header.raw_len);
                    }
                    for (n, hash) in encoded.hashes.into_iter().enumerate() {
                        hashes.set(encoded.first_chunk + n, hash)?;
                    }
                    next += 1;
                    report.bytes_read = bytes_read.load(Ordering::Relaxed);
                    report.bytes_written = writer.bytes_written();
                    progress(&report);
                }
            }
            Ok(())
        })();
        // Unblock workers and reader if writing failed.
        drop(done_rx);
        let read_result = reader.join().expect("reader thread panicked");
        read_result.and(write_result)
    });

    outcome?;
    if cancel.load(Ordering::Relaxed) {
        return Err(BackupError::Cancelled);
    }
    stats.bytes_read = bytes_read.load(Ordering::Relaxed);
    stats.bytes_written = writer.bytes_written();
    Ok(BackupOutput {
        sink: writer.finish()?,
        stats,
        hashes,
    })
}

/// Hashes the chunks of one planned block and compresses what must be stored:
/// the whole block for a full image, only runs of changed chunks otherwise.
fn encode_job(
    job: &Job,
    chunk_size: u32,
    base: Option<&HashList>,
    level: i32,
) -> volsave_format::Result<Encoded> {
    let chunk_size = chunk_size as usize;
    let first_chunk = (job.start / chunk_size as u64) as usize;
    let mut hashes = Vec::new();
    let mut changed = Vec::new();
    for (n, chunk) in job.data.chunks(chunk_size).enumerate() {
        let start = job.start + (n * chunk_size) as u64;
        let end = start + chunk.len() as u64;
        let in_use = job.parts.iter().any(|p| p.start < end && p.end > start);
        let hash = if in_use {
            chunk_hash(chunk)
        } else {
            UNUSED_CHUNK
        };
        let differs = match base {
            Some(base) => in_use && base.get(first_chunk + n) != Some(&hash),
            None => true,
        };
        hashes.push(hash);
        changed.push(differs);
    }

    let mut blocks = Vec::new();
    if base.is_none() {
        blocks.push(encode_block(job.start, &job.data, level)?);
    } else {
        // Store each run of consecutive changed chunks as one block.
        let mut n = 0;
        while n < changed.len() {
            if !changed[n] {
                n += 1;
                continue;
            }
            let run_start = n;
            while n < changed.len() && changed[n] {
                n += 1;
            }
            let from = run_start * chunk_size;
            let to = (n * chunk_size).min(job.data.len());
            blocks.push(encode_block(
                job.start + from as u64,
                &job.data[from..to],
                level,
            )?);
        }
    }
    Ok(Encoded {
        blocks,
        first_chunk,
        hashes,
    })
}
