use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rsnap_core::{BackupOptions, FileTarget, Progress, merge as run_merge, restore};
use rsnap_format::{DEFAULT_ZSTD_LEVEL, FileHeader, ImageMetadata, ImageWriter, SplitWriter};

#[macro_use]
mod log;
mod images;
#[cfg(windows)]
mod windows_cmds;

use images::{hash_path, open_image, open_view, write_hash_list};
use log::LogLevel;

/// Set by Ctrl+C; long operations stop at the next block.
static CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Parser)]
#[command(name = "rsnap", version, about = "Disk and volume imaging for Windows")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Log file. Without it, commands working on an image log next to it
    /// (c.dimg -> c.log); runs are appended
    #[arg(long, global = true, value_name = "FILE", num_args = 0..=1, conflicts_with = "no_log")]
    log: Option<Option<PathBuf>>,
    /// Do not write a log file
    #[arg(long, global = true)]
    no_log: bool,
    /// How much detail to log
    #[arg(long, global = true, value_enum, default_value_t = LogLevel::Info)]
    log_level: LogLevel,
}

#[derive(Subcommand)]
enum Command {
    /// List volumes and disks
    List,
    /// Back up a volume into an image
    Backup {
        /// Volume to back up: C:, a folder mount point or \\?\Volume{GUID}\
        source: String,
        /// Image file to create
        image: PathBuf,
        /// Make a differential image against the full image this hash list belongs to
        #[arg(long, value_name = "HASHFILE")]
        diff: Option<PathBuf>,
        /// With --diff: make a full backup if the hash list does not exist
        #[arg(long, requires = "diff")]
        full_if_hash_missing: bool,
        /// Where to write the hash list (default for full images: IMAGE with .dhsh extension)
        #[arg(long, value_name = "FILE", conflicts_with = "no_hash")]
        hash: Option<PathBuf>,
        /// Do not write a hash list
        #[arg(long)]
        no_hash: bool,
        #[command(flatten)]
        common: WriteOptions,
        /// Read the live volume instead of a VSS snapshot
        #[arg(long)]
        no_vss: bool,
        /// Also save pagefile.sys, swapfile.sys and hiberfil.sys
        #[arg(long)]
        keep_system_files: bool,
        /// Which entries of the Windows list FilesNotToBackup to leave out
        #[arg(long, value_enum, default_value_t = ExclusionChoice::Safe)]
        files_not_to_backup: ExclusionChoice,
        /// Comment stored in the image
        #[arg(long)]
        comment: Option<String>,
    },
    /// Restore an image onto a volume (overwrites the volume)
    Restore {
        image: PathBuf,
        /// Target volume: D:, a folder mount point or \\?\Volume{GUID}\
        target: String,
        /// Full image of a differential image (default: searched in the image's folder)
        #[arg(long)]
        base: Option<PathBuf>,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Combine a differential image and its full image into a new full image
    Merge {
        /// Differential image
        image: PathBuf,
        /// New full image to create
        output: PathBuf,
        #[arg(long)]
        base: Option<PathBuf>,
        #[command(flatten)]
        common: WriteOptions,
    },
    /// Show which files a backup of the volume leaves out (Windows FilesNotToBackup list)
    Exclusions {
        /// Volume: C:, a folder mount point or \\?\Volume{GUID}\
        volume: String,
    },
    /// Show what an image contains
    Info { image: PathBuf },
    /// Read every block and check its checksum
    Verify { image: PathBuf },
    /// Write the image content into a raw file of the original volume size
    Extract {
        image: PathBuf,
        output: PathBuf,
        #[arg(long)]
        base: Option<PathBuf>,
    },
}

/// Which `FilesNotToBackup` entries to apply.
#[derive(clap::ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ExclusionChoice {
    /// Only entries whose files are harmless as zeros after a restore
    /// (temp files, dumps, error reports, page file, shadow copy storage)
    Safe,
    /// Every entry, including databases such as the Windows Update store
    All,
    /// None; save every file
    None,
}

/// Options for commands that write an image.
#[derive(clap::Args, Clone)]
pub(crate) struct WriteOptions {
    /// zstd level, 1 (fastest) to 22 (smallest)
    #[arg(long, default_value_t = DEFAULT_ZSTD_LEVEL, value_parser = clap::value_parser!(i32).range(1..=22))]
    level: i32,
    /// Compression threads (default: all CPU cores)
    #[arg(long)]
    threads: Option<usize>,
    /// Split the image into files of this many MiB
    #[arg(long, value_name = "MIB")]
    split: Option<u64>,
}

impl WriteOptions {
    pub(crate) fn backup_options(&self) -> BackupOptions {
        let mut options = BackupOptions {
            zstd_level: self.level,
            ..BackupOptions::default()
        };
        if let Some(threads) = self.threads {
            options.threads = threads;
        }
        options
    }

