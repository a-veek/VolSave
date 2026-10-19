use crate::util::{u32_at, u64_at};
use crate::{Error, Result};

pub const INDEX_TAG: [u8; 4] = *b"IDX1";
pub const FOOTER_MAGIC: [u8; 8] = *b"RSNAPEND";
pub(crate) const FOOTER_LEN: u64 = 32;

const INDEX_PREFIX_LEN: usize = 12;
const ENTRY_LEN: usize = 20;
const CRC_LEN: usize = 4;

/// Location of one block in the image stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexEntry {
    /// Byte offset of the block's data within the source.
    pub offset: u64,
    pub raw_len: u32,
    /// Position of the block header within the image stream.
    pub file_pos: u64,
}

impl IndexEntry {
    pub fn end(&self) -> u64 {
        self.offset + u64::from(self.raw_len)
    }
}

pub(crate) fn encode_index(entries: &[IndexEntry]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(INDEX_PREFIX_LEN + entries.len() * ENTRY_LEN + CRC_LEN);
    buf.extend_from_slice(&INDEX_TAG);
    buf.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for entry in entries {
        buf.extend_from_slice(&entry.offset.to_le_bytes());
        buf.extend_from_slice(&entry.file_pos.to_le_bytes());
        buf.extend_from_slice(&entry.raw_len.to_le_bytes());
    }
    let crc = crc32fast::hash(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    buf
}

pub(crate) fn decode_index(buf: &[u8]) -> Result<Vec<IndexEntry>> {
    if buf.len() < INDEX_PREFIX_LEN + CRC_LEN || buf[0..4] != INDEX_TAG {
        return Err(Error::IndexCorrupt);
    }
    let count = u64_at(buf, 4);
    let expected_len = usize::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(ENTRY_LEN))
        .and_then(|len| len.checked_add(INDEX_PREFIX_LEN + CRC_LEN));
    if expected_len != Some(buf.len()) {
        return Err(Error::IndexCorrupt);
    }
    let (body, crc) = buf.split_at(buf.len() - CRC_LEN);
    if crc32fast::hash(body) != u32_at(crc, 0) {
        return Err(Error::IndexCorrupt);
    }

    let mut entries = Vec::with_capacity(count as usize);
    let mut min_offset = 0u64;
    for raw in body[INDEX_PREFIX_LEN..].chunks_exact(ENTRY_LEN) {
        let entry = IndexEntry {
            offset: u64_at(raw, 0),
            file_pos: u64_at(raw, 8),
            raw_len: u32_at(raw, 16),
        };
        let end = entry.offset.checked_add(u64::from(entry.raw_len));
        if entry.offset < min_offset || entry.raw_len == 0 || end.is_none() {
            return Err(Error::IndexCorrupt);
        }
        min_offset = entry.end();
        entries.push(entry);
    }
    Ok(entries)
}

pub(crate) fn encode_footer(index_pos: u64, index_len: u64) -> [u8; FOOTER_LEN as usize] {
    let mut buf = [0u8; FOOTER_LEN as usize];
    buf[0..8].copy_from_slice(&FOOTER_MAGIC);
    buf[8..16].copy_from_slice(&index_pos.to_le_bytes());
    buf[16..24].copy_from_slice(&index_len.to_le_bytes());
    let crc = crc32fast::hash(&buf[..24]);
    buf[24..28].copy_from_slice(&crc.to_le_bytes());
    buf
}

/// Returns `(index_pos, index_len)` of a valid footer.
pub(crate) fn decode_footer(buf: &[u8; FOOTER_LEN as usize]) -> Option<(u64, u64)> {
    let valid = buf[0..8] == FOOTER_MAGIC
        && crc32fast::hash(&buf[..24]) == u32_at(buf, 24)
        && u32_at(buf, 28) == 0;
    valid.then(|| (u64_at(buf, 8), u64_at(buf, 16)))
}
