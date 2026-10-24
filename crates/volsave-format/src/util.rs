use std::io::{self, Write};

pub(crate) fn u32_at(buf: &[u8], at: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&buf[at..at + 4]);
    u32::from_le_bytes(bytes)
}

pub(crate) fn u64_at(buf: &[u8], at: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[at..at + 8]);
    u64::from_le_bytes(bytes)
}

pub(crate) fn array16(buf: &[u8]) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&buf[..16]);
    bytes
}

/// Tracks the stream position of a sink that cannot seek (FTP, SFTP, split files).
pub(crate) struct CountingWriter<W> {
    inner: W,
    position: u64,
}

impl<W> CountingWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self { inner, position: 0 }
    }

    pub(crate) fn position(&self) -> u64 {
        self.position
    }

    pub(crate) fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.position += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
