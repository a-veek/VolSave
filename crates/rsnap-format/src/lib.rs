//! On-disk image format of rsnap.
//!
//! An image is a single byte stream (optionally split into several files):
//!
//! ```text
//! FileHeader | Block* | Index | Footer
//! ```
//!
//! Blocks carry their own volume offset and checksum, so the index can be
//! rebuilt by scanning when the end of the image is lost. The exact binary
//! layout is documented in `docs/FORMAT.md`.

mod block;
mod error;
mod hashlist;
mod header;
mod index;
mod reader;
mod split;
mod util;
mod writer;

pub use block::{BlockEncoding, BlockHeader, EncodedBlock, encode_block};
pub use error::{Error, Result};
pub use hashlist::{ChunkHash, HashList, UNUSED_CHUNK, chunk_hash};
pub use header::{FileHeader, ImageMetadata, SourceInfo, SourceKind};
pub use index::IndexEntry;
pub use reader::{ImageReader, VerifyReport};
pub use split::{SplitReader, SplitWriter, part_path};
pub use writer::ImageWriter;

/// Version written into every file header.
pub const FORMAT_VERSION: u32 = 1;
/// Default amount of volume data per block.
pub const DEFAULT_BLOCK_SIZE: u32 = 1 << 20;
/// Default granularity of the diff hash list.
pub const DEFAULT_HASH_CHUNK_SIZE: u32 = 64 << 10;
/// Largest block a reader accepts.
pub const MAX_BLOCK_SIZE: u32 = 16 << 20;
/// Default zstd compression level.
pub const DEFAULT_ZSTD_LEVEL: i32 = 3;
/// File extension of images (without the dot). Split parts append `.002`, `.003`, ….
pub const IMAGE_EXTENSION: &str = "dimg";
/// File extension of diff hash lists (without the dot).
pub const HASH_LIST_EXTENSION: &str = "dhsh";
