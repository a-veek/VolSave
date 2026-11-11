use std::cell::{Cell, RefCell};

use crate::util::{u32_at, u64_at};
use crate::{Error, MAX_BLOCK_SIZE, Result};

pub const BLOCK_TAG: [u8; 4] = *b"BLK1";
pub(crate) const BLOCK_HEADER_LEN: usize = 60;

const FLAG_ZSTD: u32 = 1;
const FLAG_ZERO: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockEncoding {
    /// Payload is the raw data (compression would not have saved space).
    Stored,
    /// Payload is a zstd frame.
    Zstd,
    /// Data is all zeros; there is no payload.
    Zero,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockHeader {
    /// Byte offset of the data within the source volume or disk.
    pub offset: u64,
    pub raw_len: u32,
    pub stored_len: u32,
    pub encoding: BlockEncoding,
    /// BLAKE3 of the raw data.
    pub raw_hash: [u8; 32],
}

impl BlockHeader {
    pub(crate) fn encode(&self) -> [u8; BLOCK_HEADER_LEN] {
        let flags = match self.encoding {
            BlockEncoding::Stored => 0,
            BlockEncoding::Zstd => FLAG_ZSTD,
            BlockEncoding::Zero => FLAG_ZERO,
        };
        let mut buf = [0u8; BLOCK_HEADER_LEN];
        buf[0..4].copy_from_slice(&BLOCK_TAG);
        buf[4..8].copy_from_slice(&flags.to_le_bytes());
        buf[8..16].copy_from_slice(&self.offset.to_le_bytes());
        buf[16..20].copy_from_slice(&self.raw_len.to_le_bytes());
        buf[20..24].copy_from_slice(&self.stored_len.to_le_bytes());
        buf[24..56].copy_from_slice(&self.raw_hash);
        let crc = crc32fast::hash(&buf[..56]);
        buf[56..60].copy_from_slice(&crc.to_le_bytes());
        buf
    }

    pub(crate) fn decode(buf: &[u8; BLOCK_HEADER_LEN], file_pos: u64) -> Result<Self> {
        let corrupt = |reason| Error::BlockCorrupt { file_pos, reason };
        if buf[0..4] != BLOCK_TAG {
            return Err(corrupt("bad block tag"));
        }
        if crc32fast::hash(&buf[..56]) != u32_at(buf, 56) {
            return Err(corrupt("header checksum mismatch"));
        }
        let encoding = match u32_at(buf, 4) {
            0 => BlockEncoding::Stored,
            FLAG_ZSTD => BlockEncoding::Zstd,
            FLAG_ZERO => BlockEncoding::Zero,
            flags => return Err(Error::UnsupportedFlags(flags)),
        };
        let raw_len = u32_at(buf, 16);
        let stored_len = u32_at(buf, 20);
        if raw_len == 0 || raw_len > MAX_BLOCK_SIZE {
            return Err(corrupt("invalid raw length"));
        }
        let stored_len_ok = match encoding {
            BlockEncoding::Stored => stored_len == raw_len,
            BlockEncoding::Zstd => stored_len > 0 && stored_len < raw_len,
            BlockEncoding::Zero => stored_len == 0,
        };
        if !stored_len_ok {
            return Err(corrupt("invalid stored length"));
        }
        let mut raw_hash = [0u8; 32];
        raw_hash.copy_from_slice(&buf[24..56]);
        Ok(Self {
            offset: u64_at(buf, 8),
            raw_len,
            stored_len,
            encoding,
            raw_hash,
        })
    }
}

/// A block ready to be appended to an image. Encoding is independent of the
/// writer so callers can compress on several threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedBlock {
    pub header: BlockHeader,
    pub payload: Vec<u8>,
}

