use std::io;
use std::mem::offset_of;

use windows::Win32::System::Ioctl::{
    DISK_ATTRIBUTE_OFFLINE, DISK_ATTRIBUTE_READ_ONLY, DISK_GEOMETRY_EX,
    DRIVE_LAYOUT_INFORMATION_EX, GET_DISK_ATTRIBUTES, IOCTL_DISK_GET_DISK_ATTRIBUTES,
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
    IOCTL_DISK_SET_DISK_ATTRIBUTES, IOCTL_DISK_UPDATE_PROPERTIES, IOCTL_STORAGE_QUERY_PROPERTY,
    PARTITION_INFORMATION_EX, PARTITION_STYLE_GPT, PARTITION_STYLE_MBR, SET_DISK_ATTRIBUTES,
    STORAGE_DEVICE_DESCRIPTOR,
};

use windows::core::GUID;

use crate::util::{Access, format_guid, from_wide, ioctl, open, read_struct, u32_at};

/// Highest `\\.\PhysicalDriveN` number probed.
const MAX_DISKS: u32 = 64;

#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub number: u32,
    /// `\\.\PhysicalDriveN`
    pub path: String,
    pub size: u64,
    pub sector_size: u32,
    pub model: String,
    pub removable: bool,
    /// `None` when the layout could not be read (needs administrator rights).
    pub layout: Option<DiskLayout>,
}

#[derive(Debug, Clone)]
pub enum DiskLayout {
    Mbr {
        signature: u32,
        partitions: Vec<PartitionInfo>,
    },
    Gpt {
        disk_id: String,
        partitions: Vec<PartitionInfo>,
    },
    Raw,
}

impl DiskLayout {
    pub fn partitions(&self) -> &[PartitionInfo] {
        match self {
            DiskLayout::Mbr { partitions, .. } | DiskLayout::Gpt { partitions, .. } => partitions,
            DiskLayout::Raw => &[],
        }
    }
}

#[derive(Debug, Clone)]
pub struct PartitionInfo {
    pub number: u32,
    pub offset: u64,
    pub length: u64,
    pub kind: PartitionKind,
}

#[derive(Debug, Clone)]
pub enum PartitionKind {
    Mbr {
        partition_type: u8,
        bootable: bool,
    },
    Gpt {
        type_id: String,
        partition_id: String,
        name: String,
    },
}

impl PartitionKind {
    /// Short human-readable description of well-known partition types.
    pub fn description(&self) -> String {
        match self {
            PartitionKind::Mbr { partition_type, .. } => match partition_type {
                0x07 => "NTFS/exFAT".into(),
                0x0b | 0x0c => "FAT32".into(),
                0x27 => "Windows RE".into(),
                0x05 | 0x0f => "Extended".into(),
                0x83 => "Linux".into(),
                0xee => "GPT protective".into(),
                other => format!("Type 0x{other:02x}"),
            },
            PartitionKind::Gpt { type_id, .. } => match type_id.as_str() {
                "c12a7328-f81f-11d2-ba4b-00a0c93ec93b" => "EFI system".into(),
                "e3c9e316-0b5c-4db8-817d-f92df00215ae" => "Microsoft reserved".into(),
                "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7" => "Basic data".into(),
                "de94bba4-06d1-4d40-a16a-bfd50179d6ac" => "Windows RE".into(),
                "0fc63daf-8483-4772-8e79-3d69d8477de4" => "Linux".into(),
                other => other.to_string(),
            },
        }
    }
}

/// All physical disks that can be opened.
pub fn list_disks() -> Vec<DiskInfo> {
    (0..MAX_DISKS).filter_map(|n| disk_info(n).ok()).collect()
}

