use std::fs::File;
use std::io;

use volsave_core::RangeSet;
use windows::Win32::System::Ioctl::{
    FSCTL_GET_RETRIEVAL_POINTER_BASE, FSCTL_GET_RETRIEVAL_POINTERS, FSCTL_GET_VOLUME_BITMAP,
};

use crate::util::{Access, i64_at, ioctl, ioctl_into, open, u32_at};

/// Files whose content is never needed after a restore.
const SYSTEM_FILES: [&str; 3] = ["pagefile.sys", "swapfile.sys", "hiberfil.sys"];

/// Bitmap bytes requested per `FSCTL_GET_VOLUME_BITMAP` call (8M clusters).
const BITMAP_CHUNK: usize = 1 << 20;

/// What the file system of a volume occupies.
#[derive(Debug, Clone)]
pub struct Allocation {
    /// Byte ranges that hold data.
    pub used: RangeSet,
    /// End of the last full cluster. The tail after it (NTFS keeps its
    /// backup boot sector there) is part of `used`, but VSS snapshots may
    /// not expose it.
    pub clusters_end: u64,
}

/// Byte ranges of `volume` that hold data, according to its file system.
///
/// Also includes everything the cluster bitmap does not describe: the area
/// before the first cluster (FAT reserved sectors and tables) and the tail
/// after the last full cluster.
pub fn used_ranges(
    volume: &File,
    volume_size: u64,
    cluster_size: u32,
    sector_size: u32,
) -> io::Result<Allocation> {
    let cluster_size = u64::from(cluster_size);
    // Offset of cluster 0 in sectors; non-zero on FAT/exFAT.
    let base = match ioctl(volume, FSCTL_GET_RETRIEVAL_POINTER_BASE, &[], 8) {
        Ok(raw) => i64_at(&raw, 0)? as u64 * u64::from(sector_size),
        Err(_) => 0,
    };

    let mut used = RangeSet::new();
    used.push(0..base);
    let mut output = vec![0u8; 16 + BITMAP_CHUNK];
    let mut next_lcn = 0i64;
    let mut total_clusters;
    loop {
        let (returned, more) = ioctl_into(
            volume,
            FSCTL_GET_VOLUME_BITMAP,
            &next_lcn.to_le_bytes(),
            &mut output,
        )?;
        // VOLUME_BITMAP_BUFFER: StartingLcn, BitmapSize (in clusters), bitmap bytes.
        let start_lcn = i64_at(&output, 0)? as u64;
        total_clusters = start_lcn + i64_at(&output, 8)? as u64;
        let bitmap = &output[16..returned.max(16)];
        let clusters = (bitmap.len() as u64 * 8).min(total_clusters - start_lcn);
        push_runs(&mut used, bitmap, clusters, start_lcn, base, cluster_size);
        if !more {
            break;
        }
        next_lcn = (start_lcn + clusters) as i64;
    }

    let clusters_end = (base + total_clusters * cluster_size).min(volume_size);
    used.push(clusters_end..volume_size);
    Ok(Allocation {
        used: used.clamp(0..volume_size),
        clusters_end,
    })
}

/// Converts runs of set bits into byte ranges.
fn push_runs(
    used: &mut RangeSet,
    bitmap: &[u8],
    clusters: u64,
    start_lcn: u64,
    base: u64,
    cluster_size: u64,
) {
    let bit = |i: u64| bitmap[(i / 8) as usize] & (1 << (i % 8)) != 0;
    let mut i = 0;
    while i < clusters {
        if bitmap[(i / 8) as usize] == 0 && i % 8 == 0 && i + 8 <= clusters {
            i += 8;
            continue;
        }
        if !bit(i) {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < clusters && bit(i) {
            i += 1;
        }
        let start = base + (start_lcn + run_start) * cluster_size;
        used.push(start..start + (i - run_start) * cluster_size);
    }
}

/// Byte ranges occupied by the file at `path` on its volume.
pub fn file_extents(path: &str, cluster_size: u32, base: u64) -> io::Result<RangeSet> {
    let file = open(path, Access::Query, false)?;
    let cluster_size = u64::from(cluster_size);
    let mut output = vec![0u8; 16 + 16 * 1024];
    let mut next_vcn = 0i64;
    let mut ranges = Vec::new();
    loop {
        let (returned, more) = ioctl_into(
            &file,
            FSCTL_GET_RETRIEVAL_POINTERS,
            &next_vcn.to_le_bytes(),
            &mut output,
        )?;
        // RETRIEVAL_POINTERS_BUFFER: ExtentCount, StartingVcn, then (NextVcn, Lcn) pairs.
        let count = u32_at(&output, 0)? as usize;
        let mut vcn = i64_at(&output, 8)?;
        for i in 0..count {
            let at = 16 + i * 16;
            if at + 16 > returned {
                break;
            }
            let next = i64_at(&output, at)?;
            let lcn = i64_at(&output, at + 8)?;
            if lcn >= 0 {
                let start = base + lcn as u64 * cluster_size;
                ranges.push(start..start + (next - vcn) as u64 * cluster_size);
            }
            vcn = next;
        }
        if !more || count == 0 {
            break;
        }
        next_vcn = vcn;
    }
    Ok(RangeSet::from_ranges(ranges))
}

/// Extents of the page, swap and hibernation files under `root` (a path
/// ending in `\`). Missing files are skipped.
pub fn system_file_extents(root: &str, cluster_size: u32) -> RangeSet {
    SYSTEM_FILES
        .iter()
        .filter_map(|name| file_extents(&format!("{root}{name}"), cluster_size, 0).ok())
        .fold(RangeSet::new(), |all, extents| all.union(&extents))
}