    pub(crate) fn create_sink(&self, path: &Path) -> Result<SplitWriter> {
        SplitWriter::create(path, self.split.map(|mib| mib << 20))
            .with_context(|| format!("cannot create {}", path.display()))
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let _ = ctrlc::set_handler(|| CANCEL.store(true, Ordering::Relaxed));
    let debug = cli.log_level == LogLevel::Debug || std::env::var_os("RSNAP_DEBUG").is_some();
    log::set_level(if debug {
        LogLevel::Debug
    } else {
        LogLevel::Info
    });
    #[cfg(windows)]
    rsnap_win::set_debug_hook(log::debug);

    let log_file = match (&cli.log, cli.no_log) {
        (_, true) => None,
        (Some(Some(path)), _) => Some(path.clone()),
        _ => default_log_path(&cli.command),
    };
    if let Some(path) = &log_file
        && let Err(error) = log::open(path)
    {
        eprintln!("warning: cannot write the log {}: {error}", path.display());
    }
    let result = run(cli.command);
    log::finish(&result)
}

/// Commands working on an image log next to it: `c.dimg` -> `c.log`.
fn default_log_path(command: &Command) -> Option<PathBuf> {
    let image = match command {
        Command::Backup { image, .. }
        | Command::Restore { image, .. }
        | Command::Verify { image }
        | Command::Extract { image, .. } => image,
        Command::Merge { output, .. } => output,
        Command::List | Command::Exclusions { .. } | Command::Info { .. } => return None,
    };
    Some(image.with_extension("log"))
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Info { image } => info(&image),
        Command::Verify { image } => verify(&image),
        Command::Extract {
            image,
            output,
            base,
        } => extract(&image, &output, base.as_deref()),
        Command::Merge {
            image,
            output,
            base,
            common,
        } => merge(&image, &output, base.as_deref(), &common),
        #[cfg(windows)]
        Command::List => windows_cmds::list(),
        #[cfg(windows)]
        Command::Exclusions { volume } => windows_cmds::exclusions(&volume),
        #[cfg(windows)]
        Command::Backup {
            source,
            image,
            diff,
            full_if_hash_missing,
            hash,
            no_hash,
            common,
            no_vss,
            keep_system_files,
            files_not_to_backup,
            comment,
        } => windows_cmds::backup(windows_cmds::BackupArgs {
            source,
            image,
            diff,
            full_if_hash_missing,
            hash,
            no_hash,
            common,
            no_vss,
            keep_system_files,
            files_not_to_backup,
            comment,
        }),
        #[cfg(windows)]
        Command::Restore {
            image,
            target,
            base,
            yes,
        } => windows_cmds::restore(&image, &target, base.as_deref(), yes),
        #[cfg(not(windows))]
        Command::List
        | Command::Exclusions { .. }
        | Command::Backup { .. }
        | Command::Restore { .. } => {
            anyhow::bail!("this command is only available on Windows")
        }
    }
}

fn info(path: &Path) -> Result<()> {
    let reader = open_image(path)?;
    let header = reader.header();
    let meta = &header.metadata;
    let source = &meta.source;
    println!("Image:        {}", path.display());
    println!("Id:           {}", header.image_id);
    match header.base_image_id {
        Some(base_id) => {
            println!("Type:         differential");
            let base = images::find_base(path, header, base_id)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "not found".into());
            println!("Full image:   {base_id} ({base})");
        }
        None => println!("Type:         full"),
    }
    println!("Created by:   {}", meta.tool);
    println!("Created:      {} (Unix time)", meta.created_unix);
    if let Some(comment) = &meta.comment {
        println!("Comment:      {comment}");
    }
    println!("Source:       {} ({:?})", source.device, source.kind);
    if let Some(host) = &source.hostname {
        println!("Computer:     {host}");
    }
    println!(
        "File system:  {} {}",
        source.filesystem.as_deref().unwrap_or("-"),
        source
            .label
            .as_deref()
            .map(|l| format!("\"{l}\""))
            .unwrap_or_default()
    );
    println!("Volume size:  {}", human(source.size_bytes));
    println!(
        "Saved data:   {} in {} blocks",
        human(reader.data_bytes()),
        reader.blocks().len()
    );
    Ok(())
}

/// Checks the image and, for a differential image, also its full image.
fn verify(path: &Path) -> Result<()> {
    let reader = open_image(path)?;
    let header = reader.header().clone();
    verify_one(path, reader)?;
    if let Some(base_id) = header.base_image_id {
        let base = images::find_base(path, &header, base_id)?;
        verify_one(&base, open_image(&base)?)?;
    }
    Ok(())
}

