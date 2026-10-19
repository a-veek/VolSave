use std::io::{Read, Write};

use uuid::Uuid;

use crate::util::{array16, u32_at, u64_at};
use crate::{Error, FORMAT_VERSION, Result};

pub const HASHLIST_MAGIC: [u8; 8] = *b"RSNAPHSH";

const PREFIX_LEN: usize = 48;
const HASH_LEN: usize = 32;

/// Truncated BLAKE3 of one chunk of volume data.
pub type ChunkHash = [u8; 16];

/// Marks a chunk that was not in use when the list was made.
pub const UNUSED_CHUNK: ChunkHash = [0; 16];

pub fn chunk_hash(data: &[u8]) -> ChunkHash {
    array16(blake3::hash(data).as_bytes())
}

/// One hash per fixed-size chunk of a volume. A differential backup compares
/// the current chunks against the list of its base image and stores only
/// the blocks that differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashList {
    pub image_id: Uuid,
    pub chunk_size: u32,
    pub volume_size: u64,
    hashes: Vec<ChunkHash>,
}

impl HashList {
    /// A list for `volume_size` bytes with every chunk marked unused.
    pub fn new(image_id: Uuid, chunk_size: u32, volume_size: u64) -> Result<Self> {
        let count = chunk_count(chunk_size, volume_size)
            .ok_or(Error::InvalidArgument("chunk size must be a power of two"))?;
        Ok(Self {
            image_id,
            chunk_size,
            volume_size,
            hashes: vec![UNUSED_CHUNK; count],
        })
    }

    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }

    pub fn get(&self, chunk: usize) -> Option<&ChunkHash> {
        self.hashes.get(chunk)
    }

    pub fn chunk_of(&self, volume_offset: u64) -> usize {
        (volume_offset / u64::from(self.chunk_size)) as usize
    }

    pub fn set(&mut self, chunk: usize, hash: ChunkHash) -> Result<()> {
        let slot = self
            .hashes
            .get_mut(chunk)
            .ok_or(Error::InvalidArgument("chunk lies past the volume end"))?;
        *slot = hash;
        Ok(())
    }

    /// Hashes `data` located at `volume_offset` chunk by chunk.
    pub fn record(&mut self, volume_offset: u64, data: &[u8]) -> Result<()> {
        if volume_offset % u64::from(self.chunk_size) != 0 {
            return Err(Error::InvalidArgument("offset is not chunk aligned"));
        }
        let first = self.chunk_of(volume_offset);
        for (n, chunk) in data.chunks(self.chunk_size as usize).enumerate() {
            let slot = self
                .hashes
                .get_mut(first + n)
                .ok_or(Error::InvalidArgument("data extends past the volume end"))?;
            *slot = chunk_hash(chunk);
        }
        Ok(())
    }

    pub fn write_to<W: Write>(&self, mut writer: W) -> Result<()> {
        let mut prefix = Vec::with_capacity(PREFIX_LEN);
        prefix.extend_from_slice(&HASHLIST_MAGIC);
        prefix.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        prefix.extend_from_slice(&self.chunk_size.to_le_bytes());
        prefix.extend_from_slice(self.image_id.as_bytes());
        prefix.extend_from_slice(&self.volume_size.to_le_bytes());
        prefix.extend_from_slice(&(self.hashes.len() as u64).to_le_bytes());

        let mut hasher = blake3::Hasher::new();
        hasher.update(&prefix);
        writer.write_all(&prefix)?;
        for hash in &self.hashes {
            hasher.update(hash);
            writer.write_all(hash)?;
        }
        writer.write_all(hasher.finalize().as_bytes())?;
        writer.flush()?;
        Ok(())
    }

    pub fn read_from<R: Read>(mut reader: R) -> Result<Self> {
        let mut prefix = [0u8; PREFIX_LEN];
        reader.read_exact(&mut prefix)?;
        if prefix[..8] != HASHLIST_MAGIC {
            return Err(Error::BadMagic);
        }
        let version = u32_at(&prefix, 8);
        if version != FORMAT_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let chunk_size = u32_at(&prefix, 12);
        let image_id = Uuid::from_bytes(array16(&prefix[16..32]));
        let volume_size = u64_at(&prefix, 32);
        let count = u64_at(&prefix, 40);
        let expected = chunk_count(chunk_size, volume_size).ok_or(Error::HashListCorrupt)?;
        if count != expected as u64 {
            return Err(Error::HashListCorrupt);
        }

        // Grow with the data actually present instead of trusting `count`.
        let raw_len = (expected as u64)
            .checked_mul(16)
            .ok_or(Error::HashListCorrupt)?;
        let mut raw = Vec::new();
        reader.by_ref().take(raw_len).read_to_end(&mut raw)?;
        if raw.len() as u64 != raw_len {
            return Err(Error::HashListCorrupt);
        }
        let mut stored_hash = [0u8; HASH_LEN];
        reader.read_exact(&mut stored_hash)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(&prefix);
        hasher.update(&raw);
        if hasher.finalize().as_bytes() != &stored_hash {
            return Err(Error::HashListCorrupt);
        }

        Ok(Self {
            image_id,
            chunk_size,
            volume_size,
            hashes: raw.chunks_exact(16).map(array16).collect(),
        })
    }
}

fn chunk_count(chunk_size: u32, volume_size: u64) -> Option<usize> {
    if !chunk_size.is_power_of_two() {
        return None;
    }
    usize::try_from(volume_size.div_ceil(u64::from(chunk_size))).ok()
}