fn disk_info(number: u32) -> io::Result<DiskInfo> {
    let path = format!(r"\\.\PhysicalDrive{number}");
    let device = open(&path, Access::Query, false)?;

    let geometry = ioctl(
        &device,
        IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
        &[],
        size_of::<DISK_GEOMETRY_EX>(),
    )?;
    // SAFETY: DISK_GEOMETRY_EX consists of integers only.
    let geometry: DISK_GEOMETRY_EX = unsafe { read_struct(&geometry, 0)? };
    let (model, removable) = describe(&device).unwrap_or_default();

    // The partition layout needs read access, i.e. administrator rights.
    let layout = open(&path, Access::Read, false)
        .and_then(|device| read_layout(&device))
        .ok();

    Ok(DiskInfo {
        number,
        path,
        size: geometry.DiskSize as u64,
        sector_size: geometry.Geometry.BytesPerSector,
        model,
        removable,
        layout,
    })
}

/// Vendor and product name plus the removable flag.
fn describe(device: &std::fs::File) -> io::Result<(String, bool)> {
    // STORAGE_PROPERTY_QUERY { StorageDeviceProperty = 0, PropertyStandardQuery = 0 }
    let query = [0u8; 12];
    let raw = ioctl(device, IOCTL_STORAGE_QUERY_PROPERTY, &query, 1024)?;
    // STORAGE_DEVICE_DESCRIPTOR holds BOOLEAN fields; a driver may put any
    // byte there, which a Rust `bool` must not hold, so the fields are read
    // at their offsets instead of as the struct.
    const REMOVABLE_MEDIA: usize = 10;
    const VENDOR_ID_OFFSET: usize = 12;
    const PRODUCT_ID_OFFSET: usize = 16;
    if raw.len() < size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IOCTL result too short",
        ));
    }
    let removable = raw[REMOVABLE_MEDIA] != 0;
    let text = |offset: u32| -> String {
        let start = offset as usize;
        if offset == 0 || start >= raw.len() {
            return String::new();
        }
        let end = raw[start..]
            .iter()
            .position(|&b| b == 0)
            .map_or(raw.len(), |n| start + n);
        String::from_utf8_lossy(&raw[start..end]).trim().to_string()
    };
    let model = [
        text(u32_at(&raw, VENDOR_ID_OFFSET)?),
        text(u32_at(&raw, PRODUCT_ID_OFFSET)?),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    Ok((model, removable))
}

/// `PARTITION_INFORMATION_EX` with its BOOLEAN fields as bytes, so that it
/// is valid for any bit pattern a driver returns. The union at the end is
/// kept as raw bytes and picked apart by partition style.
#[repr(C)]
#[derive(Clone, Copy)]
struct PartitionEntry {
    style: i32,
    _pad: u32,
    starting_offset: i64,
    partition_length: i64,
    partition_number: u32,
    _rewrite: u8,
    _is_service: u8,
    _pad2: [u8; 2],
    body: [u8; 112],
}

const _: () = {
    assert!(size_of::<PartitionEntry>() == size_of::<PARTITION_INFORMATION_EX>());
    assert!(
        offset_of!(PartitionEntry, starting_offset)
            == offset_of!(PARTITION_INFORMATION_EX, StartingOffset)
    );
    assert!(
        offset_of!(PartitionEntry, partition_number)
            == offset_of!(PARTITION_INFORMATION_EX, PartitionNumber)
    );
    assert!(offset_of!(PartitionEntry, body) == offset_of!(PARTITION_INFORMATION_EX, Anonymous));
};

