//! Console output plus an optional log file with timestamps.
//!
//! Every message goes to the console as before and, if a log is open, also
//! into the log file. The log is appended to, so one file collects the
//! history of an image.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LogLevel {
    /// Progress, results, warnings and errors
    Info,
    /// Also internal details, e.g. every VSS call
    Debug,
}

struct State {
    file: Option<(PathBuf, File)>,
    level: LogLevel,
    warnings: usize,
}

static STATE: Mutex<State> = Mutex::new(State {
    file: None,
    level: LogLevel::Info,
    warnings: 0,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    // A panic while logging must not silence all later messages.
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn set_level(level: LogLevel) {
    state().level = level;
}

/// Opens (appends to) the log file and writes a header for this run.
pub(crate) fn open(path: &Path) -> io::Result<()> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    state().file = Some((path.to_path_buf(), file));
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    write_line("", "");
    write_line(
        "",
        &format!("===== {} {} =====", crate::tool_name(), arguments.join(" ")),
    );
    let computer = std::env::var("COMPUTERNAME").unwrap_or_default();
    let user = std::env::var("USERNAME").unwrap_or_default();
    if !computer.is_empty() || !user.is_empty() {
        write_line("INFO", &format!("computer {computer}, user {user}"));
    }
    Ok(())
}

pub(crate) fn path() -> Option<PathBuf> {
    state().file.as_ref().map(|(path, _)| path.clone())
}

/// Writes one line into the log file only.
pub(crate) fn write_line(level: &str, message: &str) {
    let mut state = state();
    if let Some((_, file)) = state.file.as_mut() {
        let line = if level.is_empty() {
            format!("{message}\n")
        } else {
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            format!("{now} {level:<5} {message}\n")
        };
        // Losing a log line must not abort a backup.
        let _ = file.write_all(line.as_bytes());
    }
}

/// A result line: stdout and log.
pub(crate) fn say(message: &str) {
    println!("{message}");
    write_line("INFO", message);
}

/// A status note: stderr and log.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn note(message: &str) {
    eprintln!("{message}");
    write_line("INFO", message);
}

pub(crate) fn warn(message: &str) {
    state().warnings += 1;
    eprintln!("warning: {message}");
    write_line("WARN", message);
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn debug(message: &str) {
    if state().level >= LogLevel::Debug {
        eprintln!("[debug] {message}");
        write_line("DEBUG", message);
    }
}

/// Logs how the run ended and returns the exit code:
/// 0 = success, 1 = error, 2 = success with warnings.
pub(crate) fn finish(result: &anyhow::Result<()>) -> ExitCode {
    let warnings = state().warnings;
    match result {
        Ok(()) if warnings == 0 => {
            write_line("INFO", "result: OK");
            ExitCode::SUCCESS
        }
        Ok(()) => {
            write_line("INFO", &format!("result: OK with {warnings} warning(s)"));
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("\nerror: {error:#}");
            write_line("ERROR", &format!("{error:#}"));
            write_line("INFO", "result: FAILED");
            if let Some(path) = path() {
                eprintln!("log: {}", path.display());
            }
            ExitCode::FAILURE
        }
    }
}

macro_rules! say {
    ($($arg:tt)*) => { $crate::log::say(&format!($($arg)*)) };
}

#[cfg_attr(not(windows), allow(unused_macros))]
macro_rules! note {
    ($($arg:tt)*) => { $crate::log::note(&format!($($arg)*)) };
}

macro_rules! warn {
    ($($arg:tt)*) => { $crate::log::warn(&format!($($arg)*)) };
}
