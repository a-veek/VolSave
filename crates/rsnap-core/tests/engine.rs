#![allow(clippy::single_range_in_vec_init)] // one-range sets are intended here

use std::fs::{self, File};
use std::io::{self, Cursor};
use std::sync::atomic::AtomicBool;

use rsnap_core::{
    BackupError, BackupOptions, BackupOutput, BlockSource, FileSource, FileTarget, ImageView,
    LayerError, LayeredImage, RangeSet, RestoreError, SingleImage, backup, merge, restore,
};
use rsnap_format::{
    FileHeader, HashList, ImageMetadata, ImageReader, ImageWriter, SourceInfo, UNUSED_CHUNK,
};

const MIB: u64 = 1 << 20;
const KIB: u64 = 1 << 10;

/// A 5 MiB "volume" with distinct content in every MiB.
fn volume_bytes() -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    (0..5 * MIB)
        .map(|i| {
            if i < 2 * MIB {
                (i % 251) as u8
            } else {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            }
        })
        .collect()
}

fn full_header(size: u64) -> FileHeader {
    FileHeader::new(ImageMetadata {
        source: SourceInfo {
            size_bytes: size,
            ..SourceInfo::default()
        },
        ..ImageMetadata::default()
    })
}

fn diff_header(base: &HashList) -> FileHeader {
    let mut header = full_header(base.volume_size);
    header.base_image_id = Some(base.image_id);
    header
}

fn writer(header: FileHeader) -> ImageWriter<Vec<u8>> {
    ImageWriter::new(Vec::new(), header).unwrap()
}

fn options() -> BackupOptions {
    BackupOptions {
        threads: 3,
        ..BackupOptions::default()
    }
}

fn run(
    source: &dyn BlockSource,
    used: &RangeSet,
    header: FileHeader,
    base: Option<&HashList>,
) -> BackupOutput<Vec<u8>> {
    let cancel = AtomicBool::new(false);
    backup(
        source,
        used,
        writer(header),
        base,
        &options(),
        &cancel,
        |_| {},
    )
    .unwrap()
}

fn open(image: &[u8]) -> ImageReader<Cursor<Vec<u8>>> {
    ImageReader::open(Cursor::new(image.to_vec())).unwrap()
}

/// Restores `view` into a zeroed buffer of the volume size.
fn restore_to_vec(view: &mut dyn ImageView, size: u64) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("restored.raw");
    let mut target = FileTarget::new(File::create(&path).unwrap(), size).unwrap();
    restore(view, &mut target, &AtomicBool::new(false), |_| {}).unwrap();
    drop(target);
    fs::read(path).unwrap()
}

fn assert_same_in(used: &RangeSet, left: &[u8], right: &[u8]) {
    for range in used.ranges() {
        let r = range.start as usize..range.end as usize;
        assert!(left[r.clone()] == right[r.clone()], "data differs in {r:?}");
    }
}

#[test]
fn backup_and_restore_copy_used_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let volume = volume_bytes();
    let source_path = dir.path().join("volume.raw");
    fs::write(&source_path, &volume).unwrap();
    let source = FileSource::new(File::open(&source_path).unwrap()).unwrap();

    // Used: the first 1.5 MiB and the last 4 KiB; the rest is "free space".
    let used = RangeSet::from_ranges([0..MIB + MIB / 2, 5 * MIB - 4 * KIB..5 * MIB]);
    let cancel = AtomicBool::new(false);
    let mut reports = 0;
    let output = backup(
        &source,
        &used,
        writer(full_header(5 * MIB)),
        None,
        &options(),
        &cancel,
        |progress| {
            reports += 1;
            assert!(progress.bytes_read <= progress.bytes_total);
        },
    )
    .unwrap();
    assert_eq!(output.stats.blocks, 3);
    assert_eq!(output.stats.bytes_read, used.total_len());
    assert_eq!(reports, 3);

    let mut reader = open(&output.sink);
    assert!(reader.verify(|_, _| {}).is_ok());

    let restored = restore_to_vec(&mut SingleImage::new(reader), 5 * MIB);
    assert_same_in(&used, &restored, &volume);
    // Free space comes back as zeros.
    assert!(
        restored[(MIB + MIB / 2) as usize..(5 * MIB - 4 * KIB) as usize]
            .iter()
            .all(|&b| b == 0)
    );
}

