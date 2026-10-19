use std::fs::File;
use std::io;

use rsnap_core::{BlockSource, BlockTarget, read_exact_at, write_all_at};
use windows::Win32::System::Ioctl::{
    FSCTL_ALLOW_EXTENDED_DASD_IO, FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME,
    IOCTL_DISK_GET_LENGTH_INFO,
};

use crate::util::{Access, i64_at, ioctl, open};

/// Unbuffered access to a volume, snapshot or physical disk.
pub struct RawDevice {
    file: File,
    path: String,
    size: u64,
    sector_size: u32,
}

impl RawDevice {
    /// Opens `path` (e.g. `\\?\GLOBALROOT\Device\HarddiskVolumeShadowCopy3` or
    /// `\\?\Volume{GUID}`) for reading.
    pub fn open_read(path: &str, sector_size: u32) -> io::Result<Self> {
        Self::open(path, Access::Read, sector_size)
    }

    /// Opens a device for restoring. Call [`Self::lock_and_dismount`] before
    /// writing to a volume.
    pub fn open_write(path: &str, sector_size: u32) -> io::Result<Self> {
        Self::open(path, Access::ReadWrite, sector_size)
    }

    fn open(path: &str, access: Access, sector_size: u32) -> io::Result<Self> {
        let file = open(path, access, true)?;
        // Allows I/O past the end of the file system (the NTFS backup boot
        // sector). Physical disks do not need it and may reject it.
        let _ = ioctl(&file, FSCTL_ALLOW_EXTENDED_DASD_IO, &[], 0);
        let raw = ioctl(&file, IOCTL_DISK_GET_LENGTH_INFO, &[], 8)?;
        Ok(Self {
            size: i64_at(&raw, 0)? as u64,
            file,
            path: path.to_string(),
            sector_size: sector_size.max(512),
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn sector_size(&self) -> u32 {
        self.sector_size
    }

    /// Takes the volume away from its file system so it can be overwritten.
    /// Fails if files on the volume are still open.
    pub fn lock_and_dismount(&self) -> io::Result<()> {
        ioctl(&self.file, FSCTL_LOCK_VOLUME, &[], 0)?;
        ioctl(&self.file, FSCTL_DISMOUNT_VOLUME, &[], 0)?;
        Ok(())
    }
}

impl BlockSource for RawDevice {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        read_exact_at(&self.file, buf, offset)
    }

    fn alignment(&self) -> usize {
        self.sector_size as usize
    }
}

impl BlockTarget for RawDevice {
    fn size(&self) -> u64 {
        self.size
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        write_all_at(&self.file, data, offset)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }
}
