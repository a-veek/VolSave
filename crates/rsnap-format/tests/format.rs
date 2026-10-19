use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};

use rsnap_format::{
    BlockEncoding, DEFAULT_ZSTD_LEVEL, Error, FileHeader, HashList, ImageMetadata, ImageReader,
    ImageWriter, SourceInfo, SourceKind, SplitReader, SplitWriter, UNUSED_CHUNK, chunk_hash,
    encode_block, part_path,
};

const BLOCK: usize = 64 << 10;

fn metadata() -> ImageMetadata {
    ImageMetadata {
        tool: "rsnap-test".into(),
        created_unix: 1_760_000_000,
        comment: Some("Testimage".into()),
        source: SourceInfo {
            kind: SourceKind::Volume,
            device: r"\\?\Volume{00000000-0000-0000-0000-000000000000}\".into(),
            size_bytes: 8 * BLOCK as u64,
            sector_size: 512,
            filesystem: Some("NTFS".into()),
            ..SourceInfo::default()
        },
        base_image_name: None,
    }
}

fn header() -> FileHeader {
    let mut header = FileHeader::new(metadata());
    header.block_size = BLOCK as u32;
    header
}

/// Deterministic incompressible bytes.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn text(len: usize) -> Vec<u8> {
    b"Drive image test data. "
        .iter()
        .copied()
        .cycle()
        .take(len)
        .collect()
}

/// Volume layout used by most tests: (offset, data); gaps are unused space.
fn sample_blocks() -> Vec<(u64, Vec<u8>)> {
    vec![
        (0, text(BLOCK)),
        (BLOCK as u64, vec![0; BLOCK]),
        (3 * BLOCK as u64, noise(BLOCK, 7)),
        (4 * BLOCK as u64, text(1000)),
    ]
}

fn write_image(blocks: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut writer = ImageWriter::new(Vec::new(), header()).unwrap();
    for (offset, data) in blocks {
        writer
            .encode_and_write(*offset, data, DEFAULT_ZSTD_LEVEL)
            .unwrap();
    }
    writer.finish().unwrap()
}

#[test]
fn roundtrip_restores_every_block() {
    let blocks = sample_blocks();
    let image = write_image(&blocks);
    let mut reader = ImageReader::open(Cursor::new(image)).unwrap();

    assert!(!reader.index_rebuilt());
    assert_eq!(reader.header().metadata, metadata());
    assert_eq!(reader.blocks().len(), blocks.len());
    for (entry, (offset, data)) in reader.blocks().to_vec().iter().zip(&blocks) {
        assert_eq!(entry.offset, *offset);
        assert_eq!(&reader.read_block(entry).unwrap(), data);
    }
    assert!(reader.verify(|_, _| {}).is_ok());
}

#[test]
fn encoding_matches_content() {
    let level = DEFAULT_ZSTD_LEVEL;
    assert_eq!(
        encode_block(0, &text(BLOCK), level)
            .unwrap()
            .header
            .encoding,
        BlockEncoding::Zstd
    );
    assert_eq!(
        encode_block(0, &vec![0; BLOCK], level)
            .unwrap()
            .header
            .encoding,
        BlockEncoding::Zero
    );
    assert_eq!(
        encode_block(0, &noise(BLOCK, 3), level)
            .unwrap()
            .header
            .encoding,
        BlockEncoding::Stored
    );
    assert!(encode_block(0, &[], level).is_err());
}

#[test]
fn compressible_data_shrinks() {
    let blocks = vec![(0, text(BLOCK)), (BLOCK as u64, vec![0; BLOCK])];
    let image = write_image(&blocks);
    assert!(image.len() < BLOCK / 4, "image is {} bytes", image.len());
}

#[test]
fn find_block_maps_offsets() {
    let reader = ImageReader::open(Cursor::new(write_image(&sample_blocks()))).unwrap();
    assert_eq!(reader.find_block(0).unwrap().offset, 0);
    assert_eq!(reader.find_block(BLOCK as u64 - 1).unwrap().offset, 0);
    assert_eq!(
        reader.find_block(BLOCK as u64).unwrap().offset,
        BLOCK as u64
    );
    assert!(reader.find_block(2 * BLOCK as u64).is_none());
    assert_eq!(
        reader.find_block(4 * BLOCK as u64 + 999).unwrap().raw_len,
        1000
    );
    assert!(reader.find_block(4 * BLOCK as u64 + 1000).is_none());
}

