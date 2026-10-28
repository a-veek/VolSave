//! How operations tell the caller what they do.

use volsave_core::Progress;

/// Kind of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// A result worth keeping, e.g. the summary at the end of a backup.
    Result,
    /// What is happening right now, plans and hints.
    Status,
    /// Something the user should know about; the operation goes on.
    Warning,
}

/// Long-running part of an operation that reports progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Backup,
    Restore,
    /// Reading restored data back from the target.
    VerifyTarget,
    Merge,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Backup => "Backup",
            Stage::Restore => "Restore",
            Stage::VerifyTarget => "Verifying target",
            Stage::Merge => "Merging",
        }
    }
}

/// Receives messages and progress; the command line tool prints and logs
/// them, a graphical interface shows them.
pub trait Reporter {
    fn message(&mut self, level: Level, text: &str);

    /// Called after every block of a stage.
    fn progress(&mut self, stage: Stage, progress: &Progress);

    /// The stage has ended, successfully or not.
    fn stage_done(&mut self, _stage: Stage) {}
}

/// Discards everything; for tests and callers that do not care.
pub struct Silent;

impl Reporter for Silent {
    fn message(&mut self, _: Level, _: &str) {}
    fn progress(&mut self, _: Stage, _: &Progress) {}
}

#[cfg_attr(not(windows), allow(unused_macros))]
macro_rules! say {
    ($rep:expr, $($arg:tt)*) => { $rep.message($crate::Level::Result, &format!($($arg)*)) };
}

#[cfg_attr(not(windows), allow(unused_macros))]
macro_rules! note {
    ($rep:expr, $($arg:tt)*) => { $rep.message($crate::Level::Status, &format!($($arg)*)) };
}

macro_rules! warn {
    ($rep:expr, $($arg:tt)*) => { $rep.message($crate::Level::Warning, &format!($($arg)*)) };
}
