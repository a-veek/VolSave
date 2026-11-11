//! Operations shared by the VolSave command line tool and graphical
//! interfaces: opening images, backing up and restoring volumes and whole
//! disks.
//!
//! Messages and progress go to a [`Reporter`]; the caller decides how to
//! show them. Restores and clones are planned first ([`plan_restore`],
//! [`plan_clone`]) and run only after the caller has asked for confirmation.

#[macro_use]
mod report;
mod images;
mod store;
mod util;

#[cfg(windows)]
mod backup;
#[cfg(windows)]
mod clone;
#[cfg(windows)]
mod disk;
#[cfg(windows)]
mod restore;

pub use images::{
    OpenedImage, Reader, check_output, find_base, hash_path, open_image, open_view, read_hash_list,
    write_hash_list,
};
pub use report::{Level, Reporter, Silent, Stage};
pub use store::{ImageSink, ImageSource, ImageStore, LocalStore, set_store, store};
pub use util::{VERSION, WriteOptions, human, now_unix, tool_name};

#[cfg(windows)]
pub use backup::{BackupRequest, backup};
#[cfg(windows)]
pub use clone::{ClonePlan, CloneRequest, plan_clone};
#[cfg(windows)]
pub use disk::{DiskRestore, find_disk, parse_disk, print_plan, volumes_on};
#[cfg(windows)]
pub use restore::{RestorePlan, VolumeRestore, plan_restore};
#[cfg(windows)]
pub use volsave_win::ExclusionMode;
