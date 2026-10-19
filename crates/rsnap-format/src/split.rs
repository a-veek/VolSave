use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Path of part `part` (1-based) of a split image: part 1 is `base` itself,
/// later parts get `.002`, `.003`, … appended.
pub fn part_path(base: &Path, part: u32) -> PathBuf {
    if part <= 1 {
        return base.to_path_buf();
    }
    let mut name = OsString::from(base.as_os_str());
    name.push(format!(".{part:03}"));
    PathBuf::from(name)
}

/// Writes one logical stream into files of at most `max_part_len` bytes.
pub struct SplitWriter {
    base: PathBuf,
    max_part_len: u64,
    part: u32,
    part_len: u64,
    file: BufWriter<File>,
}

impl SplitWriter {
    /// Creates (or truncates) the first part. Leftover parts of an older image
    /// with the same name are deleted so readers do not pick them up.
    /// `max_part_len = None` writes a single file.
    pub fn create(base: impl Into<PathBuf>, max_part_len: Option<u64>) -> io::Result<Self> {
        let base = base.into();
        let max_part_len = max_part_len.unwrap_or(u64::MAX);
        if max_part_len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "part size must not be 0",
            ));
        }
        let file = BufWriter::new(File::create(&base)?);
        let mut stale = 2;
        while fs::remove_file(part_path(&base, stale)).is_ok() {
            stale += 1;
        }
        Ok(Self {
            base,
            max_part_len,
            part: 1,
            part_len: 0,
            file,
        })
    }

    /// Number of parts written so far.
    pub fn parts(&self) -> u32 {
        self.part
    }

    /// Flushes and syncs the last part; returns the number of parts.
    pub fn finish(mut self) -> io::Result<u32> {
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        Ok(self.part)
    }
}

impl Write for SplitWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.part_len == self.max_part_len {
            self.file.flush()?;
            self.file.get_ref().sync_all()?;
            self.part += 1;
            self.file = BufWriter::new(File::create(part_path(&self.base, self.part))?);
            self.part_len = 0;
        }
        let room = self.max_part_len - self.part_len;
        let len = (buf.len() as u64).min(room) as usize;
        let written = self.file.write(&buf[..len])?;
        self.part_len += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Reads the parts written by [`SplitWriter`] as one seekable stream.
pub struct SplitReader {
    parts: Vec<PathBuf>,
    /// Stream position where each part starts.
    starts: Vec<u64>,
    total_len: u64,
    pos: u64,
    open: Option<(usize, File)>,
}

impl SplitReader {
    /// Opens `base` and every following part that exists.
    pub fn open(base: impl AsRef<Path>) -> io::Result<Self> {
        let base = base.as_ref();
        let mut parts = Vec::new();
        let mut starts = Vec::new();
        let mut total_len = 0u64;
        let mut part = 1;
        loop {
            let path = part_path(base, part);
            let len = match fs::metadata(&path) {
                Ok(meta) => meta.len(),
                Err(error) if part == 1 => return Err(error),
                Err(_) => break,
            };
            starts.push(total_len);
            parts.push(path);
            total_len += len;
            part += 1;
        }
        Ok(Self {
            parts,
            starts,
            total_len,
            pos: 0,
            open: None,
        })
    }

    pub fn part_count(&self) -> usize {
        self.parts.len()
    }

    pub fn len(&self) -> u64 {
        self.total_len
    }

    pub fn is_empty(&self) -> bool {
        self.total_len == 0
    }
}

impl Read for SplitReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.total_len {
            return Ok(0);
        }
        let part = self.starts.partition_point(|&start| start <= self.pos) - 1;
        if self.open.as_ref().map(|(index, _)| *index) != Some(part) {
            self.open = Some((part, File::open(&self.parts[part])?));
        }
        let part_end = self.starts.get(part + 1).copied().unwrap_or(self.total_len);
        let local = self.pos - self.starts[part];
        let len = (buf.len() as u64).min(part_end - self.pos) as usize;
        let (_, file) = self.open.as_mut().expect("part was opened above");
        file.seek(SeekFrom::Start(local))?;
        let read = file.read(&mut buf[..len])?;
        self.pos += read as u64;
        Ok(read)
    }
}

impl Seek for SplitReader {
    fn seek(&mut self, target: SeekFrom) -> io::Result<u64> {
        let new_pos = match target {
            SeekFrom::Start(pos) => Some(pos),
            SeekFrom::End(delta) => self.total_len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        };
        let new_pos = new_pos.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "seek before start of image")
        })?;
        self.pos = new_pos;
        Ok(new_pos)
    }
}
