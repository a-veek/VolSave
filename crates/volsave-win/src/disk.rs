use std::io;
use std::mem::offset_of;

use windows::Win32::System::Ioctl::{
    DISK_GEOMETRY_EX, DRIVE_LAYOUT_INFORMATION_EX, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
    IOCTL_DISK_GET_DRIVE_LAYOUT_EX, IOCTL_STORAGE_QUERY_PROPERTY, PARTITION_INFORMATION_EX,
    PARTITION_STYLE_GPT, PARTITION_STYLE_MBR, STORAGE_DEVICE_DESCRIPTOR,
};

use crate::util::{Access, format_guid, from_wide, ioctl, open, read_struct};

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
                0x05 | 0x0f => "Erweitert".into(),
                0x83 => "Linux".into(),
                0xee => "GPT-Schutz".into(),
                other => format!("Typ 0x{other:02x}"),
            },
            PartitionKind::Gpt { type_id, .. } => match type_id.as_str() {
                "c12a7328-f81f-11d2-ba4b-00a0c93ec93b" => "EFI-System".into(),
                "e3c9e316-0b5c-4db8-817d-f92df00215ae" => "Microsoft reserviert".into(),
                "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7" => "Basisdaten".into(),
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
    // SAFETY: STORAGE_DEVICE_DESCRIPTOR consists of integers only.
    let descriptor: STORAGE_DEVICE_DESCRIPTOR = unsafe { read_struct(&raw, 0)? };
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
        text(descriptor.VendorIdOffset),
        text(descriptor.ProductIdOffset),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    Ok((model, descriptor.RemovableMedia))
}

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
        // SAFETY: as above; `read_struct` checks the bounds.
        let entry: PARTITION_INFORMATION_EX =
            unsafe { read_struct(&raw, first + i * size_of::<PARTITION_INFORMATION_EX>())? };
        if entry.PartitionNumber == 0 || entry.PartitionLength == 0 {
            continue; // unused MBR slot or extended container
        }
        let kind = if entry.PartitionStyle == PARTITION_STYLE_GPT {
            // SAFETY: the style says which union member is valid.
            let gpt = unsafe { entry.Anonymous.Gpt };
            PartitionKind::Gpt {
                type_id: format_guid(&gpt.PartitionType),
                partition_id: format_guid(&gpt.PartitionId),
                name: from_wide(&gpt.Name),
            }
        } else {
            // SAFETY: as above.
            let mbr = unsafe { entry.Anonymous.Mbr };
            PartitionKind::Mbr {
                partition_type: mbr.PartitionType,
                bootable: mbr.BootIndicator,
            }
        };
        partitions.push(PartitionInfo {
            number: entry.PartitionNumber,
            offset: entry.StartingOffset as u64,
            length: entry.PartitionLength as u64,
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
