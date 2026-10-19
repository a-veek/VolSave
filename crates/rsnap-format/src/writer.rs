use std::io::Write;

use crate::block::{EncodedBlock, encode_block};
use crate::index::{IndexEntry, encode_footer, encode_index};
use crate::util::CountingWriter;
use crate::{Error, FileHeader, Result};

/// Appends blocks to an image. Only needs `Write`, so it can stream to
/// FTP/SFTP or a [`crate::SplitWriter`].
pub struct ImageWriter<W: Write> {
    sink: CountingWriter<W>,
    header: FileHeader,
    index: Vec<IndexEntry>,
    min_offset: u64,
}

impl<W: Write> ImageWriter<W> {
    /// Writes the file header and returns a writer for the blocks.
    pub fn new(sink: W, header: FileHeader) -> Result<Self> {
        let encoded = header.encode()?;
        let mut sink = CountingWriter::new(sink);
        sink.write_all(&encoded)?;
        Ok(Self {
            sink,
            header,
            index: Vec::new(),
            min_offset: 0,
        })
    }

    pub fn header(&self) -> &FileHeader {
        &self.header
    }

    pub fn bytes_written(&self) -> u64 {
        self.sink.position()
    }

    pub fn blocks_written(&self) -> usize {
        self.index.len()
    }

    /// Appends an encoded block. Blocks must arrive in ascending, non-overlapping order.
    pub fn write_block(&mut self, block: &EncodedBlock) -> Result<()> {
        let header = &block.header;
        if header.offset < self.min_offset {
            return Err(Error::OffsetOrder {
                offset: header.offset,
                min_offset: self.min_offset,
            });
        }
        if header.raw_len > self.header.block_size {
            return Err(Error::InvalidArgument(
                "block is larger than the image block size",
            ));
        }
        if block.payload.len() != header.stored_len as usize {
            return Err(Error::InvalidArgument(
                "payload length does not match block header",
            ));
        }
        let end = header
            .offset
            .checked_add(u64::from(header.raw_len))
            .ok_or(Error::InvalidArgument("block offset overflows"))?;

        let file_pos = self.sink.position();
        self.sink.write_all(&header.encode())?;
        self.sink.write_all(&block.payload)?;
        self.index.push(IndexEntry {
            offset: header.offset,
            raw_len: header.raw_len,
            file_pos,
        });
        self.min_offset = end;
        Ok(())
    }

    /// Encodes `data` on the calling thread and appends it.
    pub fn encode_and_write(&mut self, offset: u64, data: &[u8], zstd_level: i32) -> Result<()> {
        let block = encode_block(offset, data, zstd_level)?;
        self.write_block(&block)
    }

    /// Writes index and footer and returns the sink.
    pub fn finish(mut self) -> Result<W> {
        let index_pos = self.sink.position();
        let index = encode_index(&self.index);
        self.sink.write_all(&index)?;
        self.sink
            .write_all(&encode_footer(index_pos, index.len() as u64))?;
        self.sink.flush()?;
        Ok(self.sink.into_inner())
    }
}
