//! Windows access for rsnap: volumes, disks, VSS snapshots and raw I/O.
//!
//! Everything is Windows-only; on other platforms the crate is empty so the
//! workspace still builds and tests there.
#![cfg(windows)]

mod allocation;
mod device;
mod disk;
mod util;
mod volume;
mod vss;

pub use allocation::{Allocation, file_extents, system_file_extents, used_ranges};
pub use device::RawDevice;
pub use disk::{DiskInfo, DiskLayout, PartitionInfo, PartitionKind, list_disks};
pub use util::format_guid;
pub use volume::{DiskExtent, VolumeInfo, list_volumes, resolve_volume};
pub use vss::{Snapshot, SnapshotSet, VssError};
