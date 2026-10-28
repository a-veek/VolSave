//! Operations shared by the VolSave command line tool and graphical
//! interfaces: opening images, backing up and restoring volumes and whole
//! disks.
//!
//! Messages and progress go to a [`Reporter`]; the caller decides how to
//! show them. Restores are planned first ([`plan_restore`]) and run only
//! after the caller has asked for confirmation.

#[macro_use]
mod report;
mod images;
mod util;

#[cfg(windows)]
mod backup;
#[cfg(windows)]
mod disk;
#[cfg(windows)]
mod restore;

pub use images::{
    OpenedImage, Reader, find_base, hash_path, open_image, open_view, read_hash_list,
    write_hash_list,
};
pub use report::{Level, Reporter, Silent, Stage};
pub use util::{WriteOptions, human, now_unix, tool_name};

#[cfg(windows)]
pub use backup::{BackupRequest, backup};
#[cfg(windows)]
pub use disk::{DiskRestore, find_disk, parse_disk, print_plan, volumes_on};
#[cfg(windows)]
pub use restore::{RestorePlan, VolumeRestore, plan_restore};
#[cfg(windows)]
pub use volsave_win::ExclusionMode;
