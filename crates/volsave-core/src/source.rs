use std::fs::File;
use std::io;
use std::ops::Range;

/// Something that can be backed up: a volume, a disk or a plain file.
pub trait BlockSource: Sync {
    /// Size in bytes.
    fn size(&self) -> u64;

    /// Fills `buf` with the bytes at `offset`. Callers keep offset, length
    /// and buffer address aligned to [`Self::alignment`].
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    /// Required I/O alignment in bytes (the sector size for devices).
    fn alignment(&self) -> usize {
        1
    }
}

/// Something an image can be restored onto.
pub trait BlockTarget {
    fn size(&self) -> u64;

    fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()>;

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Reads `from..` from `tail` and everything before it from `main`.
///
/// Used when a snapshot does not expose the end of its volume: the NTFS
/// backup boot sector after the last cluster is then read from the live
/// volume, where it does not change.
pub struct PatchedSource<'a> {
    main: &'a dyn BlockSource,
    tail: &'a dyn BlockSource,
    from: u64,
}

impl<'a> PatchedSource<'a> {
    pub fn new(main: &'a dyn BlockSource, tail: &'a dyn BlockSource, from: u64) -> Self {
        Self { main, tail, from }
    }
}

impl BlockSource for PatchedSource<'_> {
    fn size(&self) -> u64 {
        self.main.size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset + buf.len() as u64;
        if end <= self.from {
            return self.main.read_at(offset, buf);
        }
        if offset >= self.from {
            return self.tail.read_at(offset, buf);
        }
        let (head, rest) = buf.split_at_mut((self.from - offset) as usize);
        self.main.read_at(offset, head)?;
        self.tail.read_at(self.from, rest)
    }

    fn alignment(&self) -> usize {
        self.main.alignment().max(self.tail.alignment())
    }
}

/// Reads a disk from `base` except for the ranges mapped to other sources,
/// e.g. partitions read through VSS snapshots of their volumes. A mapped
/// range starts at offset 0 of its source.
pub struct MappedSource<'a> {
    base: &'a dyn BlockSource,
    maps: Vec<(Range<u64>, &'a dyn BlockSource)>,
}

impl<'a> MappedSource<'a> {
    pub fn new(base: &'a dyn BlockSource) -> Self {
        Self {
            base,
            maps: Vec::new(),
        }
    }

    /// Reads `range` from `source`. Ranges must not overlap.
    pub fn map(&mut self, range: Range<u64>, source: &'a dyn BlockSource) {
        assert!(
            self.maps
                .iter()
                .all(|(r, _)| r.end <= range.start || range.end <= r.start),
            "mapped ranges overlap"
        );
        self.maps.push((range, source));
        self.maps.sort_by_key(|(r, _)| r.start);
    }
}

impl BlockSource for MappedSource<'_> {
    fn size(&self) -> u64 {
        self.base.size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut pos = offset;
        let end = offset + buf.len() as u64;
        while pos < end {
            let at = (pos - offset) as usize;
            let mapped = self.maps.iter().find(|(r, _)| r.end > pos);
            match mapped {
                Some((range, source)) if range.start <= pos => {
                    let to = range.end.min(end);
                    source.read_at(pos - range.start, &mut buf[at..(to - offset) as usize])?;
                    pos = to;
                }
                _ => {
                    let to = mapped.map_or(end, |(r, _)| r.start.min(end));
                    self.base
                        .read_at(pos, &mut buf[at..(to - offset) as usize])?;
                    pos = to;
                }
            }
        }
        Ok(())
    }

    fn alignment(&self) -> usize {
        self.maps
            .iter()
            .map(|(_, s)| s.alignment())
            .fold(self.base.alignment(), usize::max)
    }
}

/// A regular file used as a source, e.g. a raw volume dump.
pub struct FileSource {
    file: File,
    size: u64,
}

impl FileSource {
    pub fn new(file: File) -> io::Result<Self> {
        let size = file.metadata()?.len();
        Ok(Self { file, size })
    }
}

impl BlockSource for FileSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        read_exact_at(&self.file, buf, offset)
    }
}

/// A regular file used as a restore target; it is sized to the volume, so
/// areas the image does not cover read as zeros.
pub struct FileTarget {
    file: File,
    size: u64,
}

impl FileTarget {
    pub fn new(file: File, size: u64) -> io::Result<Self> {
        file.set_len(size)?;
        Ok(Self { file, size })
    }
}

impl BlockTarget for FileTarget {
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

#[cfg(unix)]
pub fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
pub fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut std::mem::take(&mut buf)[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
pub fn write_all_at(file: &File, data: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, data, offset)
}

#[cfg(windows)]
pub fn write_all_at(file: &File, mut data: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        match file.seek_write(data, offset)? {
            0 => return Err(io::ErrorKind::WriteZero.into()),
            n => {
                data = &data[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Constant(u8);

    impl BlockSource for Constant {
        fn size(&self) -> u64 {
            100
        }

        fn read_at(&self, _: u64, buf: &mut [u8]) -> io::Result<()> {
            buf.fill(self.0);
            Ok(())
        }
    }

    #[test]
    fn patched_source_splits_reads_at_the_boundary() {
        let (main, tail) = (Constant(1), Constant(2));
        let source = PatchedSource::new(&main, &tail, 90);
        let mut buf = [0u8; 20];
        source.read_at(80, &mut buf).unwrap();
        assert_eq!(&buf[..10], &[1; 10]);
        assert_eq!(&buf[10..], &[2; 10]);
        source.read_at(0, &mut buf[..10]).unwrap();
        assert_eq!(&buf[..10], &[1; 10]);
        source.read_at(95, &mut buf[..5]).unwrap();
        assert_eq!(&buf[..5], &[2; 5]);
    }

    struct Offsets;

    impl BlockSource for Offsets {
        fn size(&self) -> u64 {
            100
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            for (i, b) in buf.iter_mut().enumerate() {
                *b = (offset as usize + i) as u8;
            }
            Ok(())
        }
    }

    #[test]
    fn mapped_source_reads_ranges_from_their_sources() {
        let (base, a, b) = (Constant(9), Offsets, Constant(7));
        let mut source = MappedSource::new(&base);
        source.map(40..50, &b);
        source.map(10..20, &a);
        let mut buf = [0u8; 60];
        source.read_at(5, &mut buf).unwrap();
        assert_eq!(&buf[..5], &[9; 5]);
        assert_eq!(&buf[5..15], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(&buf[15..35], &[9; 20]);
        assert_eq!(&buf[35..45], &[7; 10]);
        assert_eq!(&buf[45..], &[9; 15]);
        source.read_at(15, &mut buf[..2]).unwrap();
        assert_eq!(&buf[..2], &[5, 6]);
    }
}
