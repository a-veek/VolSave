//! Platform-independent backup and restore engine.
//!
//! The engine reads through [`BlockSource`] and writes through
//! [`BlockTarget`]; platform crates provide implementations for volumes and
//! disks. Images are read through [`ImageView`], which also combines a
//! differential image with its full image.

mod backup;
mod bootsector;
mod buffer;
mod exclusion;
mod partition;
mod ranges;
mod restore;
mod source;
mod view;

pub use backup::{BackupError, BackupOptions, BackupOutput, BackupStats, Progress, backup};
pub use bootsector::{
    BootSectorKind, HiddenSectorsFix, fix_ntfs_hidden_sectors, identify_boot_sector,
};
pub use buffer::AlignedBuf;
pub use exclusion::{PathRule, wildcard_match};
pub use partition::{
    DiskLayout, Gpt, Guid, LayoutError, Partition, PartitionKind, PartitionStyle, Retarget,
    gpt_types,
};
pub use ranges::{BlockPlan, PlannedBlock, RangeSet};
pub use restore::{RestoreError, RestoreStats, merge, restore, restore_disk};
pub use source::{
    BlockSource, BlockTarget, FileSource, FileTarget, MappedSource, PatchedSource, read_exact_at,
    write_all_at,
};
pub use view::{ImageView, LayerError, LayeredImage, PartitionView, SingleImage};