#[test]
fn full_backup_records_chunk_hashes() {
    let source = MemorySource(volume_bytes());
    let used = RangeSet::from_ranges([0..128 * KIB, 3 * MIB..3 * MIB + 10 * KIB]);
    let output = run(&source, &used, full_header(5 * MIB), None);
    let hashes = output.hashes;
    assert_eq!(hashes.len(), 80); // 5 MiB in 64 KiB chunks
    assert_ne!(hashes.get(0), Some(&UNUSED_CHUNK));
    assert_ne!(hashes.get(1), Some(&UNUSED_CHUNK));
    assert_eq!(hashes.get(2), Some(&UNUSED_CHUNK));
    assert_ne!(hashes.get(48), Some(&UNUSED_CHUNK)); // partially used chunk
    assert_eq!(hashes.get(49), Some(&UNUSED_CHUNK));
}

#[test]
fn differential_backup_stores_only_changed_chunks() {
    let mut volume = volume_bytes();
    let used = RangeSet::full(5 * MIB);
    let full = run(
        &MemorySource(volume.clone()),
        &used,
        full_header(5 * MIB),
        None,
    );

    // Change two separate chunks and a run of three chunks.
    volume[100] ^= 1;
    volume[(3 * MIB + 5) as usize] ^= 1;
    for offset in [4 * MIB, 4 * MIB + 64 * KIB, 4 * MIB + 128 * KIB] {
        volume[offset as usize + 7] ^= 1;
    }
    let diff = run(
        &MemorySource(volume.clone()),
        &used,
        diff_header(&full.hashes),
        Some(&full.hashes),
    );
    assert_eq!(diff.stats.blocks, 3);
    assert_eq!(diff.stats.bytes_stored, 5 * 64 * KIB);
    assert_eq!(diff.stats.bytes_read, 5 * MIB);

    let diff_reader = open(&diff.sink);
    assert_eq!(
        diff_reader.header().base_image_id,
        Some(full.hashes.image_id)
    );
    let mut layered = LayeredImage::new(open(&full.sink), diff_reader).unwrap();
    assert_eq!(restore_to_vec(&mut layered, 5 * MIB), volume);
}

#[test]
fn unchanged_volume_gives_empty_differential_image() {
    let volume = MemorySource(volume_bytes());
    let used = RangeSet::from_ranges([0..2 * MIB]);
    let full = run(&volume, &used, full_header(5 * MIB), None);
    let diff = run(
        &volume,
        &used,
        diff_header(&full.hashes),
        Some(&full.hashes),
    );
    assert_eq!(diff.stats.blocks, 0);
    // The new hashes equal the base hashes.
    assert!((0..full.hashes.len()).all(|n| diff.hashes.get(n) == full.hashes.get(n)));
}

#[test]
fn newly_used_and_freed_space_is_handled() {
    let volume = volume_bytes();
    let full = run(
        &MemorySource(volume.clone()),
        &RangeSet::from_ranges([0..MIB]),
        full_header(5 * MIB),
        None,
    );
    // Now the first MiB is free and the second MiB is in use.
    let used_now = RangeSet::from_ranges([MIB..2 * MIB]);
    let diff = run(
        &MemorySource(volume.clone()),
        &used_now,
        diff_header(&full.hashes),
        Some(&full.hashes),
    );
    assert_eq!(diff.stats.bytes_stored, MIB);

    let mut layered = LayeredImage::new(open(&full.sink), open(&diff.sink)).unwrap();
    assert_same_in(&used_now, &restore_to_vec(&mut layered, 5 * MIB), &volume);
}

