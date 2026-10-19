use std::io::{Read, Seek};

use rsnap_format::{ImageReader, IndexEntry};

use crate::RangeSet;

/// Read access to the volume content an image (or image chain) describes.
pub trait ImageView: Send {
    /// Size of the backed-up volume.
    fn source_size(&self) -> u64;

    /// Byte ranges of the volume the image stores.
    fn regions(&self) -> RangeSet;

    /// Fills `out` with the volume bytes at `offset`; bytes the image does
    /// not store read as zeros.
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> rsnap_format::Result<()>;
}

/// A single full or differential image.
pub struct SingleImage<R: Read + Seek> {
    reader: ImageReader<R>,
    /// The last decoded block; consecutive reads usually hit it.
    cached: Option<(u64, Vec<u8>)>,
}

impl<R: Read + Seek> SingleImage<R> {
    pub fn new(reader: ImageReader<R>) -> Self {
        Self {
            reader,
            cached: None,
        }
    }

    pub fn reader(&self) -> &ImageReader<R> {
        &self.reader
    }

    pub fn reader_mut(&mut self) -> &mut ImageReader<R> {
        &mut self.reader
    }

    /// Copies the parts of `offset..offset + out.len()` this image stores
    /// into `out` and leaves the rest of `out` untouched.
    fn overlay(&mut self, offset: u64, out: &mut [u8]) -> rsnap_format::Result<()> {
        let end = offset + out.len() as u64;
        let mut i = self.reader.blocks().partition_point(|b| b.end() <= offset);
        while let Some(&entry) = self.reader.blocks().get(i) {
            if entry.offset >= end {
                break;
            }
            let from = entry.offset.max(offset);
            let to = entry.end().min(end);
            let data = self.block(&entry)?;
            out[(from - offset) as usize..(to - offset) as usize].copy_from_slice(
                &data[(from - entry.offset) as usize..(to - entry.offset) as usize],
            );
            i += 1;
        }
        Ok(())
    }

    fn block(&mut self, entry: &IndexEntry) -> rsnap_format::Result<&[u8]> {
        if self.cached.as_ref().map(|(offset, _)| *offset) != Some(entry.offset) {
            let data = self.reader.read_block(entry)?;
            self.cached = Some((entry.offset, data));
        }
        Ok(&self.cached.as_ref().expect("cache was filled above").1)
    }
}

impl<R: Read + Seek + Send> ImageView for SingleImage<R> {
    fn source_size(&self) -> u64 {
        match self.reader.header().metadata.source.size_bytes {
            0 => self.reader.blocks().last().map_or(0, IndexEntry::end),
            size => size,
        }
    }

    fn regions(&self) -> RangeSet {
        RangeSet::from_ranges(self.reader.blocks().iter().map(|b| b.offset..b.end()))
    }

    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> rsnap_format::Result<()> {
        out.fill(0);
        self.overlay(offset, out)
    }
}

/// A differential image on top of its full image.
pub struct LayeredImage<R: Read + Seek> {
    base: SingleImage<R>,
    diff: SingleImage<R>,
}

#[derive(Debug, thiserror::Error)]
pub enum LayerError {
    #[error("the image is not differential")]
    NotDifferential,
    #[error("the full image {found} is not the base of this differential image (needs {expected})")]
    WrongBase { expected: String, found: String },
    #[error("the base image is itself differential")]
    BaseIsDifferential,
}

impl<R: Read + Seek> LayeredImage<R> {
    pub fn new(base: ImageReader<R>, diff: ImageReader<R>) -> Result<Self, LayerError> {
        let expected = diff
            .header()
            .base_image_id
            .ok_or(LayerError::NotDifferential)?;
        if base.header().is_differential() {
            return Err(LayerError::BaseIsDifferential);
        }
        if base.header().image_id != expected {
            return Err(LayerError::WrongBase {
                expected: expected.to_string(),
                found: base.header().image_id.to_string(),
            });
        }
        Ok(Self {
            base: SingleImage::new(base),
            diff: SingleImage::new(diff),
        })
    }

    pub fn base(&self) -> &ImageReader<R> {
        self.base.reader()
    }

    pub fn diff(&self) -> &ImageReader<R> {
        self.diff.reader()
    }
}

impl<R: Read + Seek + Send> ImageView for LayeredImage<R> {
    fn source_size(&self) -> u64 {
        self.diff.source_size()
    }

    fn regions(&self) -> RangeSet {
        self.base.regions().union(&self.diff.regions())
    }

    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> rsnap_format::Result<()> {
        out.fill(0);
        self.base.overlay(offset, out)?;
        self.diff.overlay(offset, out)
    }
}
