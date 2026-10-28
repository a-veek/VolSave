use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use volsave_core::BackupOptions;
use volsave_format::{DEFAULT_ZSTD_LEVEL, SplitWriter};

/// Name and version written into images and logs.
pub fn tool_name() -> String {
    format!("VolSave {}", env!("CARGO_PKG_VERSION"))
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `1536` -> `1.50 KiB`.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// How an image is written.
#[derive(Debug, Clone)]
pub struct WriteOptions {
    /// zstd level, 1 to 22.
    pub zstd_level: i32,
    /// Compression threads; `None` = all CPU cores.
    pub threads: Option<usize>,
    /// Split the image into files of this many bytes.
    pub split_bytes: Option<u64>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            zstd_level: DEFAULT_ZSTD_LEVEL,
            threads: None,
            split_bytes: None,
        }
    }
}

impl WriteOptions {
    pub fn backup_options(&self) -> BackupOptions {
        let mut options = BackupOptions {
            zstd_level: self.zstd_level,
            ..BackupOptions::default()
        };
        if let Some(threads) = self.threads {
            options.threads = threads;
        }
        options
    }

    pub fn create_sink(&self, path: &Path) -> Result<SplitWriter> {
        SplitWriter::create(path, self.split_bytes)
            .with_context(|| format!("cannot create {}", path.display()))
    }
}
