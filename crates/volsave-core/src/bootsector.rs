//! Boot sector fix-ups after restoring a volume to another place on a disk.
//!
//! The NTFS boot sector stores the partition's start sector ("hidden
//! sectors"). Windows ignores it once running, but the boot code uses it to
//! find BOOTMGR, so a volume restored to a partition at a different offset
//! does not start until the field is corrected.

use std::io;

use crate::{AlignedBuf, BlockSource, BlockTarget};

const BYTES_PER_SECTOR: usize = 0x0B;
const HIDDEN_SECTORS: usize = 0x1C;
const TOTAL_SECTORS: usize = 0x28;

/// File system a partition's first sector belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootSectorKind {
    Ntfs,
    Refs,
    Fat,
    ExFat,
    /// BitLocker-encrypted volume. Read through its volume device it would
    /// come out decrypted, so a disk image copies its raw sectors.
    BitLocker,
    Unknown,
}

pub fn identify_boot_sector(sector: &[u8]) -> BootSectorKind {
    if sector.len() < 512 {
        return BootSectorKind::Unknown;
    }
    match &sector[3..11] {
        b"-FVE-FS-" => BootSectorKind::BitLocker,
        b"NTFS    " => BootSectorKind::Ntfs,
        b"EXFAT   " => BootSectorKind::ExFat,
        oem if &oem[..4] == b"ReFS" => BootSectorKind::Refs,
        _ if &sector[0x36..0x39] == b"FAT" || &sector[0x52..0x57] == b"FAT32" => {
            BootSectorKind::Fat
        }
        _ => BootSectorKind::Unknown,
    }
}

/// What [`fix_ntfs_hidden_sectors`] changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenSectorsFix {
    pub old: u32,
    pub new: u32,
    /// Whether the backup boot sector at the end of the volume was found and
    /// corrected as well.
    pub backup_fixed: bool,
}

fn is_ntfs(sector: &[u8]) -> bool {
    sector.len() >= 512 && &sector[3..11] == b"NTFS    " && sector[510..512] == [0x55, 0xAA]
}

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(data[at..at + 2].try_into().unwrap())
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

