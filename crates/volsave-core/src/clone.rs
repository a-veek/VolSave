//! Copying a disk directly onto another one, without an image in between.

use std::io;

use crate::{AlignedBuf, BlockSource, ImageView, RangeSet};

/// Shows the selected ranges of a live source as if they were an image, so
/// that [`crate::restore_disk`] and [`crate::verify_target`] can copy and
/// check a disk directly. Everything outside `regions` reads as zeros.
pub struct SourceView<'a> {
    source: &'a dyn BlockSource,
    regions: RangeSet,
    /// Reused for the aligned reads; grows to the largest request.
    buffer: AlignedBuf,
}

impl<'a> SourceView<'a> {
    pub fn new(source: &'a dyn BlockSource, regions: RangeSet) -> Self {
        let regions = regions.clamp(0..source.size());
        Self {
            source,
            regions,
            buffer: AlignedBuf::zeroed(0),
        }
    }
}

impl ImageView for SourceView<'_> {
    fn source_size(&self) -> u64 {
        self.source.size()
    }

    fn regions(&self) -> RangeSet {
        self.regions.clone()
    }

    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> volsave_format::Result<()> {
        out.fill(0);
        let end = offset + out.len() as u64;
        let align = self.source.alignment().max(1) as u64;
        let size = self.source.size();
        // The ranges are sorted and disjoint: find the first one that
        // reaches into the request instead of filtering them all (a
        // fragmented volume has hundreds of thousands).
        let ranges = self.regions.ranges();
        let first = ranges.partition_point(|range| range.end <= offset);
        for range in &ranges[first..] {
            if range.start >= end {
                break;
            }
            let (start, stop) = (range.start.max(offset), range.end.min(end));
            // Devices are read unbuffered: offset, length and buffer aligned.
            let from = start / align * align;
            let to = stop.div_ceil(align).saturating_mul(align).min(size);
            let len_aligned = (to - from) as usize;
            if self.buffer.len() < len_aligned {
                self.buffer = AlignedBuf::zeroed(len_aligned);
            }
            let data = &mut self.buffer[..len_aligned];
            self.source.read_at(from, data).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("read error at source offset {from}: {error}"),
                )
            })?;
            let skip = (start - from) as usize;
            let len = (stop - start) as usize;
            let at = (start - offset) as usize;
            out[at..at + len].copy_from_slice(&data[skip..skip + len]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::{BlockTarget, restore, verify_target};

    /// A disk in memory that insists on aligned reads, like a device.
    struct Disk {
        data: Vec<u8>,
        align: usize,
    }

    impl BlockSource for Disk {
        fn size(&self) -> u64 {
            self.data.len() as u64
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            assert_eq!(offset as usize % self.align, 0, "unaligned offset");
            assert_eq!(buf.len() % self.align, 0, "unaligned length");
            let start = offset as usize;
            buf.copy_from_slice(&self.data[start..start + buf.len()]);
            Ok(())
        }
        fn alignment(&self) -> usize {
            self.align
        }
    }

    impl BlockTarget for Disk {
        fn size(&self) -> u64 {
            self.data.len() as u64
        }
        fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
            let start = offset as usize;
            self.data[start..start + data.len()].copy_from_slice(data);
            Ok(())
        }
    }

    #[test]
    fn copies_and_verifies_only_the_selected_ranges() {
        let data: Vec<u8> = (0..4 << 20).map(|i: u32| (i % 253) as u8).collect();
        let source = Disk {
            data: data.clone(),
            align: 512,
        };
        let used = RangeSet::from_ranges([0..4096, 1 << 20..(3 << 20) + 512]);
        let mut target = Disk {
            data: vec![0xEE; data.len()],
            align: 512,
        };
        let cancel = AtomicBool::new(false);
        let mut view = SourceView::new(&source, used.clone());
        let stats = restore(&mut view, &mut target, &cancel, |_| {}).unwrap();
        assert_eq!(stats.bytes_written, used.total_len());
        for range in used.ranges() {
            let r = range.start as usize..range.end as usize;
            assert_eq!(target.data[r.clone()], data[r]);
        }
        // Outside the selection the target is untouched.
        assert!(target.data[8192..1 << 20].iter().all(|&b| b == 0xEE));
        // Reads at odd offsets and into unaligned buffers work too.
        let mut odd = vec![0u8; 700];
        view.read_at(4000, &mut odd).unwrap();
        assert_eq!(odd[..96], data[4000..4096]);
        assert!(odd[96..].iter().all(|&b| b == 0));
        assert_eq!(
            verify_target(&mut view, &target, &stats.copied, &cancel, |_| {}).unwrap(),
            used.total_len()
        );
        target.data[(2 << 20) + 5] ^= 1;
        assert!(verify_target(&mut view, &target, &stats.copied, &cancel, |_| {}).is_err());
    }
}
