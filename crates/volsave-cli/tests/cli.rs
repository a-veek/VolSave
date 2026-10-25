//! Runs the real `volsave` binary on images built with the library.

use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use volsave_core::{BackupOptions, BlockSource, RangeSet, backup};
use volsave_format::{
    DiskSummary, FileHeader, ImageMetadata, ImageWriter, PartitionSummary, SourceInfo, SourceKind,
    SplitWriter,
};

struct Pattern;

impl BlockSource for Pattern {
    fn size(&self) -> u64 {
        3 << 20
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        for (i, byte) in buf.iter_mut().enumerate() {
            *byte = ((offset as usize + i) % 251) as u8;
        }
        Ok(())
    }
}

fn make_image(path: &Path) {
    let writer = ImageWriter::new(
        SplitWriter::create(path, None).unwrap(),
        FileHeader::new(ImageMetadata::default()),
    )
    .unwrap();
    let output = backup(
        &Pattern,
        &RangeSet::full(3 << 20),
        writer,
        None,
        &BackupOptions::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    output.sink.finish().unwrap();
}

fn volsave(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_volsave"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn verify_logs_next_to_the_image() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("c.dimg");
    make_image(&image);
    let image = image.to_str().unwrap();

    let first = volsave(&["verify", image]);
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    volsave(&["verify", image]);

    let log = fs::read_to_string(dir.path().join("c.log")).unwrap();
    assert_eq!(log.matches("=====").count(), 4, "two runs appended:\n{log}");
    assert_eq!(log.matches("result: OK").count(), 2);
    assert!(log.contains("Verifying 50%"));
    assert!(log.contains("Verifying 100%"));
    assert!(log.contains("OK, 3 blocks"));
    // Every log line after the header starts with a timestamp.
    let line = log.lines().find(|l| l.contains("result: OK")).unwrap();
    assert!(line.chars().take(4).all(|c| c.is_ascii_digit()), "{line}");
}

#[test]
fn damaged_image_fails_with_exit_code_1_and_logs_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("broken.dimg");
    make_image(&image);
    // Flip a byte inside the first block's payload.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .read(true)
        .open(&image)
        .unwrap();
    let len = file.metadata().unwrap().len();
    file.seek(SeekFrom::Start(len / 3)).unwrap();
    file.write_all(&[0xff; 8]).unwrap();
    drop(file);

    let output = volsave(&["verify", image.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let log = fs::read_to_string(dir.path().join("broken.log")).unwrap();
    assert!(log.contains("ERROR"), "{log}");
    assert!(log.contains("result: FAILED"));
}

#[test]
fn log_options_choose_or_suppress_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("d.dimg");
    make_image(&image);
    let image = image.to_str().unwrap();

    let custom = dir.path().join("custom.txt");
    volsave(&["verify", image, "--log", custom.to_str().unwrap()]);
    assert!(custom.exists());
    assert!(!dir.path().join("d.log").exists());

    volsave(&["--no-log", "verify", image]);
    assert!(!dir.path().join("d.log").exists());

    // `--log` without a file name means the default next to the image.
    volsave(&["verify", image, "--log"]);
    assert!(dir.path().join("d.log").exists());

    // `info` only reads and does not log by default.
    volsave(&["info", image]);
    let _ = File::open(dir.path().join("d.log")).unwrap();
    assert!(
        !fs::read_to_string(dir.path().join("d.log"))
            .unwrap()
            .contains("info")
    );
}

/// A 4 MiB MBR disk with two partitions: 1 MiB at 1 MiB and 1 MiB at 2 MiB.
struct MbrDisk(Vec<u8>);

impl MbrDisk {
    fn new() -> Self {
        let mut disk = vec![0u8; 4 << 20];
        for (slot, start) in [(0usize, 2048u32), (1, 4096)] {
            let e = &mut disk[446 + slot * 16..][..16];
            e[4] = 0x07;
            e[8..12].copy_from_slice(&start.to_le_bytes());
            e[12..16].copy_from_slice(&2048u32.to_le_bytes());
        }
        disk[510] = 0x55;
        disk[511] = 0xAA;
        for (i, byte) in disk[1 << 20..3 << 20].iter_mut().enumerate() {
            *byte = (i % 241) as u8 | 1;
        }
        Self(disk)
    }
}

impl BlockSource for MbrDisk {
    fn size(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        buf.copy_from_slice(&self.0[offset as usize..offset as usize + buf.len()]);
        Ok(())
    }
}

#[test]
fn disk_images_show_partitions_and_extract_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.dimg");
    let disk = MbrDisk::new();
    let metadata = ImageMetadata {
        source: SourceInfo {
            kind: SourceKind::Disk,
            device: r"\\.\PhysicalDrive1".into(),
            size_bytes: disk.size(),
            sector_size: 512,
            ..SourceInfo::default()
        },
        disk: Some(DiskSummary {
            style: "MBR".into(),
            disk_id: "00000000".into(),
            model: Some("Test disk".into()),
            partitions: vec![PartitionSummary {
                number: 2,
                offset: 2 << 20,
                length: 1 << 20,
                kind: "NTFS/exFAT".into(),
                method: "all sectors".into(),
                ..PartitionSummary::default()
            }],
        }),
        ..ImageMetadata::default()
    };
    let writer = ImageWriter::new(
        SplitWriter::create(&path, None).unwrap(),
        FileHeader::new(metadata),
    )
    .unwrap();
    let output = backup(
        &disk,
        &RangeSet::full(3 << 20),
        writer,
        None,
        &BackupOptions::default(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    output.sink.finish().unwrap();
    let image = path.to_str().unwrap();

    let info = volsave(&["info", image]);
    let text = String::from_utf8_lossy(&info.stdout);
    assert!(text.contains("Disk:         MBR 00000000"), "{text}");
    assert!(text.contains("#2"), "{text}");

    let part = dir.path().join("p2.img");
    let extract = volsave(&["extract", image, part.to_str().unwrap(), "--partition", "2"]);
    assert_eq!(extract.status.code(), Some(0), "{extract:?}");
    assert!(fs::read(&part).unwrap() == disk.0[2 << 20..3 << 20]);

    let missing = volsave(&["extract", image, part.to_str().unwrap(), "--partition", "3"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no partition 3"));
}
