//! Opening images (with their full image, if differential) and hash lists.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rsnap_core::{ImageView, LayeredImage, SingleImage};
use rsnap_format::{
    FileHeader, HASH_LIST_EXTENSION, HashList, IMAGE_EXTENSION, ImageReader, SplitReader,
};
use uuid::Uuid;

pub(crate) type Reader = ImageReader<SplitReader>;

pub(crate) fn open_image(path: &Path) -> Result<Reader> {
    let source =
        SplitReader::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let reader = ImageReader::open(source)
        .with_context(|| format!("{} is not a valid image", path.display()))?;
    if reader.index_rebuilt() {
        warn!(
            "the index of {} was missing and has been rebuilt; the image may be incomplete",
            path.display()
        );
    }
    Ok(reader)
}

/// An image ready for restoring: a full image, or a differential image
/// layered on top of its full image.
pub(crate) struct OpenedImage {
    pub view: Box<dyn ImageView>,
    pub header: FileHeader,
    /// Path of the full image, for differential images.
    pub base_path: Option<PathBuf>,
}

pub(crate) fn open_view(path: &Path, base: Option<&Path>) -> Result<OpenedImage> {
    let reader = open_image(path)?;
    let header = reader.header().clone();
    let Some(base_id) = header.base_image_id else {
        return Ok(OpenedImage {
            view: Box::new(SingleImage::new(reader)),
            header,
            base_path: None,
        });
    };
    let base_path = match base {
        Some(base) => base.to_path_buf(),
        None => find_base(path, &header, base_id)?,
    };
    let base_reader = open_image(&base_path)?;
    let layered = LayeredImage::new(base_reader, reader).with_context(|| {
        format!(
            "cannot combine {} with {}",
            path.display(),
            base_path.display()
        )
    })?;
    Ok(OpenedImage {
        view: Box::new(layered),
        header,
        base_path: Some(base_path),
    })
}

/// Looks for the full image of a differential image: first under the name
/// stored in the image, then among all images in the same folder.
pub(crate) fn find_base(diff: &Path, header: &FileHeader, base_id: Uuid) -> Result<PathBuf> {
    let folder = diff
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut candidates = Vec::new();
    if let Some(name) = &header.metadata.base_image_name {
        candidates.push(folder.join(name));
    }
    if let Ok(entries) = fs::read_dir(folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_image = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(IMAGE_EXTENSION));
            if is_image && path != diff {
                candidates.push(path);
            }
        }
    }
    for candidate in candidates {
        let id = File::open(&candidate)
            .ok()
            .and_then(|file| ImageReader::open(BufReader::new(file)).ok())
            .map(|reader| reader.header().image_id);
        if id == Some(base_id) {
            return Ok(candidate);
        }
    }
    bail!(
        "the full image {base_id} for this differential image was not found in {}; use --base",
        folder.display()
    )
}

/// Default hash list path for an image: the image path with the hash list extension.
pub(crate) fn hash_path(image: &Path) -> PathBuf {
    image.with_extension(HASH_LIST_EXTENSION)
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn read_hash_list(path: &Path) -> Result<HashList> {
    let file =
        File::open(path).with_context(|| format!("cannot open hash list {}", path.display()))?;
    HashList::read_from(BufReader::new(file))
        .with_context(|| format!("{} is not a valid hash list", path.display()))
}

/// Writes via a temporary file so an existing list is only replaced by a complete one.
pub(crate) fn write_hash_list(path: &Path, list: &HashList) -> Result<()> {
    let temp = path.with_extension(format!("{HASH_LIST_EXTENSION}.tmp"));
    let mut file = BufWriter::new(
        File::create(&temp).with_context(|| format!("cannot create {}", temp.display()))?,
    );
    list.write_to(&mut file)?;
    file.flush()?;
    file.into_inner()?.sync_all()?;
    fs::rename(&temp, path).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    use rsnap_core::{BackupOptions, BlockSource, RangeSet, backup};
    use rsnap_format::{ImageMetadata, ImageWriter, SplitWriter};

    struct Zeros;

    impl BlockSource for Zeros {
        fn size(&self) -> u64 {
            1 << 20
        }

        fn read_at(&self, _: u64, buf: &mut [u8]) -> std::io::Result<()> {
            buf.fill(1);
            Ok(())
        }
    }

    fn make_image(path: &Path, header: FileHeader) -> HashList {
        let writer = ImageWriter::new(SplitWriter::create(path, None).unwrap(), header).unwrap();
        let output = backup(
            &Zeros,
            &RangeSet::full(1 << 20),
            writer,
            None,
            &BackupOptions::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        output.sink.finish().unwrap();
        output.hashes
    }

    #[test]
    fn base_is_found_by_id_in_the_same_folder() {
        let dir = tempfile::tempdir().unwrap();
        let full_path = dir.path().join("monday.dimg");
        let full = make_image(&full_path, FileHeader::new(ImageMetadata::default()));
        make_image(
            &dir.path().join("other.dimg"),
            FileHeader::new(ImageMetadata::default()),
        );

        let mut header = FileHeader::new(ImageMetadata::default());
        header.base_image_id = Some(full.image_id);
        let diff_path = dir.path().join("tuesday.dimg");
        let writer =
            ImageWriter::new(SplitWriter::create(&diff_path, None).unwrap(), header).unwrap();
        writer.finish().unwrap().finish().unwrap();

        let opened = open_view(&diff_path, None).unwrap();
        assert_eq!(opened.base_path.as_deref(), Some(full_path.as_path()));
    }

    #[test]
    fn hash_list_roundtrip_via_files() {
        let dir = tempfile::tempdir().unwrap();
        let list = make_image(
            &dir.path().join("a.dimg"),
            FileHeader::new(ImageMetadata::default()),
        );
        let path = hash_path(&dir.path().join("a.dimg"));
        assert_eq!(path.file_name().unwrap(), "a.dhsh");
        write_hash_list(&path, &list).unwrap();
        assert_eq!(read_hash_list(&path).unwrap(), list);
        assert!(!dir.path().join("a.dhsh.tmp").exists());
    }
}
