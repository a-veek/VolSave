//! Partition tables: reading MBR (with logical partitions) and GPT from a
//! disk or a disk image, deciding which areas outside the partitions a disk
//! image must hold, and adapting a GPT to a target disk of another size.
//!
//! The tables are read from the raw sectors rather than from the operating
//! system, so the same code serves disks, images and tests.

use std::fmt;
use std::io;
use std::ops::Range;

use crate::{AlignedBuf, RangeSet};

const MBR_SIGNATURE: [u8; 2] = [0x55, 0xAA];
const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
const PROTECTIVE_TYPE: u8 = 0xEE;
/// MBR types that hold logical partitions.
const EXTENDED_TYPES: [u8; 3] = [0x05, 0x0F, 0x85];
/// More logical partitions than this means a broken or looping EBR chain.
const MAX_LOGICAL: usize = 128;
/// Largest GPT entry array accepted (the usual one is 16 KiB).
const MAX_GPT_ENTRIES_BYTES: u64 = 1 << 20;

/// 16 bytes as stored on disk (first three fields little endian).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Guid(pub [u8; 16]);

impl Guid {
    pub fn is_zero(&self) -> bool {
        self.0 == [0; 16]
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            b[3],
            b[2],
            b[1],
            b[0],
            b[5],
            b[4],
            b[7],
            b[6],
            b[8],
            b[9],
            b[10],
            b[11],
            b[12],
            b[13],
            b[14],
            b[15]
        )
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Well-known GPT partition types.
pub mod gpt_types {
    pub const EFI_SYSTEM: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
    pub const MICROSOFT_RESERVED: &str = "e3c9e316-0b5c-4db8-817d-f92df00215ae";
    pub const BASIC_DATA: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
    pub const WINDOWS_RECOVERY: &str = "de94bba4-06d1-4d40-a16a-bfd50179d6ac";
    pub const LDM_METADATA: &str = "5808c8aa-7e8f-42e0-85d2-e1e90434cfb3";
    pub const LDM_DATA: &str = "af9b60a0-1431-4f62-bc68-3311714a69ad";
    pub const STORAGE_SPACES: &str = "e75caf8f-f680-4cee-afa3-b001e56efc2d";
    pub const LINUX_DATA: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionStyle {
    Mbr,
    Gpt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartitionKind {
    Mbr {
        type_byte: u8,
        bootable: bool,
        logical: bool,
    },
    Gpt {
        type_guid: Guid,
        unique_guid: Guid,
        name: String,
        attributes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// MBR: primary slots 1-4, logical partitions from 5 on. GPT: entry
    /// index + 1. Windows numbers partitions differently.
    pub number: u32,
    /// Byte offset on the disk.
    pub offset: u64,
    pub length: u64,
    pub kind: PartitionKind,
}

impl Partition {
    pub fn end(&self) -> u64 {
        self.offset + self.length
    }

    pub fn range(&self) -> Range<u64> {
        self.offset..self.end()
    }

    /// Short English description of well-known types.
    pub fn type_name(&self) -> String {
        match &self.kind {
            PartitionKind::Mbr { type_byte, .. } => match type_byte {
                0x01 | 0x04 | 0x06 | 0x0e => "FAT".into(),
                0x07 => "NTFS/exFAT".into(),
                0x0b | 0x0c => "FAT32".into(),
                0x27 => "Windows RE".into(),
                0x42 => "Dynamic disk (LDM)".into(),
                0x82 => "Linux swap".into(),
                0x83 => "Linux".into(),
                0x8e => "Linux LVM".into(),
                0xef => "EFI system".into(),
                other => format!("type 0x{other:02x}"),
            },
            PartitionKind::Gpt { type_guid, .. } => match type_guid.to_string().as_str() {
                gpt_types::EFI_SYSTEM => "EFI system".into(),
                gpt_types::MICROSOFT_RESERVED => "Microsoft reserved".into(),
                gpt_types::BASIC_DATA => "Basic data".into(),
                gpt_types::WINDOWS_RECOVERY => "Windows RE".into(),
                gpt_types::LDM_METADATA => "LDM metadata (dynamic disk)".into(),
                gpt_types::LDM_DATA => "LDM data (dynamic disk)".into(),
                gpt_types::STORAGE_SPACES => "Storage Spaces".into(),
                gpt_types::LINUX_DATA => "Linux".into(),
                other => other.to_string(),
            },
        }
    }

    /// Dynamic disks and Storage Spaces spread volumes over partitions in
    /// ways only their own drivers understand.
    pub fn is_pooled(&self) -> bool {
        match &self.kind {
            PartitionKind::Mbr { type_byte, .. } => *type_byte == 0x42,
            PartitionKind::Gpt { type_guid, .. } => matches!(
                type_guid.to_string().as_str(),
                gpt_types::LDM_METADATA | gpt_types::LDM_DATA | gpt_types::STORAGE_SPACES
            ),
        }
    }
}

/// The GPT fields needed to copy or move it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpt {
    pub disk_guid: Guid,
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    /// LBA of the backup header.
    pub alternate_lba: u64,
    pub entries_lba: u64,
    /// Where the backup entry array starts.
    pub backup_entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
    /// The primary header sector as read.
    header: Vec<u8>,
    /// The entry array, padded to whole sectors.
    entries: Vec<u8>,
}

impl Gpt {
    fn entries_sectors(&self, sector_size: u64) -> u64 {
        self.entries.len() as u64 / sector_size
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskLayout {
    pub style: PartitionStyle,
    pub sector_size: u32,
    /// Size of the disk the layout was read from.
    pub disk_size: u64,
    /// MBR disk signature (0x1B8); Windows identifies MBR disks by it.
    pub mbr_signature: u32,
    /// Data partitions in table order; MBR extended containers are left out.
    pub partitions: Vec<Partition>,
    /// Byte offsets of the extended boot records of logical partitions,
    /// each with the start of the partition it describes.
    pub ebrs: Vec<(u64, u64)>,
    pub gpt: Option<Gpt>,
    /// Sector 0 as read.
    mbr: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("the disk has no partition table")]
    NoPartitionTable,
    #[error("invalid partition table: {0}")]
    Invalid(String),
    #[error("{0}")]
    Unsupported(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn invalid(message: impl Into<String>) -> LayoutError {
    LayoutError::Invalid(message.into())
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

fn guid_at(data: &[u8], at: usize) -> Guid {
    Guid(data[at..at + 16].try_into().unwrap())
}

/// Reads whole sectors through an aligned buffer.
struct SectorReader<'a> {
    read: &'a mut dyn FnMut(u64, &mut [u8]) -> io::Result<()>,
    sector_size: u64,
    disk_size: u64,
}

impl SectorReader<'_> {
    fn read(&mut self, lba: u64, sectors: u64) -> Result<Vec<u8>, LayoutError> {
        let offset = lba
            .checked_mul(self.sector_size)
            .ok_or_else(|| invalid("sector number out of range"))?;
        let len = sectors * self.sector_size;
        if offset + len > self.disk_size {
            return Err(invalid(format!(
                "sector {lba} lies beyond the end of the disk"
            )));
        }
        let mut buf = AlignedBuf::zeroed(len as usize);
        (self.read)(offset, &mut buf)?;
        Ok(buf.to_vec())
    }
}

impl DiskLayout {
    /// Reads the partition table. `read` fills a buffer from a byte offset;
    /// offsets and lengths are whole sectors, buffers are 4096-aligned.
    pub fn read(
        read: &mut dyn FnMut(u64, &mut [u8]) -> io::Result<()>,
        disk_size: u64,
        sector_size: u32,
    ) -> Result<Self, LayoutError> {
        if !sector_size.is_power_of_two() || sector_size < 512 {
            return Err(invalid(format!("unusable sector size {sector_size}")));
        }
        let mut reader = SectorReader {
            read,
            sector_size: u64::from(sector_size),
            disk_size,
        };
        let mbr = reader.read(0, 1)?;
        if mbr[510..512] != MBR_SIGNATURE {
            return Err(LayoutError::NoPartitionTable);
        }
        let mut layout = DiskLayout {
            style: PartitionStyle::Mbr,
            sector_size,
            disk_size,
            mbr_signature: u32_at(&mbr, 0x1B8),
            partitions: Vec::new(),
            ebrs: Vec::new(),
            gpt: None,
            mbr,
        };
        let entries = mbr_entries(&layout.mbr);
        if entries.iter().any(|e| e.type_byte == PROTECTIVE_TYPE) {
            layout.style = PartitionStyle::Gpt;
            layout.read_gpt(&mut reader)?;
        } else {
            layout.read_mbr(&mut reader, &entries)?;
        }
        layout.check()?;
        Ok(layout)
    }

    fn read_mbr(
        &mut self,
        reader: &mut SectorReader<'_>,
        entries: &[MbrEntry],
    ) -> Result<(), LayoutError> {
        let sector = reader.sector_size;
        let mut extended = None;
        for (slot, entry) in entries.iter().enumerate() {
            if entry.type_byte == 0 || entry.sectors == 0 {
                continue;
            }
            if EXTENDED_TYPES.contains(&entry.type_byte) {
                if extended.replace(entry.start).is_some() {
                    return Err(invalid("more than one extended partition"));
                }
                continue;
            }
            self.partitions.push(Partition {
                number: slot as u32 + 1,
                offset: entry.start * sector,
                length: entry.sectors * sector,
                kind: PartitionKind::Mbr {
                    type_byte: entry.type_byte,
                    bootable: entry.bootable,
                    logical: false,
                },
            });
        }

        // Logical partitions: a chain of EBRs, each describing one partition
        // (relative to itself) and the next EBR (relative to the container).
        let Some(container) = extended else {
            return Ok(());
        };
        let mut ebr_lba = container;
        let mut number = 5;
        loop {
            if self.ebrs.len() >= MAX_LOGICAL {
                return Err(invalid("too many logical partitions (looping EBR chain?)"));
            }
            let ebr = reader.read(ebr_lba, 1)?;
            if ebr[510..512] != MBR_SIGNATURE {
                return Err(invalid(format!(
                    "no extended boot record at sector {ebr_lba}"
                )));
            }
            let links = mbr_entries(&ebr);
            let logical = &links[0];
            let mut described = ebr_lba * sector;
            if logical.type_byte != 0 && logical.sectors != 0 {
                described = (ebr_lba + logical.start) * sector;
                self.partitions.push(Partition {
                    number,
                    offset: described,
                    length: logical.sectors * sector,
                    kind: PartitionKind::Mbr {
                        type_byte: logical.type_byte,
                        bootable: logical.bootable,
                        logical: true,
                    },
                });
                number += 1;
            }
            self.ebrs.push((ebr_lba * sector, described));
            let next = &links[1];
            if next.type_byte == 0 || next.sectors == 0 {
                break;
            }
            let next_lba = container + next.start;
            if next_lba <= ebr_lba {
                return Err(invalid("extended boot records point backwards"));
            }
            ebr_lba = next_lba;
        }
        Ok(())
    }

    fn read_gpt(&mut self, reader: &mut SectorReader<'_>) -> Result<(), LayoutError> {
        let sector = reader.sector_size;
        let header = reader.read(1, 1)?;
        let gpt = parse_gpt_header(&header, 1).map_err(|e| invalid(format!("GPT header: {e}")))?;
        let entries_bytes = u64::from(gpt.entry_count) * u64::from(gpt.entry_size);
        if gpt.entry_size < 128 || entries_bytes > MAX_GPT_ENTRIES_BYTES {
            return Err(invalid("implausible GPT entry array size"));
        }
        let entries = reader.read(gpt.entries_lba, entries_bytes.div_ceil(sector))?;
        if crc32fast::hash(&entries[..entries_bytes as usize]) != gpt.entries_crc {
            return Err(invalid("GPT entry array checksum mismatch"));
        }

        // The backup header says where the backup entries are; fall back to
        // the usual place right before it if it cannot be read.
        let entries_sectors = entries.len() as u64 / sector;
        let usual = gpt.alternate_lba.saturating_sub(entries_sectors);
        let backup_entries_lba = reader
            .read(gpt.alternate_lba, 1)
            .ok()
            .and_then(|backup| parse_gpt_header(&backup, gpt.alternate_lba).ok())
            .map_or(usual, |backup| backup.entries_lba);

        for i in 0..gpt.entry_count as usize {
            let e = &entries[i * gpt.entry_size as usize..][..128];
            let type_guid = guid_at(e, 0);
            if type_guid.is_zero() {
                continue;
            }
            let (first, last) = (u64_at(e, 32), u64_at(e, 40));
            if last < first {
                return Err(invalid(format!(
                    "GPT entry {} ends before it starts",
                    i + 1
                )));
            }
            let name: Vec<u16> = (0..36)
                .map(|c| u16_at(e, 56 + c * 2))
                .take_while(|&c| c != 0)
                .collect();
            self.partitions.push(Partition {
                number: i as u32 + 1,
                offset: first * sector,
                length: (last - first + 1) * sector,
                kind: PartitionKind::Gpt {
                    type_guid,
                    unique_guid: guid_at(e, 16),
                    name: String::from_utf16_lossy(&name),
                    attributes: u64_at(e, 48),
                },
            });
        }
        self.gpt = Some(Gpt {
            disk_guid: gpt.disk_guid,
            first_usable_lba: gpt.first_usable_lba,
            last_usable_lba: gpt.last_usable_lba,
            alternate_lba: gpt.alternate_lba,
            entries_lba: gpt.entries_lba,
            backup_entries_lba,
            entry_count: gpt.entry_count,
            entry_size: gpt.entry_size,
            header,
            entries,
        });
        Ok(())
    }

    /// Partitions must lie on the disk and must not overlap.
    fn check(&self) -> Result<(), LayoutError> {
        let mut sorted: Vec<_> = self.partitions.iter().collect();
        sorted.sort_by_key(|p| p.offset);
        for p in &sorted {
            if p.offset == 0 || p.end() > self.disk_size {
                return Err(invalid(format!(
                    "partition {} lies outside the disk",
                    p.number
                )));
            }
        }
        for pair in sorted.windows(2) {
            if pair[0].end() > pair[1].offset {
                return Err(invalid(format!(
                    "partitions {} and {} overlap",
                    pair[0].number, pair[1].number
                )));
            }
        }
        Ok(())
    }

    /// GPT disk GUID or MBR signature, the way Windows shows them.
    pub fn disk_id(&self) -> String {
        match &self.gpt {
            Some(gpt) => format!("{{{}}}", gpt.disk_guid),
            None => format!("{:08X}", self.mbr_signature),
        }
    }

    /// Areas outside the partitions that a disk image must hold: the boot
    /// sector with everything up to the first partition (boot loaders such
    /// as GRUB live there), the extended boot records and the backup GPT.
    pub fn table_ranges(&self) -> RangeSet {
        let sector = u64::from(self.sector_size);
        let first = self
            .partitions
            .iter()
            .map(|p| p.offset)
            .chain(self.ebrs.iter().map(|&(ebr, _)| ebr))
            .min();
        let head_end = match (&self.gpt, first) {
            (_, Some(first)) => first,
            (Some(gpt), None) => (gpt.entries_lba + gpt.entries_sectors(sector)) * sector,
            (None, None) => sector,
        };
        let mut ranges = Vec::with_capacity(self.ebrs.len() + 2);
        ranges.push(0..head_end.min(self.disk_size));
        for &(ebr, described) in &self.ebrs {
            ranges.push(ebr..described.max(ebr + sector));
        }
        if let Some(backup) = self.backup_gpt_range() {
            ranges.push(backup);
        }
        RangeSet::from_ranges(ranges).clamp(0..self.disk_size)
    }

    /// The backup GPT: entry array and header at the end of the disk.
    pub fn backup_gpt_range(&self) -> Option<Range<u64>> {
        let gpt = self.gpt.as_ref()?;
        let sector = u64::from(self.sector_size);
        let start = gpt.backup_entries_lba.min(gpt.alternate_lba) * sector;
        Some(start..(gpt.alternate_lba + 1) * sector)
    }

    /// Unpartitioned areas that are not part of [`Self::table_ranges`].
    pub fn gaps(&self) -> RangeSet {
        let partitions = RangeSet::from_ranges(self.partitions.iter().map(Partition::range));
        RangeSet::full(self.disk_size)
            .subtract(&partitions)
            .subtract(&self.table_ranges())
    }

    /// Bytes from the start of the disk up to the first partition; written
    /// last when restoring so the new table only appears once the data is in
    /// place.
    pub fn head_range(&self) -> Range<u64> {
        let head = self.table_ranges();
        head.ranges().first().cloned().unwrap_or(0..0)
    }

    /// Smallest disk the layout fits on.
    pub fn required_size(&self) -> u64 {
        let sector = u64::from(self.sector_size);
        let partitions_end = self
            .partitions
            .iter()
            .map(Partition::end)
            .max()
            .unwrap_or(0);
        let head_end = self.head_range().end;
        let ebrs_end = self
            .ebrs
            .iter()
            .map(|&(e, _)| e + sector)
            .max()
            .unwrap_or(0);
        let data_end = partitions_end.max(head_end).max(ebrs_end);
        match &self.gpt {
            // Room for the backup entry array and header behind the data.
            Some(gpt) => data_end + (gpt.entries_sectors(sector) + 1) * sector,
            None => data_end,
        }
    }

    /// What to change when writing this layout to a disk of `target_size`
    /// bytes instead of the original one.
    pub fn retarget(&self, target_size: u64) -> Result<Retarget, LayoutError> {
        let sector = u64::from(self.sector_size);
        let required = self.required_size();
        if target_size < required {
            return Err(LayoutError::Unsupported(format!(
                "the target disk is too small: {required} bytes needed, {target_size} available"
            )));
        }
        let mut plan = Retarget::default();
        let target_sectors = target_size / sector;
        // Old partition table signatures at the end of the target (a stale
        // backup GPT) would confuse tools; clear what no partition uses.
        let data_end = self
            .partitions
            .iter()
            .map(Partition::end)
            .max()
            .unwrap_or(0)
            .max(self.head_range().end);
        let tail = (target_sectors * sector)
            .saturating_sub(1 << 20)
            .max(data_end)..target_sectors * sector;

        let Some(gpt) = &self.gpt else {
            if target_size != self.disk_size {
                plan.zero.push(tail);
            }
            return Ok(plan);
        };
        if target_size == self.disk_size {
            return Ok(plan);
        }

        let entries_sectors = gpt.entries_sectors(sector);
        let alternate = target_sectors - 1;
        let backup_entries = alternate - entries_sectors;
        let last_usable = backup_entries - 1;
        let last_used = self
            .partitions
            .iter()
            .map(|p| p.end() / sector - 1)
            .max()
            .unwrap_or(0);
        if last_used > last_usable {
            return Err(LayoutError::Unsupported(
                "the last partition would overlap the backup GPT".into(),
            ));
        }

        let mut primary = gpt.header.clone();
        primary[32..40].copy_from_slice(&alternate.to_le_bytes());
        primary[48..56].copy_from_slice(&last_usable.to_le_bytes());
        seal_gpt_header(&mut primary);

        let mut backup = primary.clone();
        backup[24..32].copy_from_slice(&alternate.to_le_bytes());
        backup[32..40].copy_from_slice(&1u64.to_le_bytes());
        backup[72..80].copy_from_slice(&backup_entries.to_le_bytes());
        seal_gpt_header(&mut backup);

        let mut mbr = self.mbr.clone();
        let old_size = (self.disk_size / sector - 1).min(u64::from(u32::MAX)) as u32;
        let new_size = (target_sectors - 1).min(u64::from(u32::MAX)) as u32;
        for slot in 0..4 {
            let at = 446 + slot * 16;
            if mbr[at + 4] == PROTECTIVE_TYPE
                && u32_at(&mbr, at + 8) == 1
                && u32_at(&mbr, at + 12) == old_size
            {
                mbr[at + 12..at + 16].copy_from_slice(&new_size.to_le_bytes());
            }
        }

        // The original backup GPT now lies in free space or past the end.
        plan.skip = self.backup_gpt_range().into_iter().collect();
        plan.patches = vec![(0, mbr), (sector, primary)];
        let mut tail_data = gpt.entries.clone();
        tail_data.extend_from_slice(&backup);
        let backup_start = backup_entries * sector;
        if tail.start < backup_start {
            plan.zero.push(tail.start..backup_start);
        }
        plan.writes.push((backup_start, tail_data));
        Ok(plan)
    }
}

/// Adjustments for restoring a layout onto a disk of another size.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Retarget {
    /// Image areas not to copy (the old backup GPT).
    pub skip: Vec<Range<u64>>,
    /// Replacement bytes for sectors inside the image's head area.
    pub patches: Vec<(u64, Vec<u8>)>,
    /// Data to write outside the image's areas (the new backup GPT).
    pub writes: Vec<(u64, Vec<u8>)>,
    /// Areas to fill with zeros.
    pub zero: Vec<Range<u64>>,
}

struct MbrEntry {
    bootable: bool,
    type_byte: u8,
    start: u64,
    sectors: u64,
}

fn mbr_entries(sector: &[u8]) -> Vec<MbrEntry> {
    (0..4)
        .map(|slot| {
            let e = &sector[446 + slot * 16..][..16];
            MbrEntry {
                bootable: e[0] == 0x80,
                type_byte: e[4],
                start: u64::from(u32_at(e, 8)),
                sectors: u64::from(u32_at(e, 12)),
            }
        })
        .collect()
}

struct GptHeader {
    disk_guid: Guid,
    first_usable_lba: u64,
    last_usable_lba: u64,
    alternate_lba: u64,
    entries_lba: u64,
    entry_count: u32,
    entry_size: u32,
    entries_crc: u32,
}

fn parse_gpt_header(sector: &[u8], my_lba: u64) -> Result<GptHeader, String> {
    if &sector[..8] != GPT_SIGNATURE {
        return Err("signature missing".into());
    }
    let size = u32_at(sector, 12) as usize;
    if size < 92 || size > sector.len() {
        return Err(format!("header size {size}"));
    }
    let mut copy = sector[..size].to_vec();
    copy[16..20].fill(0);
    if crc32fast::hash(&copy) != u32_at(sector, 16) {
        return Err("checksum mismatch".into());
    }
    if u64_at(sector, 24) != my_lba {
        return Err("header is not at its own position".into());
    }
    Ok(GptHeader {
        first_usable_lba: u64_at(sector, 40),
        last_usable_lba: u64_at(sector, 48),
        alternate_lba: u64_at(sector, 32),
        disk_guid: guid_at(sector, 56),
        entries_lba: u64_at(sector, 72),
        entry_count: u32_at(sector, 80),
        entry_size: u32_at(sector, 84),
        entries_crc: u32_at(sector, 88),
    })
}

/// Recomputes the header checksum.
fn seal_gpt_header(sector: &mut [u8]) {
    let size = u32_at(sector, 12) as usize;
    sector[16..20].fill(0);
    let crc = crc32fast::hash(&sector[..size]);
    sector[16..20].copy_from_slice(&crc.to_le_bytes());
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // one-range sets are intended here
pub(crate) mod tests {
    use super::*;

    const S: u64 = 512;

    fn read_from(disk: &[u8]) -> impl FnMut(u64, &mut [u8]) -> io::Result<()> + '_ {
        move |offset, buf| {
            let o = offset as usize;
            buf.copy_from_slice(&disk[o..o + buf.len()]);
            Ok(())
        }
    }

    fn mbr_entry(sector: &mut [u8], slot: usize, kind: u8, start: u32, sectors: u32) {
        let e = &mut sector[446 + slot * 16..][..16];
        e[4] = kind;
        e[8..12].copy_from_slice(&start.to_le_bytes());
        e[12..16].copy_from_slice(&sectors.to_le_bytes());
    }

    /// An MBR disk like Windows makes it: System Reserved, C:, and an
    /// extended partition with two logical drives.
    pub(crate) fn mbr_disk() -> Vec<u8> {
        let mut disk = vec![0u8; 64 << 20];
        disk[0x1B8..0x1BC].copy_from_slice(&0xA1B2C3D4u32.to_le_bytes());
        mbr_entry(&mut disk, 0, 0x07, 2048, 2048);
        disk[446] = 0x80;
        mbr_entry(&mut disk, 1, 0x07, 4096, 20480);
        mbr_entry(&mut disk, 2, 0x0F, 30720, 40960);
        disk[510..512].copy_from_slice(&MBR_SIGNATURE);
        // EBR 1 at the container start, EBR 2 at 10 MiB into it.
        let ebr1 = 30720 * 512;
        mbr_entry(&mut disk[ebr1..], 0, 0x07, 2048, 8192);
        mbr_entry(&mut disk[ebr1..], 1, 0x05, 20480, 10240);
        disk[ebr1 + 510..ebr1 + 512].copy_from_slice(&MBR_SIGNATURE);
        let ebr2 = (30720 + 20480) * 512;
        mbr_entry(&mut disk[ebr2..], 0, 0x83, 2048, 8192);
        disk[ebr2 + 510..ebr2 + 512].copy_from_slice(&MBR_SIGNATURE);
        disk
    }

    fn write_gpt_header(
        disk: &mut [u8],
        my_lba: u64,
        alternate: u64,
        entries_lba: u64,
        last_usable: u64,
        entries_crc: u32,
    ) {
        let h = &mut disk[(my_lba * S) as usize..][..512];
        h[..8].copy_from_slice(GPT_SIGNATURE);
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&my_lba.to_le_bytes());
        h[32..40].copy_from_slice(&alternate.to_le_bytes());
        h[40..48].copy_from_slice(&34u64.to_le_bytes());
        h[48..56].copy_from_slice(&last_usable.to_le_bytes());
        h[56..72].copy_from_slice(&[0x11; 16]);
        h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        seal_gpt_header(h);
    }

    /// A GPT disk like Windows makes it: EFI, MSR, Windows.
    pub(crate) fn gpt_disk(size: u64) -> Vec<u8> {
        let sectors = size / S;
        let mut disk = vec![0u8; size as usize];
        mbr_entry(&mut disk, 0, PROTECTIVE_TYPE, 1, (sectors - 1) as u32);
        disk[510..512].copy_from_slice(&MBR_SIGNATURE);
        let mut entries = vec![0u8; 128 * 128];
        let parts = [
            (
                gpt_types::EFI_SYSTEM,
                2048u64,
                4095u64,
                "EFI system partition",
            ),
            (
                gpt_types::MICROSOFT_RESERVED,
                4096,
                6143,
                "Microsoft reserved",
            ),
            (gpt_types::BASIC_DATA, 6144, 40959, "Basic data partition"),
        ];
        for (i, (kind, first, last, name)) in parts.iter().enumerate() {
            let e = &mut entries[i * 128..][..128];
            e[..16].copy_from_slice(&guid_bytes(kind));
            e[16..32].copy_from_slice(&[i as u8 + 1; 16]);
            e[32..40].copy_from_slice(&first.to_le_bytes());
            e[40..48].copy_from_slice(&last.to_le_bytes());
            for (c, unit) in name.encode_utf16().enumerate() {
                e[56 + c * 2..58 + c * 2].copy_from_slice(&unit.to_le_bytes());
            }
        }
        let crc = crc32fast::hash(&entries);
        disk[2 * S as usize..34 * S as usize].copy_from_slice(&entries);
        let backup_entries = sectors - 33;
        disk[(backup_entries * S) as usize..((sectors - 1) * S) as usize].copy_from_slice(&entries);
        write_gpt_header(&mut disk, 1, sectors - 1, 2, sectors - 34, crc);
        write_gpt_header(&mut disk, sectors - 1, 1, backup_entries, sectors - 34, crc);
        disk
    }

    fn guid_bytes(text: &str) -> [u8; 16] {
        let hex: Vec<u8> = text
            .split('-')
            .collect::<String>()
            .as_bytes()
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let mut b = [0u8; 16];
        b[..4].copy_from_slice(&[hex[3], hex[2], hex[1], hex[0]]);
        b[4..6].copy_from_slice(&[hex[5], hex[4]]);
        b[6..8].copy_from_slice(&[hex[7], hex[6]]);
        b[8..].copy_from_slice(&hex[8..]);
        b
    }

    pub(crate) fn layout_of(disk: &[u8]) -> DiskLayout {
        DiskLayout::read(&mut read_from(disk), disk.len() as u64, 512).unwrap()
    }

    #[test]
    fn reads_mbr_with_logical_partitions() {
        let disk = mbr_disk();
        let layout = layout_of(&disk);
        assert_eq!(layout.style, PartitionStyle::Mbr);
        assert_eq!(layout.disk_id(), "A1B2C3D4");
        let summary: Vec<_> = layout
            .partitions
            .iter()
            .map(|p| (p.number, p.offset / S, p.length / S))
            .collect();
        assert_eq!(
            summary,
            [
                (1, 2048, 2048),
                (2, 4096, 20480),
                (5, 32768, 8192),
                (6, 30720 + 20480 + 2048, 8192)
            ]
        );
        assert!(matches!(
            layout.partitions[0].kind,
            PartitionKind::Mbr {
                bootable: true,
                logical: false,
                ..
            }
        ));
        assert_eq!(layout.partitions[3].type_name(), "Linux");
        // Head up to the first partition plus both EBRs with their gaps.
        assert_eq!(
            layout.table_ranges().ranges(),
            [0..2048 * S, 30720 * S..32768 * S, 51200 * S..53248 * S]
        );
        assert_eq!(layout.required_size(), 61440 * S);
    }

    #[test]
    fn reads_gpt_and_finds_backup() {
        let disk = gpt_disk(32 << 20);
        let layout = layout_of(&disk);
        assert_eq!(layout.style, PartitionStyle::Gpt);
        assert_eq!(layout.partitions.len(), 3);
        assert_eq!(layout.partitions[0].type_name(), "EFI system");
        let PartitionKind::Gpt { name, .. } = &layout.partitions[2].kind else {
            panic!()
        };
        assert_eq!(name, "Basic data partition");
        let sectors = (32 << 20) / S;
        assert_eq!(
            layout.backup_gpt_range(),
            Some((sectors - 33) * S..sectors * S)
        );
        assert_eq!(
            layout.table_ranges().ranges(),
            [0..2048 * S, (sectors - 33) * S..sectors * S]
        );
        // Between the last partition and the backup GPT.
        assert_eq!(layout.gaps().ranges(), [40960 * S..(sectors - 33) * S]);
    }

    #[test]
    fn rejects_damaged_tables() {
        let mut disk = gpt_disk(8 << 20);
        disk[2 * 512 + 40] ^= 1; // inside the entry array
        let error = DiskLayout::read(&mut read_from(&disk), disk.len() as u64, 512).unwrap_err();
        assert!(error.to_string().contains("checksum"), "{error}");

        let blank = vec![0u8; 1 << 20];
        assert!(matches!(
            DiskLayout::read(&mut read_from(&blank), blank.len() as u64, 512),
            Err(LayoutError::NoPartitionTable)
        ));

        let mut overlapping = mbr_disk();
        mbr_entry(&mut overlapping, 1, 0x07, 3000, 20480);
        assert!(DiskLayout::read(&mut read_from(&overlapping), 64 << 20, 512).is_err());
    }

    #[test]
    fn gpt_moves_to_a_larger_disk() {
        let disk = gpt_disk(32 << 20);
        let layout = layout_of(&disk);
        assert_eq!(layout.retarget(32 << 20).unwrap(), Retarget::default());

        let target_size = 48u64 << 20;
        let plan = layout.retarget(target_size).unwrap();
        let mut target = vec![0xEEu8; target_size as usize];
        target[..disk.len()].copy_from_slice(&disk);
        for range in &plan.skip {
            target[range.start as usize..range.end as usize].fill(0);
        }
        for (offset, data) in plan.patches.iter().chain(&plan.writes) {
            target[*offset as usize..*offset as usize + data.len()].copy_from_slice(data);
        }
        for range in &plan.zero {
            target[range.start as usize..range.end as usize].fill(0);
        }

        let moved = layout_of(&target);
        assert_eq!(moved.partitions, layout.partitions);
        let gpt = moved.gpt.as_ref().unwrap();
        let sectors = target_size / S;
        assert_eq!(gpt.alternate_lba, sectors - 1);
        assert_eq!(gpt.last_usable_lba, sectors - 34);
        assert_eq!(gpt.backup_entries_lba, sectors - 33);
        // The backup header is valid and points back to the primary one.
        let backup =
            parse_gpt_header(&target[((sectors - 1) * S) as usize..], sectors - 1).unwrap();
        assert_eq!(backup.alternate_lba, 1);
        // Protective MBR covers the new size.
        assert_eq!(u32_at(&target, 446 + 12), (sectors - 1) as u32);
        // The last MiB before the new backup GPT holds no stale bytes.
        let stale = target[((target_size - (1 << 20)) as usize)..((sectors - 33) * S) as usize]
            .contains(&0xEE);
        assert!(!stale);
    }

    #[test]
    fn gpt_moves_to_a_smaller_disk_if_the_partitions_fit() {
        let layout = layout_of(&gpt_disk(32 << 20));
        // Last partition ends at sector 40960 (20 MiB).
        assert_eq!(layout.required_size(), (40960 + 33) * S);
        assert!(layout.retarget(21 << 20).is_ok());
        assert!(layout.retarget(20 << 20).is_err());
    }

    #[test]
    fn mbr_on_a_larger_disk_clears_the_tail() {
        let layout = layout_of(&mbr_disk());
        let plan = layout.retarget(128 << 20).unwrap();
        assert!(plan.patches.is_empty() && plan.writes.is_empty());
        assert_eq!(plan.zero, [(127 << 20)..(128 << 20)]);
    }
}