#[test]
fn missing_index_is_rebuilt_by_scanning() {
    let blocks = sample_blocks();
    let mut image = write_image(&blocks);
    // Simulate an aborted backup: drop index and footer.
    let last_block_end = {
        let mut reader = ImageReader::open(Cursor::new(image.clone())).unwrap();
        let last = *reader.blocks().last().unwrap();
        reader.read_block(&last).unwrap();
        reader.into_inner().position()
    };
    image.truncate(last_block_end as usize + 10);

    let mut reader = ImageReader::open(Cursor::new(image)).unwrap();
    assert!(reader.index_rebuilt());
    assert_eq!(reader.blocks().len(), blocks.len());
    assert!(reader.verify(|_, _| {}).is_ok());
}

#[test]
fn truncated_last_block_is_dropped_on_rebuild() {
    let blocks = sample_blocks();
    let image = write_image(&blocks);
    let reader = ImageReader::open(Cursor::new(image.clone())).unwrap();
    let last = *reader.blocks().last().unwrap();
    // Header plus one payload byte: the payload is cut short.
    let truncated = image[..last.file_pos as usize + 61].to_vec();

    let reader = ImageReader::open(Cursor::new(truncated)).unwrap();
    assert!(reader.index_rebuilt());
    assert_eq!(reader.blocks().len(), blocks.len() - 1);
}

#[test]
fn corrupted_payload_is_detected() {
    let image = write_image(&sample_blocks());
    let reader = ImageReader::open(Cursor::new(image.clone())).unwrap();
    // The noise block is stored uncompressed; flip a byte in its payload.
    let entry = reader.blocks()[2];
    let mut damaged = image;
    damaged[entry.file_pos as usize + 60 + 100] ^= 0xff;

    let mut reader = ImageReader::open(Cursor::new(damaged)).unwrap();
    assert!(matches!(
        reader.read_block(&entry),
        Err(Error::BlockHashMismatch { offset }) if offset == entry.offset
    ));
    let report = reader.verify(|_, _| {});
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.blocks_ok, 3);
}

#[test]
fn corrupted_header_is_rejected() {
    let mut image = write_image(&sample_blocks());
    image[20] ^= 1;
    assert!(matches!(
        ImageReader::open(Cursor::new(image)),
        Err(Error::HeaderCorrupt)
    ));

    let mut image = write_image(&sample_blocks());
    image[0] = b'X';
    assert!(matches!(
        ImageReader::open(Cursor::new(image)),
        Err(Error::BadMagic)
    ));
}

#[test]
fn blocks_must_ascend() {
    let mut writer = ImageWriter::new(Vec::new(), header()).unwrap();
    writer
        .encode_and_write(BLOCK as u64, &text(BLOCK), 1)
        .unwrap();
    let overlapping = writer.encode_and_write(BLOCK as u64 + 1, &text(10), 1);
    assert!(matches!(overlapping, Err(Error::OffsetOrder { .. })));
    let too_large = writer.encode_and_write(4 * BLOCK as u64, &text(BLOCK + 1), 1);
    assert!(matches!(too_large, Err(Error::InvalidArgument(_))));
}

#[test]
fn differential_header_keeps_base_id() {
    let base = header();
    let mut diff = header();
    diff.base_image_id = Some(base.image_id);
    let writer = ImageWriter::new(Vec::new(), diff.clone()).unwrap();
    let image = writer.finish().unwrap();

    let reader = ImageReader::open(Cursor::new(image)).unwrap();
    assert!(reader.header().is_differential());
    assert_eq!(reader.header().base_image_id, Some(base.image_id));
    assert!(reader.blocks().is_empty());
}