#[test]
fn merge_produces_equivalent_full_image() {
    let mut volume = volume_bytes();
    let used = RangeSet::from_ranges([0..3 * MIB]);
    let full = run(
        &MemorySource(volume.clone()),
        &used,
        full_header(5 * MIB),
        None,
    );
    volume[(2 * MIB + 3) as usize] ^= 0xff;
    let diff = run(
        &MemorySource(volume.clone()),
        &used,
        diff_header(&full.hashes),
        Some(&full.hashes),
    );

    let layered = LayeredImage::new(open(&full.sink), open(&diff.sink)).unwrap();
    let merged = merge(
        Box::new(layered),
        writer(full_header(5 * MIB)),
        &options(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let merged_reader = open(&merged.sink);
    assert!(!merged_reader.header().is_differential());
    assert_same_in(
        &used,
        &restore_to_vec(&mut SingleImage::new(merged_reader), 5 * MIB),
        &volume,
    );
    // The merged image's hashes describe the current volume: a new diff is empty.
    let next = run(
        &MemorySource(volume),
        &used,
        diff_header(&merged.hashes),
        Some(&merged.hashes),
    );
    assert_eq!(next.stats.blocks, 0);
}

#[test]
fn layering_checks_the_base() {
    let volume = MemorySource(volume_bytes());
    let used = RangeSet::full(MIB);
    let full = run(&volume, &used, full_header(5 * MIB), None);
    let other = run(&volume, &used, full_header(5 * MIB), None);
    let diff = run(
        &volume,
        &used,
        diff_header(&full.hashes),
        Some(&full.hashes),
    );

    assert!(matches!(
        LayeredImage::new(open(&other.sink), open(&diff.sink)),
        Err(LayerError::WrongBase { .. })
    ));
    assert!(matches!(
        LayeredImage::new(open(&full.sink), open(&other.sink)),
        Err(LayerError::NotDifferential)
    ));
}

#[test]
fn differential_backup_rejects_mismatching_base() {
    let volume = MemorySource(volume_bytes());
    let full = run(&volume, &RangeSet::full(MIB), full_header(5 * MIB), None);
    let cancel = AtomicBool::new(false);

    // Header does not name the hash list's image.
    let result = backup(
        &volume,
        &RangeSet::full(MIB),
        writer(full_header(5 * MIB)),
        Some(&full.hashes),
        &options(),
        &cancel,
        |_| {},
    );
    assert!(matches!(result, Err(BackupError::BaseMismatch(_))));

    // Volume size changed.
    let smaller = MemorySource(vec![0; 4 * MIB as usize]);
    let result = backup(
        &smaller,
        &RangeSet::full(MIB),
        writer(diff_header(&full.hashes)),
        Some(&full.hashes),
        &options(),
        &cancel,
        |_| {},
    );
    assert!(matches!(result, Err(BackupError::BaseMismatch(_))));
}

#[test]
fn restore_rejects_small_target() {
    let output = run(
        &MemorySource(vec![1u8; 2 * MIB as usize]),
        &RangeSet::full(2 * MIB),
        full_header(2 * MIB),
        None,
    );
    let dir = tempfile::tempdir().unwrap();
    let mut target = FileTarget::new(File::create(dir.path().join("t")).unwrap(), MIB).unwrap();
    assert!(matches!(
        restore(
            &mut SingleImage::new(open(&output.sink)),
            &mut target,
            &AtomicBool::new(false),
            |_| {}
        ),
        Err(RestoreError::TargetTooSmall { .. })
    ));
}

#[test]
fn read_errors_abort_the_backup() {
    let cancel = AtomicBool::new(false);
    let result = backup(
        &FailingSource,
        &RangeSet::full(4 * MIB),
        writer(full_header(4 * MIB)),
        None,
        &options(),
        &cancel,
        |_| {},
    );
    assert!(matches!(result, Err(BackupError::Read { offset, .. }) if offset == 2 * MIB));
}

#[test]
fn cancel_stops_the_backup() {
    let source = MemorySource(vec![0u8; 4 * MIB as usize]);
    let cancel = AtomicBool::new(true);
    let result = backup(
        &source,
        &RangeSet::full(4 * MIB),
        writer(full_header(4 * MIB)),
        None,
        &options(),
        &cancel,
        |_| {},
    );
    assert!(matches!(result, Err(BackupError::Cancelled)));
}

#[test]
fn empty_used_set_gives_empty_image() {
    let output = run(
        &MemorySource(vec![0u8; MIB as usize]),
        &RangeSet::new(),
        full_header(MIB),
        None,
    );
    assert_eq!(output.stats.blocks, 0);
    assert!(open(&output.sink).blocks().is_empty());
}

struct MemorySource(Vec<u8>);

impl BlockSource for MemorySource {
    fn size(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let start = offset as usize;
        buf.copy_from_slice(&self.0[start..start + buf.len()]);
        Ok(())
    }
}

/// Fails for everything at or after 2 MiB.
struct FailingSource;

impl BlockSource for FailingSource {
    fn size(&self) -> u64 {
        4 * MIB
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if offset >= 2 * MIB {
            return Err(io::Error::other("bad sector"));
        }
        buf.fill(0xaa);
        Ok(())
    }
}