thread_local! {
    /// zstd contexts are expensive to set up (several MiB of workspace), so
    /// each thread keeps one of each instead of building them per block.
    static COMPRESSOR: RefCell<Option<(i32, zstd::bulk::Compressor<'static>)>> =
        const { RefCell::new(None) };
    static DECOMPRESSOR: RefCell<Option<zstd::bulk::Decompressor<'static>>> =
        const { RefCell::new(None) };
    /// Hash of an all-zero block of the last seen length.
    static ZERO_HASH: Cell<Option<(usize, [u8; 32])>> = const { Cell::new(None) };
}

fn compress(data: &[u8], level: i32) -> std::io::Result<Vec<u8>> {
    COMPRESSOR.with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.as_mut() {
            Some((current, compressor)) if *current == level => compressor.compress(data),
            _ => {
                let mut compressor = zstd::bulk::Compressor::new(level)?;
                let compressed = compressor.compress(data)?;
                *slot = Some((level, compressor));
                Ok(compressed)
            }
        }
    })
}

fn decompress(payload: &[u8], capacity: usize) -> std::io::Result<Vec<u8>> {
    DECOMPRESSOR.with(|slot| {
        let mut slot = slot.borrow_mut();
        let decompressor = match slot.as_mut() {
            Some(decompressor) => decompressor,
            None => slot.insert(zstd::bulk::Decompressor::new()?),
        };
        decompressor.decompress(payload, capacity)
    })
}

/// BLAKE3 of `len` zero bytes; blocks of one image share one length.
fn zero_hash(len: usize) -> [u8; 32] {
    ZERO_HASH.with(|cache| match cache.get() {
        Some((cached_len, hash)) if cached_len == len => hash,
        _ => {
            let hash = *blake3::hash(&vec![0u8; len]).as_bytes();
            cache.set(Some((len, hash)));
            hash
        }
    })
}

/// Compresses one block of volume data located at `offset`.
pub fn encode_block(offset: u64, data: &[u8], zstd_level: i32) -> Result<EncodedBlock> {
    let raw_len = u32::try_from(data.len())
        .ok()
        .filter(|&len| len > 0 && len <= MAX_BLOCK_SIZE)
        .ok_or(Error::InvalidArgument(
            "block length must be 1 byte to 16 MiB",
        ))?;
    let (encoding, payload, raw_hash) = if data.iter().all(|&byte| byte == 0) {
        (BlockEncoding::Zero, Vec::new(), zero_hash(data.len()))
    } else {
        let raw_hash = *blake3::hash(data).as_bytes();
        let compressed = compress(data, zstd_level)?;
        if compressed.len() < data.len() {
            (BlockEncoding::Zstd, compressed, raw_hash)
        } else {
            (BlockEncoding::Stored, data.to_vec(), raw_hash)
        }
    };
    Ok(EncodedBlock {
        header: BlockHeader {
            offset,
            raw_len,
            stored_len: payload.len() as u32,
            encoding,
            raw_hash,
        },
        payload,
    })
}

/// Restores the raw data of a block and checks it against its hash.
/// `payload` has `header.stored_len` bytes.
pub(crate) fn decode_payload(
    header: &BlockHeader,
    payload: Vec<u8>,
    file_pos: u64,
) -> Result<Vec<u8>> {
    let raw_len = header.raw_len as usize;
    let mismatch = Error::BlockHashMismatch {
        offset: header.offset,
    };
    let data = match header.encoding {
        BlockEncoding::Zero => {
            // The header's checksum already covers the hash; comparing it
            // with the known zero hash avoids hashing a fresh buffer.
            if zero_hash(raw_len) != header.raw_hash {
                return Err(mismatch);
            }
            return Ok(vec![0u8; raw_len]);
        }
        BlockEncoding::Stored => payload,
        BlockEncoding::Zstd => {
            let data = decompress(&payload, raw_len).map_err(|source| Error::Decompress {
                offset: header.offset,
                source,
            })?;
            if data.len() != raw_len {
                return Err(Error::BlockCorrupt {
                    file_pos,
                    reason: "decompressed length mismatch",
                });
            }
            data
        }
    };
    if blake3::hash(&data).as_bytes() != &header.raw_hash {
        return Err(mismatch);
    }
    Ok(data)
}
