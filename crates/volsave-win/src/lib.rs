//! Windows access for VolSave: volumes, disks, VSS snapshots and raw I/O.
//!
//! Everything is Windows-only; on other platforms the crate is empty so the
//! workspace still builds and tests there.
#![cfg(windows)]

mod allocation;
mod device;
mod disk;
mod exclusions;
mod privileges;
mod util;
mod volume;
mod vss;

pub use allocation::{Allocation, file_extents, system_file_extents, used_ranges};
pub use device::RawDevice;
pub use disk::{
    DiskInfo, DiskLayout, PartitionInfo, PartitionKind, disk_attributes, list_disks,
    open_disk_for_writing, refresh_disk, set_disk_attributes,
};
pub use exclusions::{ExcludedEntry, ExclusionMode, excluded_files, files_not_to_backup};
pub use privileges::enable_backup_privileges;
pub use util::{format_guid, set_debug_hook};
pub use volume::{DiskExtent, VolumeInfo, list_volumes, resolve_volume};
pub use vss::{Snapshot, SnapshotSet, VssError};
