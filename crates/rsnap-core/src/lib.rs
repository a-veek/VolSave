//! Platform-independent backup and restore engine.
//!
//! The engine reads through [`BlockSource`] and writes through
//! [`BlockTarget`]; platform crates provide implementations for volumes and
//! disks. Images are read through [`ImageView`], which also combines a
//! differential image with its full image.

mod backup;
mod buffer;
mod exclusion;
mod ranges;
mod restore;
mod source;
mod view;

pub use backup::{BackupError, BackupOptions, BackupOutput, BackupStats, Progress, backup};
pub use buffer::AlignedBuf;
pub use exclusion::{PathRule, wildcard_match};
pub use ranges::{BlockPlan, PlannedBlock, RangeSet};
pub use restore::{RestoreError, RestoreStats, merge, restore};
pub use source::{
    BlockSource, BlockTarget, FileSource, FileTarget, PatchedSource, read_exact_at, write_all_at,
};
pub use view::{ImageView, LayerError, LayeredImage, SingleImage};
