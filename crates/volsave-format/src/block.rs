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

/// Compresses one block of volume data located at `offset`.
pub fn encode_block(offset: u64, data: &[u8], zstd_level: i32) -> Result<EncodedBlock> {
    let raw_len = u32::try_from(data.len())
        .ok()
        .filter(|&len| len > 0 && len <= MAX_BLOCK_SIZE)
        .ok_or(Error::InvalidArgument(
            "block length must be 1 byte to 16 MiB",
        ))?;
    let raw_hash = *blake3::hash(data).as_bytes();
    let (encoding, payload) = if data.iter().all(|&byte| byte == 0) {
        (BlockEncoding::Zero, Vec::new())
    } else {
        let compressed = zstd::bulk::compress(data, zstd_level)?;
        if compressed.len() < data.len() {
            (BlockEncoding::Zstd, compressed)
        } else {
            (BlockEncoding::Stored, data.to_vec())
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
pub(crate) fn decode_payload(
    header: &BlockHeader,
    payload: &[u8],
    file_pos: u64,
) -> Result<Vec<u8>> {
    let raw_len = header.raw_len as usize;
    let data = match header.encoding {
        BlockEncoding::Zero => vec![0u8; raw_len],
        BlockEncoding::Stored => payload.to_vec(),
        BlockEncoding::Zstd => {
            let data =
                zstd::bulk::decompress(payload, raw_len).map_err(|source| Error::Decompress {
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
        return Err(Error::BlockHashMismatch {
            offset: header.offset,
        });
    }
    Ok(data)
}
