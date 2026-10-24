use std::io;

use windows::Win32::Foundation::ERROR_NO_MORE_FILES;
use windows::Win32::Storage::FileSystem::{
    FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetDiskFreeSpaceExW, GetDiskFreeSpaceW,
    GetDriveTypeW, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW,
    GetVolumePathNamesForVolumeNameW, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
};
use windows::core::PCWSTR;

use crate::util::{
    Access, from_multi_wide, from_wide, i64_at, io_error, ioctl, open, u32_at, wide,
};

/// `GetDriveTypeW` result for removable media.
const DRIVE_REMOVABLE: u32 = 2;

/// Part of a volume on one physical disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskExtent {
    pub disk_number: u32,
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone)]
pub struct VolumeInfo {
    /// `\\?\Volume{GUID}\`
    pub guid_path: String,
    /// Drive letters and folder mount points, e.g. `C:\`.
    pub mount_points: Vec<String>,
    pub label: String,
    pub filesystem: String,
    pub size: u64,
    pub free: u64,
    pub cluster_size: u32,
    pub sector_size: u32,
    pub removable: bool,
    pub extents: Vec<DiskExtent>,
}

impl VolumeInfo {
    /// Path for opening the volume itself (`\\?\Volume{GUID}` without the
    /// trailing backslash, which would open the root directory instead).
    pub fn device_path(&self) -> String {
        self.guid_path.trim_end_matches('\\').to_string()
    }

    /// Drive letter if there is one, otherwise the GUID path.
    pub fn display_name(&self) -> &str {
        self.mount_points.first().unwrap_or(&self.guid_path)
    }

    /// True if VSS can snapshot this file system.
    pub fn supports_vss(&self) -> bool {
        matches!(self.filesystem.as_str(), "NTFS" | "ReFS")
    }
}

/// All mounted volumes that are ready (media present).
pub fn list_volumes() -> io::Result<Vec<VolumeInfo>> {
    let mut name = [0u16; 64];
    // SAFETY: the buffer is valid for the duration of the call.
    let find = unsafe { FindFirstVolumeW(&mut name) }.map_err(io_error)?;
    let mut volumes = Vec::new();
    loop {
        if let Ok(volume) = volume_info(&from_wide(&name)) {
            volumes.push(volume);
        }
        // SAFETY: `find` is a live search handle.
        match unsafe { FindNextVolumeW(find, &mut name) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
            Err(error) => {
                // SAFETY: closing the handle opened above.
                let _ = unsafe { FindVolumeClose(find) };
                return Err(io_error(error));
            }
        }
    }
    // SAFETY: closing the handle opened above.
    let _ = unsafe { FindVolumeClose(find) };
    volumes.sort_by(|a, b| a.display_name().cmp(b.display_name()));
    Ok(volumes)
}

/// Accepts `C`, `C:`, `C:\`, a folder mount point or `\\?\Volume{GUID}\`.
pub fn resolve_volume(spec: &str) -> io::Result<VolumeInfo> {
    let spec = spec.trim();
    if spec.to_ascii_lowercase().starts_with(r"\\?\volume{") {
        return volume_info(&format!("{}\\", spec.trim_end_matches('\\')));
    }
    let mount_point = match spec.len() {
        1 => format!("{spec}:\\"),
        _ if spec.ends_with('\\') => spec.to_string(),
        _ => format!("{spec}\\"),
    };
    let mut name = [0u16; 64];
    let mount_point = wide(&mount_point);
    // SAFETY: both buffers are valid for the call.
    unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(mount_point.as_ptr()), &mut name) }
        .map_err(io_error)?;
    volume_info(&from_wide(&name))
}

fn volume_info(guid_path: &str) -> io::Result<VolumeInfo> {
    let root = wide(guid_path);
    let root = PCWSTR(root.as_ptr());

    let mut label = [0u16; 261];
    let mut filesystem = [0u16; 261];
    // SAFETY: all buffers are valid for the call.
    unsafe {
        GetVolumeInformationW(
            root,
            Some(&mut label),
            None,
            None,
            None,
            Some(&mut filesystem),
        )
    }
    .map_err(io_error)?;

    let (mut sectors_per_cluster, mut sector_size, mut free_clusters, mut total_clusters) =
        (0, 0, 0, 0);
    // SAFETY: the out-pointers reference locals.
    unsafe {
        GetDiskFreeSpaceW(
            root,
            Some(&mut sectors_per_cluster),
            Some(&mut sector_size),
            Some(&mut free_clusters),
            Some(&mut total_clusters),
        )
    }
    .map_err(io_error)?;
    let mut free = 0u64;
    // SAFETY: the out-pointer references a local.
    let _ = unsafe { GetDiskFreeSpaceExW(root, None, None, Some(&mut free)) };

    let cluster_size = sectors_per_cluster * sector_size;
    let extents = disk_extents(guid_path).unwrap_or_default();
    let size = match extents.iter().map(|e| e.length).sum::<u64>() {
        0 => u64::from(total_clusters) * u64::from(cluster_size),
        sum => sum,
    };
    Ok(VolumeInfo {
        guid_path: guid_path.to_string(),
        mount_points: mount_points(root),
        label: from_wide(&label),
        filesystem: from_wide(&filesystem),
        size,
        free,
        cluster_size,
        sector_size,
        // SAFETY: `root` is a valid NUL-terminated string.
        removable: unsafe { GetDriveTypeW(root) } == DRIVE_REMOVABLE,
        extents,
    })
}

fn mount_points(root: PCWSTR) -> Vec<String> {
    let mut buf = vec![0u16; 1024];
    let mut needed = 0u32;
    // SAFETY: buffer and length pointer are valid for the call.
    match unsafe { GetVolumePathNamesForVolumeNameW(root, Some(&mut buf), &mut needed) } {
        Ok(()) => from_multi_wide(&buf),
        Err(_) => Vec::new(),
    }
}

fn disk_extents(guid_path: &str) -> io::Result<Vec<DiskExtent>> {
    let device = open(guid_path.trim_end_matches('\\'), Access::Query, false)?;
    let raw = ioctl(
        &device,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
        &[],
        8 + 24 * 4,
    )?;
    // VOLUME_DISK_EXTENTS: u32 count, padding, then 24-byte DISK_EXTENT entries.
    let count = u32_at(&raw, 0)? as usize;
    (0..count)
        .map(|i| {
            let at = 8 + i * 24;
            Ok(DiskExtent {
                disk_number: u32_at(&raw, at)?,
                offset: i64_at(&raw, at + 8)? as u64,
                length: i64_at(&raw, at + 16)? as u64,
            })
        })
        .collect()
}
