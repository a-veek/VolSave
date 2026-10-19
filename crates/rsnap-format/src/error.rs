use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("not an rsnap image (bad magic)")]
    BadMagic,

    #[error("unsupported format version {0}")]
    UnsupportedVersion(u32),

    #[error("unsupported flags 0x{0:08x}")]
    UnsupportedFlags(u32),

    #[error("image header is corrupt")]
    HeaderCorrupt,

    #[error("invalid image metadata: {0}")]
    Metadata(#[from] serde_json::Error),

    #[error("block at file position {file_pos} is corrupt: {reason}")]
    BlockCorrupt { file_pos: u64, reason: &'static str },

    #[error("checksum mismatch in block at volume offset {offset}")]
    BlockHashMismatch { offset: u64 },

    #[error("decompression failed for block at volume offset {offset}: {source}")]
    Decompress {
        offset: u64,
        #[source]
        source: io::Error,
    },

    #[error("block index is missing or corrupt")]
    IndexCorrupt,

    #[error(
        "blocks must be written in ascending order without overlap (offset {offset} < {min_offset})"
    )]
    OffsetOrder { offset: u64, min_offset: u64 },

    #[error("hash list is corrupt")]
    HashListCorrupt,

    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
