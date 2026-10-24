//! Runs the real `volsave` binary on images built with the library.

use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use volsave_core::{BackupOptions, BlockSource, RangeSet, backup};
use volsave_format::{FileHeader, ImageMetadata, ImageWriter, SplitWriter};

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
