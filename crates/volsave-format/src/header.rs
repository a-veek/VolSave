use std::io::Read;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::util::{array16, u32_at};
use crate::{
    DEFAULT_BLOCK_SIZE, DEFAULT_HASH_CHUNK_SIZE, Error, FORMAT_VERSION, MAX_BLOCK_SIZE, Result,
};

pub const HEADER_MAGIC: [u8; 8] = *b"VSAVEIMG";

const FIXED_LEN: usize = 64;
const HASH_LEN: usize = 32;
const MAX_METADATA_LEN: u32 = 16 << 20;
const FLAG_DIFFERENTIAL: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    #[default]
    Volume,
    Disk,
}

/// Describes what was backed up. Unknown fields from newer writers are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceInfo {
    pub kind: SourceKind,
    /// Device path as seen at backup time, e.g. `\\?\Volume{...}\` or `\\.\PhysicalDrive0`.
    pub device: String,
    pub size_bytes: u64,
    pub sector_size: u32,
    pub filesystem: Option<String>,
    pub label: Option<String>,
    pub disk_number: Option<u32>,
    pub partition_number: Option<u32>,
    pub partition_offset: Option<u64>,
    pub hostname: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageMetadata {
    /// Name and version of the program that wrote the image.
    pub tool: String,
    pub created_unix: u64,
    pub comment: Option<String>,
    pub source: SourceInfo,
    /// For differential images: file name of the full image they build on,
    /// as a hint for finding it. The base image id in the header is binding.
    pub base_image_name: Option<String>,
    /// For disk images: the partitions, for display. Restores read the
    /// partition table from the image data itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<DiskSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiskSummary {
    /// `MBR` or `GPT`.
    pub style: String,
    /// MBR signature (8 hex digits) or GPT disk GUID in braces.
    pub disk_id: String,
    pub model: Option<String>,
    pub partitions: Vec<PartitionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PartitionSummary {
    /// Number in the partition table (MBR logical partitions from 5).
    pub number: u32,
    pub offset: u64,
    pub length: u64,
    /// Partition type, e.g. `EFI system` or `NTFS/exFAT`.
    pub kind: String,
    pub filesystem: Option<String>,
    pub label: Option<String>,
    pub mount_points: Vec<String>,
    /// How the content was read, e.g. `used blocks, VSS snapshot`.
    pub method: String,
    /// Bytes of this partition selected for the image.
    pub bytes_selected: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileHeader {
    pub image_id: Uuid,
    /// Set for differential images: the image this one is based on.
    pub base_image_id: Option<Uuid>,
    pub block_size: u32,
    pub hash_chunk_size: u32,
    pub metadata: ImageMetadata,
}

impl FileHeader {
    /// A full image with a fresh id and default block sizes.
    pub fn new(metadata: ImageMetadata) -> Self {
        Self {
            image_id: Uuid::new_v4(),
            base_image_id: None,
            block_size: DEFAULT_BLOCK_SIZE,
            hash_chunk_size: DEFAULT_HASH_CHUNK_SIZE,
            metadata,
        }
    }

    pub fn is_differential(&self) -> bool {
        self.base_image_id.is_some()
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.block_size == 0 || self.block_size > MAX_BLOCK_SIZE {
            return Err(Error::InvalidArgument(
                "block size must be 1 byte to 16 MiB",
            ));
        }
        if !self.hash_chunk_size.is_power_of_two() {
            return Err(Error::InvalidArgument(
                "hash chunk size must be a power of two",
            ));
        }
        Ok(())
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let metadata = serde_json::to_vec(&self.metadata)?;
        let metadata_len = u32::try_from(metadata.len())
            .ok()
            .filter(|&len| len <= MAX_METADATA_LEN)
            .ok_or(Error::InvalidArgument("metadata too large"))?;
        let total_len = FIXED_LEN + metadata.len() + HASH_LEN;
        let flags = if self.is_differential() {
            FLAG_DIFFERENTIAL
        } else {
            0
        };

        let mut buf = Vec::with_capacity(total_len);
        buf.extend_from_slice(&HEADER_MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf.extend_from_slice(&(total_len as u32).to_le_bytes());
        buf.extend_from_slice(self.image_id.as_bytes());
        buf.extend_from_slice(self.base_image_id.unwrap_or(Uuid::nil()).as_bytes());
        buf.extend_from_slice(&flags.to_le_bytes());
        buf.extend_from_slice(&self.block_size.to_le_bytes());
        buf.extend_from_slice(&self.hash_chunk_size.to_le_bytes());
        buf.extend_from_slice(&metadata_len.to_le_bytes());
        debug_assert_eq!(buf.len(), FIXED_LEN);
        buf.extend_from_slice(&metadata);
        let hash = blake3::hash(&buf);
        buf.extend_from_slice(hash.as_bytes());
        Ok(buf)
    }

    /// Reads and checks a header; returns it with its length in bytes.
    pub(crate) fn read_from<R: Read>(reader: &mut R) -> Result<(Self, u64)> {
        let mut fixed = [0u8; FIXED_LEN];
        reader.read_exact(&mut fixed)?;
        if fixed[..8] != HEADER_MAGIC {
            return Err(Error::BadMagic);
        }
        let version = u32_at(&fixed, 8);
        if version != FORMAT_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let total_len = u32_at(&fixed, 12);
        let metadata_len = u32_at(&fixed, 60);
        if metadata_len > MAX_METADATA_LEN
            || total_len as usize != FIXED_LEN + metadata_len as usize + HASH_LEN
        {
            return Err(Error::HeaderCorrupt);
        }

        let mut rest = vec![0u8; metadata_len as usize + HASH_LEN];
        reader.read_exact(&mut rest)?;
        let (metadata, stored_hash) = rest.split_at(metadata_len as usize);
        let mut hasher = blake3::Hasher::new();
        hasher.update(&fixed);
        hasher.update(metadata);
        if &hasher.finalize().as_bytes()[..] != stored_hash {
            return Err(Error::HeaderCorrupt);
        }

        let flags = u32_at(&fixed, 48);
        if flags & !FLAG_DIFFERENTIAL != 0 {
            return Err(Error::UnsupportedFlags(flags));
        }
        let base_image_id = Uuid::from_bytes(array16(&fixed[32..48]));
        let header = Self {
            image_id: Uuid::from_bytes(array16(&fixed[16..32])),
            base_image_id: (flags & FLAG_DIFFERENTIAL != 0).then_some(base_image_id),
            block_size: u32_at(&fixed, 52),
            hash_chunk_size: u32_at(&fixed, 56),
            metadata: serde_json::from_slice(metadata)?,
        };
        header.validate().map_err(|_| Error::HeaderCorrupt)?;
        Ok((header, u64::from(total_len)))
    }
}