fn verify_one(path: &Path, mut reader: images::Reader) -> Result<()> {
    let total = reader.data_bytes();
    let blocks = reader.blocks().to_vec();
    let mut printer = ProgressPrinter::new("Verifying");
    let mut done = 0u64;
    let report = reader.verify(|n, _| {
        done += u64::from(blocks[n - 1].raw_len);
        printer.update(&Progress {
            bytes_read: done,
            bytes_total: total,
            bytes_written: 0,
        });
    });
    printer.finish();
    for (block, error) in &report.errors {
        eprintln!("bad block at offset {}: {error}", block.offset);
        log::write_line(
            "ERROR",
            &format!("bad block at offset {}: {error}", block.offset),
        );
    }
    anyhow::ensure!(
        report.is_ok(),
        "{}: {} of {} blocks are damaged",
        path.display(),
        report.errors.len(),
        blocks.len()
    );
    say!(
        "{}: OK, {} blocks, {} checked",
        path.display(),
        report.blocks_ok,
        human(report.bytes_ok)
    );
    Ok(())
}

fn extract(path: &Path, output: &Path, base: Option<&Path>) -> Result<()> {
    let mut image = open_view(path, base)?;
    let size = image.view.source_size();
    let file =
        File::create(output).with_context(|| format!("cannot create {}", output.display()))?;
    let mut target = FileTarget::new(file, size)?;
    let mut printer = ProgressPrinter::new("Extracting");
    let stats = restore(image.view.as_mut(), &mut target, &CANCEL, |p| {
        printer.update(p)
    })?;
    printer.finish();
    say!(
        "Wrote {} to {}",
        human(stats.bytes_written),
        output.display()
    );
    Ok(())
}

fn merge(path: &Path, output: &Path, base: Option<&Path>, common: &WriteOptions) -> Result<()> {
    let image = open_view(path, base)?;
    anyhow::ensure!(
        image.base_path.is_some(),
        "{} is not a differential image",
        path.display()
    );
    let header = FileHeader::new(ImageMetadata {
        tool: tool_name(),
        created_unix: now_unix(),
        comment: image.header.metadata.comment.clone(),
        source: image.header.metadata.source.clone(),
        base_image_name: None,
    });
    let writer = ImageWriter::new(common.create_sink(output)?, header)?;
    let mut printer = ProgressPrinter::new("Merging");
    let result = run_merge(image.view, writer, &common.backup_options(), &CANCEL, |p| {
        printer.update(p)
    })?;
    printer.finish();
    result.sink.finish()?;
    let hashes = hash_path(output);
    write_hash_list(&hashes, &result.hashes)?;
    say!(
        "Done: {} written to {}, hash list {}",
        human(result.stats.bytes_written),
        output.display(),
        hashes.display()
    );
    Ok(())
}

pub(crate) fn tool_name() -> String {
    format!("rsnap {}", env!("CARGO_PKG_VERSION"))
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// One status line on stderr, refreshed at most twice per second.
pub(crate) struct ProgressPrinter {
    label: &'static str,
    started: Instant,
    last: Option<Instant>,
    /// Most recent state, shown unthrottled by `finish`.
    latest: Option<Progress>,
    /// Next percentage to record in the log.
    next_logged: u32,
}

impl ProgressPrinter {
    pub(crate) fn new(label: &'static str) -> Self {
        Self {
            label,
            started: Instant::now(),
            last: None,
            latest: None,
            next_logged: 10,
        }
    }

    pub(crate) fn update(&mut self, progress: &Progress) {
        self.latest = Some(*progress);
        let percent = percent(progress);
        while self.next_logged <= 100 && percent >= f64::from(self.next_logged) {
            log::write_line(
                "INFO",
                &format!(
                    "{} {}%: {} of {}",
                    self.label,
                    self.next_logged,
                    human(progress.bytes_read),
                    human(progress.bytes_total)
                ),
            );
            self.next_logged += 10;
        }
        if self
            .last
            .is_some_and(|last| last.elapsed() < Duration::from_millis(500))
        {
            return;
        }
        self.last = Some(Instant::now());
        self.print(progress);
    }

    fn print(&self, progress: &Progress) {
        let percent = percent(progress);
        let seconds = self.started.elapsed().as_secs_f64().max(0.001);
        let rate = progress.bytes_read as f64 / seconds;
        eprint!(
            "\r{} {:5.1}%  {} / {}  {}/s   ",
            self.label,
            percent,
            human(progress.bytes_read),
            human(progress.bytes_total),
            human(rate as u64)
        );
    }

    pub(crate) fn finish(&self) {
        if let Some(latest) = &self.latest {
            self.print(latest);
            eprintln!();
        }
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

fn percent(progress: &Progress) -> f64 {
    match progress.bytes_total {
        0 => 100.0,
        total => progress.bytes_read as f64 * 100.0 / total as f64,
    }
}

pub(crate) fn human(bytes: u64) -> String {
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