#[test]
fn split_image_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("c.dimg");
    let blocks = sample_blocks();
    let sink = SplitWriter::create(&base, Some(50_000)).unwrap();
    let mut writer = ImageWriter::new(sink, header()).unwrap();
    for (offset, data) in &blocks {
        writer
            .encode_and_write(*offset, data, DEFAULT_ZSTD_LEVEL)
            .unwrap();
    }
    let parts = writer.finish().unwrap().finish().unwrap();
    assert!(parts >= 2, "expected several parts, got {parts}");

    let source = SplitReader::open(&base).unwrap();
    assert_eq!(source.part_count(), parts as usize);
    let mut reader = ImageReader::open(source).unwrap();
    assert!(!reader.index_rebuilt());
    for (entry, (_, data)) in reader.blocks().to_vec().iter().zip(&blocks) {
        assert_eq!(&reader.read_block(entry).unwrap(), data);
    }
}

#[test]
fn stale_parts_of_an_older_image_are_removed() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("c.dimg");
    fs::write(part_path(&base, 2), b"stale").unwrap();
    fs::write(part_path(&base, 3), b"stale").unwrap();

    let writer = ImageWriter::new(SplitWriter::create(&base, None).unwrap(), header()).unwrap();
    assert_eq!(writer.finish().unwrap().finish().unwrap(), 1);
    assert!(!part_path(&base, 2).exists());
    assert!(!part_path(&base, 3).exists());
    assert!(ImageReader::open(SplitReader::open(&base).unwrap()).is_ok());
}

#[test]
fn split_reader_seeks_across_parts() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("x.dimg");
    let data = noise(10_000, 5);
    let mut sink = SplitWriter::create(&base, Some(3_000)).unwrap();
    sink.write_all(&data).unwrap();
    assert_eq!(sink.finish().unwrap(), 4);

    let mut source = SplitReader::open(&base).unwrap();
    assert_eq!(source.len(), 10_000);
    source.seek(SeekFrom::Start(2_990)).unwrap();
    let mut buf = vec![0u8; 3_020];
    source.read_exact(&mut buf).unwrap();
    assert_eq!(buf, data[2_990..6_010]);
    assert_eq!(source.seek(SeekFrom::End(-10)).unwrap(), 9_990);
    assert!(source.seek(SeekFrom::Current(-20_000)).is_err());
}

#[test]
fn hash_list_roundtrip() {
    let id = header().image_id;
    let chunk = 4096u32;
    let mut list = HashList::new(id, chunk, 10 * 4096 + 100).unwrap();
    assert_eq!(list.len(), 11);

    let data = text(3 * 4096);
    list.record(2 * 4096, &data).unwrap();
    list.record(10 * 4096, &text(100)).unwrap();
    assert!(list.record(4096 + 1, &data).is_err());
    assert!(list.record(10 * 4096, &text(5000)).is_err());

    let mut file = Vec::new();
    list.write_to(&mut file).unwrap();
    let loaded = HashList::read_from(file.as_slice()).unwrap();
    assert_eq!(loaded, list);
    assert_eq!(loaded.get(0), Some(&UNUSED_CHUNK));
    assert_eq!(loaded.get(2), Some(&chunk_hash(&data[..4096])));

    let mut damaged = file.clone();
    damaged[60] ^= 1;
    assert!(matches!(
        HashList::read_from(damaged.as_slice()),
        Err(Error::HashListCorrupt)
    ));
    assert!(HashList::read_from(&file[..file.len() - 1]).is_err());
}

#[test]
fn image_on_disk_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.dimg");
    let file = File::create(&path).unwrap();
    let mut writer = ImageWriter::new(file, header()).unwrap();
    writer
        .encode_and_write(0, &text(BLOCK), DEFAULT_ZSTD_LEVEL)
        .unwrap();
    writer.finish().unwrap().sync_all().unwrap();

    // Appended garbage after the footer invalidates the footer; the scan still finds the block.
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"garbage").unwrap();
    drop(file);

    let mut reader = ImageReader::open(File::open(&path).unwrap()).unwrap();
    assert!(reader.index_rebuilt());
    assert_eq!(reader.blocks().len(), 1);
    assert!(reader.verify(|_, _| {}).is_ok());
}