fn read_layout(device: &std::fs::File) -> io::Result<DiskLayout> {
    let raw = ioctl(
        device,
        IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
        &[],
        48 + 128 * size_of::<PARTITION_INFORMATION_EX>(),
    )?;
    // SAFETY: the layout header consists of integers and GUIDs only.
    let header: DRIVE_LAYOUT_INFORMATION_EX = unsafe { read_struct(&raw, 0)? };
    let first = offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
    let mut partitions = Vec::new();
    for i in 0..header.PartitionCount as usize {
        // SAFETY: `PartitionEntry` consists of integers and byte arrays;
        // `read_struct` checks the bounds.
        let entry: PartitionEntry =
            unsafe { read_struct(&raw, first + i * size_of::<PartitionEntry>())? };
        if entry.partition_number == 0 || entry.partition_length == 0 {
            continue; // unused MBR slot or extended container
        }
        let kind = if entry.style == PARTITION_STYLE_GPT.0 {
            // PARTITION_INFORMATION_GPT: PartitionType, PartitionId, Attributes, Name[36].
            // SAFETY: a GUID is four integers, valid for any bytes; in bounds.
            let type_id: GUID = unsafe { read_struct(&entry.body, 0)? };
            let partition_id: GUID = unsafe { read_struct(&entry.body, 16)? };
            let name: Vec<u16> = entry.body[40..112]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            PartitionKind::Gpt {
                type_id: format_guid(&type_id),
                partition_id: format_guid(&partition_id),
                name: from_wide(&name),
            }
        } else {
            // PARTITION_INFORMATION_MBR: PartitionType, BootIndicator, ...
            PartitionKind::Mbr {
                partition_type: entry.body[0],
                bootable: entry.body[1] != 0,
            }
        };
        partitions.push(PartitionInfo {
            number: entry.partition_number,
            offset: entry.starting_offset as u64,
            length: entry.partition_length as u64,
            kind,
        });
    }

    Ok(match header.PartitionStyle as i32 {
        style if style == PARTITION_STYLE_MBR.0 => DiskLayout::Mbr {
            // SAFETY: the style says which union member is valid.
            signature: unsafe { header.Anonymous.Mbr.Signature },
            partitions,
        },
        style if style == PARTITION_STYLE_GPT.0 => DiskLayout::Gpt {
            // SAFETY: as above.
            disk_id: format_guid(&unsafe { header.Anonymous.Gpt.DiskId }),
            partitions,
        },
        _ => DiskLayout::Raw,
    })
}

/// Opens a disk for restoring.
pub fn open_disk_for_writing(number: u32) -> io::Result<std::fs::File> {
    open(
        &format!(r"\\.\PhysicalDrive{number}"),
        Access::ReadWrite,
        false,
    )
}

/// Whether the disk is offline and whether it is read-only.
pub fn disk_attributes(disk: &std::fs::File) -> io::Result<(bool, bool)> {
    let raw = ioctl(
        disk,
        IOCTL_DISK_GET_DISK_ATTRIBUTES,
        &[],
        size_of::<GET_DISK_ATTRIBUTES>(),
    )?;
    // SAFETY: GET_DISK_ATTRIBUTES consists of integers only.
    let attributes: GET_DISK_ATTRIBUTES = unsafe { read_struct(&raw, 0)? };
    Ok((
        attributes.Attributes & DISK_ATTRIBUTE_OFFLINE != 0,
        attributes.Attributes & DISK_ATTRIBUTE_READ_ONLY != 0,
    ))
}

/// Takes a disk offline or online and sets its read-only flag. With
/// `persist` the state survives a reboot.
pub fn set_disk_attributes(
    disk: &std::fs::File,
    offline: bool,
    read_only: bool,
    persist: bool,
) -> io::Result<()> {
    let mut attributes = 0;
    if offline {
        attributes |= DISK_ATTRIBUTE_OFFLINE;
    }
    if read_only {
        attributes |= DISK_ATTRIBUTE_READ_ONLY;
    }
    let request = SET_DISK_ATTRIBUTES {
        Version: size_of::<SET_DISK_ATTRIBUTES>() as u32,
        Persist: persist,
        Attributes: attributes,
        AttributesMask: DISK_ATTRIBUTE_OFFLINE | DISK_ATTRIBUTE_READ_ONLY,
        ..Default::default()
    };
    // SAFETY: SET_DISK_ATTRIBUTES is a plain C struct; viewing it as bytes is sound.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (&raw const request).cast::<u8>(),
            size_of::<SET_DISK_ATTRIBUTES>(),
        )
    };
    ioctl(disk, IOCTL_DISK_SET_DISK_ATTRIBUTES, bytes, 0)?;
    Ok(())
}

/// Makes Windows read the partition table again after it was written raw.
pub fn refresh_disk(disk: &std::fs::File) -> io::Result<()> {
    ioctl(disk, IOCTL_DISK_UPDATE_PROPERTIES, &[], 0)?;
    Ok(())
}