/// Sets the hidden sectors field of the NTFS volume on `device` to match a
/// partition starting `partition_offset` bytes into its disk, in the boot
/// sector and in the backup boot sector.
///
/// Returns `None` when the volume is not NTFS or the field is already right.
pub fn fix_ntfs_hidden_sectors<D>(
    device: &mut D,
    partition_offset: u64,
) -> io::Result<Option<HiddenSectorsFix>>
where
    D: BlockSource + BlockTarget,
{
    let unit = BlockSource::alignment(device).max(512);
    let device_size = BlockSource::size(device);
    let mut buf = AlignedBuf::zeroed(unit);

    device.read_at(0, &mut buf)?;
    if !is_ntfs(&buf) {
        return Ok(None);
    }
    let bytes_per_sector = u64::from(u16_at(&buf, BYTES_PER_SECTOR));
    if bytes_per_sector < 512 || partition_offset % bytes_per_sector != 0 {
        return Ok(None);
    }
    let Ok(new) = u32::try_from(partition_offset / bytes_per_sector) else {
        return Ok(None);
    };
    let old = u32_at(&buf, HIDDEN_SECTORS);
    if old == new {
        return Ok(None);
    }
    let total_sectors = u64_at(&buf, TOTAL_SECTORS);
    buf[HIDDEN_SECTORS..HIDDEN_SECTORS + 4].copy_from_slice(&new.to_le_bytes());
    device.write_at(0, &buf)?;

    // NTFS keeps a copy of the boot sector right after its last sector.
    let backup = total_sectors.saturating_mul(bytes_per_sector);
    let start = backup / unit as u64 * unit as u64;
    let at = (backup - start) as usize;
    let mut backup_fixed = false;
    if start + unit as u64 <= device_size {
        device.read_at(start, &mut buf)?;
        if is_ntfs(&buf[at..]) && u32_at(&buf, at + HIDDEN_SECTORS) == old {
            buf[at + HIDDEN_SECTORS..at + HIDDEN_SECTORS + 4].copy_from_slice(&new.to_le_bytes());
            device.write_at(start, &buf)?;
            backup_fixed = true;
        }
    }
    BlockTarget::flush(device)?;
    Ok(Some(HiddenSectorsFix {
        old,
        new,
        backup_fixed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem(Vec<u8>);

    impl BlockSource for Mem {
        fn size(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            let o = offset as usize;
            buf.copy_from_slice(&self.0[o..o + buf.len()]);
            Ok(())
        }
        fn alignment(&self) -> usize {
            512
        }
    }

    impl BlockTarget for Mem {
        fn size(&self) -> u64 {
            self.0.len() as u64
        }
        fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
            let o = offset as usize;
            self.0[o..o + data.len()].copy_from_slice(data);
            Ok(())
        }
    }

    fn ntfs_volume(sectors: u64, hidden: u32, partition_sectors: u64) -> Mem {
        let mut boot = vec![0u8; 512];
        boot[3..11].copy_from_slice(b"NTFS    ");
        boot[BYTES_PER_SECTOR..BYTES_PER_SECTOR + 2].copy_from_slice(&512u16.to_le_bytes());
        boot[HIDDEN_SECTORS..HIDDEN_SECTORS + 4].copy_from_slice(&hidden.to_le_bytes());
        // NTFS counts all sectors but the backup boot sector.
        boot[TOTAL_SECTORS..TOTAL_SECTORS + 8].copy_from_slice(&(sectors - 1).to_le_bytes());
        boot[510] = 0x55;
        boot[511] = 0xAA;
        let mut data = vec![0u8; (partition_sectors * 512) as usize];
        data[..512].copy_from_slice(&boot);
        let backup = ((sectors - 1) * 512) as usize;
        data[backup..backup + 512].copy_from_slice(&boot);
        Mem(data)
    }

    #[test]
    fn fixes_boot_sector_and_backup() {
        let mut dev = ntfs_volume(64, 104_448, 80);
        let fix = fix_ntfs_hidden_sectors(&mut dev, 2048 * 512).unwrap();
        assert_eq!(
            fix,
            Some(HiddenSectorsFix {
                old: 104_448,
                new: 2048,
                backup_fixed: true
            })
        );
        assert_eq!(u32_at(&dev.0, HIDDEN_SECTORS), 2048);
        assert_eq!(u32_at(&dev.0, 63 * 512 + HIDDEN_SECTORS), 2048);
    }

    #[test]
    fn identifies_file_systems() {
        let mut sector = vec![0u8; 512];
        assert_eq!(identify_boot_sector(&sector), BootSectorKind::Unknown);
        sector[3..11].copy_from_slice(b"-FVE-FS-");
        assert_eq!(identify_boot_sector(&sector), BootSectorKind::BitLocker);
        sector[3..11].copy_from_slice(b"MSDOS5.0");
        sector[0x52..0x57].copy_from_slice(b"FAT32");
        assert_eq!(identify_boot_sector(&sector), BootSectorKind::Fat);
        let ntfs = ntfs_volume(8, 0, 8);
        assert_eq!(identify_boot_sector(&ntfs.0), BootSectorKind::Ntfs);
    }

    #[test]
    fn leaves_correct_or_foreign_volumes_alone() {
        let mut dev = ntfs_volume(64, 2048, 64);
        assert_eq!(fix_ntfs_hidden_sectors(&mut dev, 2048 * 512).unwrap(), None);

        let mut fat = Mem(vec![0u8; 4096]);
        assert_eq!(fix_ntfs_hidden_sectors(&mut fat, 2048 * 512).unwrap(), None);
        assert!(fat.0.iter().all(|&b| b == 0));
    }
}
