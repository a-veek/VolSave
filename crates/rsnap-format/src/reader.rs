use std::io::{Read, Seek, SeekFrom};

use crate::block::{BLOCK_HEADER_LEN, BlockHeader, decode_payload};
use crate::index::{FOOTER_LEN, IndexEntry, decode_footer, decode_index};
use crate::{Error, FileHeader, Result};

/// Random access to the blocks of an image.
pub struct ImageReader<R: Read + Seek> {
    source: R,
    header: FileHeader,
    index: Vec<IndexEntry>,
    index_rebuilt: bool,
}

/// Outcome of [`ImageReader::verify`].
#[derive(Debug, Default)]
pub struct VerifyReport {
    pub blocks_ok: usize,
    pub bytes_ok: u64,
    /// Blocks that could not be read, with the reason.
    pub errors: Vec<(IndexEntry, String)>,
}

impl VerifyReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

impl<R: Read + Seek> ImageReader<R> {
    /// Opens an image. If the index at the end is missing or damaged (for
    /// example after an aborted backup), it is rebuilt by scanning the blocks;
    /// [`Self::index_rebuilt`] then returns `true`.
    pub fn open(mut source: R) -> Result<Self> {
        source.seek(SeekFrom::Start(0))?;
        let (header, data_start) = FileHeader::read_from(&mut source)?;
        let len = source.seek(SeekFrom::End(0))?;
        let (index, index_rebuilt) = match read_index(&mut source, data_start, len) {
            Ok(index) => (index, false),
            Err(_) => (scan_blocks(&mut source, data_start, len)?, true),
        };
        Ok(Self {
            source,
            header,
            index,
            index_rebuilt,
        })
    }

    pub fn header(&self) -> &FileHeader {
        &self.header
    }

    /// All blocks in ascending volume order.
    pub fn blocks(&self) -> &[IndexEntry] {
        &self.index
    }

    pub fn index_rebuilt(&self) -> bool {
        self.index_rebuilt
    }

    /// Sum of the raw lengths of all blocks.
    pub fn data_bytes(&self) -> u64 {
        self.index
            .iter()
            .map(|entry| u64::from(entry.raw_len))
            .sum()
    }

    /// The block that contains `volume_offset`, if the image stores it.
    pub fn find_block(&self, volume_offset: u64) -> Option<IndexEntry> {
        let after = self
            .index
            .partition_point(|entry| entry.offset <= volume_offset);
        let entry = *self.index.get(after.checked_sub(1)?)?;
        (volume_offset < entry.end()).then_some(entry)
    }

    /// Reads, decompresses and checks one block.
    pub fn read_block(&mut self, entry: &IndexEntry) -> Result<Vec<u8>> {
        self.source.seek(SeekFrom::Start(entry.file_pos))?;
        let mut raw_header = [0u8; BLOCK_HEADER_LEN];
        self.source.read_exact(&mut raw_header)?;
        let header = BlockHeader::decode(&raw_header, entry.file_pos)?;
        if header.offset != entry.offset || header.raw_len != entry.raw_len {
            return Err(Error::BlockCorrupt {
                file_pos: entry.file_pos,
                reason: "block header does not match index",
            });
        }
        let mut payload = vec![0u8; header.stored_len as usize];
        self.source.read_exact(&mut payload)?;
        decode_payload(&header, &payload, entry.file_pos)
    }

    /// Reads every block and checks its checksum. `progress` gets
    /// `(blocks done, blocks total)` after each block.
    pub fn verify(&mut self, mut progress: impl FnMut(usize, usize)) -> VerifyReport {
        let mut report = VerifyReport::default();
        let total = self.index.len();
        for done in 0..total {
            let entry = self.index[done];
            match self.read_block(&entry) {
                Ok(_) => {
                    report.blocks_ok += 1;
                    report.bytes_ok += u64::from(entry.raw_len);
                }
                Err(error) => report.errors.push((entry, error.to_string())),
            }
            progress(done + 1, total);
        }
        report
    }

    pub fn into_inner(self) -> R {
        self.source
    }
}

fn read_index<R: Read + Seek>(
    source: &mut R,
    data_start: u64,
    len: u64,
) -> Result<Vec<IndexEntry>> {
    let footer_pos = len
        .checked_sub(FOOTER_LEN)
        .filter(|&pos| pos >= data_start)
        .ok_or(Error::IndexCorrupt)?;
    source.seek(SeekFrom::Start(footer_pos))?;
    let mut footer = [0u8; FOOTER_LEN as usize];
    source.read_exact(&mut footer)?;
    let (index_pos, index_len) = decode_footer(&footer).ok_or(Error::IndexCorrupt)?;
    if index_pos < data_start || index_pos.checked_add(index_len) != Some(footer_pos) {
        return Err(Error::IndexCorrupt);
    }

    source.seek(SeekFrom::Start(index_pos))?;
    let mut raw = vec![0u8; usize::try_from(index_len).map_err(|_| Error::IndexCorrupt)?];
    source.read_exact(&mut raw)?;
    let index = decode_index(&raw)?;
    let positions_ok = index
        .iter()
        .all(|entry| entry.file_pos >= data_start && entry.file_pos < index_pos);
    if !positions_ok {
        return Err(Error::IndexCorrupt);
    }
    Ok(index)
}

/// Rebuilds the index by walking the blocks; stops at the first damaged or
/// truncated block.
fn scan_blocks<R: Read + Seek>(
    source: &mut R,
    data_start: u64,
    len: u64,
) -> Result<Vec<IndexEntry>> {
    let mut index = Vec::new();
    let mut pos = data_start;
    let mut min_offset = 0u64;
    while pos + BLOCK_HEADER_LEN as u64 <= len {
        source.seek(SeekFrom::Start(pos))?;
        let mut raw_header = [0u8; BLOCK_HEADER_LEN];
        source.read_exact(&mut raw_header)?;
        let Ok(header) = BlockHeader::decode(&raw_header, pos) else {
            break;
        };
        let next = pos + BLOCK_HEADER_LEN as u64 + u64::from(header.stored_len);
        if next > len || header.offset < min_offset {
            break;
        }
        index.push(IndexEntry {
            offset: header.offset,
            raw_len: header.raw_len,
            file_pos: pos,
        });
        min_offset = header.offset + u64::from(header.raw_len);
        pos = next;
    }
    Ok(index)
}
