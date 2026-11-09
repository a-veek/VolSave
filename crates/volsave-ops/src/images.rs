//! Opening images (with their full image, if differential) and hash lists.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::Reporter;
use crate::store::{ImageSource, store};
use anyhow::{Context, Result, bail};
use uuid::Uuid;
use volsave_core::{DiskLayout, ImageView, LayeredImage, Partition, PartitionView, SingleImage};
use volsave_format::{
    FileHeader, HASH_LIST_EXTENSION, HashList, IMAGE_EXTENSION, ImageReader, SourceKind,
    is_encrypted,
};

pub type Reader = ImageReader<Box<dyn ImageSource>>;

/// Said when a file is encrypted and the store cannot decrypt it.
fn encrypted_error(path: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "{} is encrypted; open it with VolSave Pro and its password",
        path.display()
    )
}

/// Whether the file starts like an encrypted file, as seen without decryption.
fn looks_encrypted(path: &Path) -> bool {
    let mut start = [0u8; 8];
    store()
        .local_path(path)
        .and_then(|local| fs::File::open(local).ok())
        .is_some_and(|mut file| file.read_exact(&mut start).is_ok() && is_encrypted(&start))
}

pub fn open_image(path: &Path, rep: &mut dyn Reporter) -> Result<Reader> {
    let source = store()
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let reader = match ImageReader::open(source) {
        Ok(reader) => reader,
        Err(_) if looks_encrypted(path) => return Err(encrypted_error(path)),
        Err(error) => {
            return Err(error).with_context(|| format!("{} is not a valid image", path.display()));
        }
    };
    if reader.index_rebuilt() {
        warn!(
            rep,
            "the index of {} was missing and has been rebuilt; the image may be incomplete",
            path.display()
        );
    }
    Ok(reader)
}

/// An image ready for restoring: a full image, or a differential image
/// layered on top of its full image.
pub struct OpenedImage {
    pub view: Box<dyn ImageView>,
    pub header: FileHeader,
    /// Path of the full image, for differential images.
    pub base_path: Option<PathBuf>,
}

impl OpenedImage {
    pub fn is_disk(&self) -> bool {
        self.header.metadata.source.kind == SourceKind::Disk
    }

    /// The partition table stored in a disk image.
    pub fn layout(&mut self) -> Result<DiskLayout> {
        let size = self.view.source_size();
        let sector_size = self.header.metadata.source.sector_size.max(512);
        let view = &mut self.view;
        DiskLayout::read(
            &mut |offset, buf| view.read_at(offset, buf).map_err(io::Error::other),
            size,
            sector_size,
        )
        .context("cannot read the partition table stored in the image")
    }

    /// Narrows a disk image to one of its partitions.
    pub fn into_partition(mut self, number: u32) -> Result<(Box<dyn ImageView>, Partition)> {
        anyhow::ensure!(self.is_disk(), "--partition needs a disk image");
        let layout = self.layout()?;
        let partition = layout
            .partitions
            .into_iter()
            .find(|p| p.number == number)
            .with_context(|| format!("the image has no partition {number} (see `volsave info`)"))?;
        let view = PartitionView::new(self.view, partition.range());
        Ok((Box::new(view), partition))
    }
}

pub fn open_view(path: &Path, base: Option<&Path>, rep: &mut dyn Reporter) -> Result<OpenedImage> {
    let reader = open_image(path, rep)?;
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
    let base_reader = open_image(&base_path, rep)?;
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
pub fn find_base(diff: &Path, header: &FileHeader, base_id: Uuid) -> Result<PathBuf> {
    let folder = diff
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut candidates = Vec::new();
    if let Some(name) = &header.metadata.base_image_name {
        candidates.push(folder.join(name));
    }
    let store = store();
    if let Ok(entries) = store.list_folder(folder) {
        for path in entries {
            let is_image = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(IMAGE_EXTENSION));
            if is_image && path != diff {
                candidates.push(path);
            }
        }
    }
    for candidate in candidates {
        let id = store
            .open(&candidate)
            .ok()
            .and_then(|source| ImageReader::open(source).ok())
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
pub fn hash_path(image: &Path) -> PathBuf {
    image.with_extension(HASH_LIST_EXTENSION)
}

pub fn read_hash_list(path: &Path) -> Result<HashList> {
    let data = store()
        .read_file(path)
        .with_context(|| format!("cannot open hash list {}", path.display()))?;
    if is_encrypted(&data) {
        return Err(encrypted_error(path));
    }
    HashList::read_from(data.as_slice())
        .with_context(|| format!("{} is not a valid hash list", path.display()))
}

/// An existing list is only replaced by a complete one.
pub fn write_hash_list(path: &Path, list: &HashList) -> Result<()> {
    let mut data = Vec::new();
    list.write_to(&mut data)?;
    store()
        .write_file(path, &data)
        .with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    use volsave_core::{BackupOptions, BlockSource, RangeSet, backup};
    use volsave_format::{ImageMetadata, ImageWriter, SplitWriter};

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

        let opened = open_view(&diff_path, None, &mut crate::Silent).unwrap();
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

/// Refuses to create `output` over an existing file unless `overwrite`, and
/// never over one of `inputs` (images or hash lists the command reads).
pub fn check_output(output: &Path, overwrite: bool, inputs: &[&Path]) -> Result<()> {
    let store = store();
    if !store.exists(output) {
        return Ok(());
    }
    // Local files are compared by their real location, others by name.
    let identity = |path: &Path| match store.local_path(path) {
        Some(local) => fs::canonicalize(local).ok(),
        None => Some(PathBuf::from(path.to_string_lossy().to_lowercase())),
    };
    let target = identity(output);
    if let Some(input) = inputs
        .iter()
        .find(|path| target.is_some() && identity(path) == target)
    {
        bail!(
            "{} is also read by this command ({}) and would be destroyed; choose another file name",
            output.display(),
            input.display()
        );
    }
    if !overwrite {
        bail!(
            "{} already exists; choose another file name (or allow overwriting it, \
             --overwrite on the command line)",
            output.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn existing_files_and_inputs_are_protected() {
        let dir = std::env::temp_dir().join(format!("volsave-check-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let full = dir.join("full.dimg");
        let other = dir.join("other.dimg");
        fs::write(&full, b"x").unwrap();
        assert!(check_output(&other, false, &[&full]).is_ok());
        fs::write(&other, b"y").unwrap();
        assert!(check_output(&other, false, &[&full]).is_err());
        assert!(check_output(&other, true, &[&full]).is_ok());
        // The same file under another spelling is still an input.
        let spelled = dir.join(".").join("full.dimg");
        assert!(check_output(&spelled, true, &[&full]).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod encrypted_tests {
    use super::*;

    #[test]
    fn encrypted_files_are_named_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let mut data = volsave_format::ENCRYPTED_MAGIC.to_vec();
        data.extend([0u8; 600]);
        let image = dir.path().join("e.dimg");
        fs::write(&image, &data).unwrap();
        let error = open_image(&image, &mut crate::Silent).err().unwrap();
        assert!(format!("{error:#}").contains("is encrypted"), "{error:#}");
        let hashes = dir.path().join("e.dhsh");
        fs::write(&hashes, &data).unwrap();
        let error = read_hash_list(&hashes).unwrap_err();
        assert!(format!("{error:#}").contains("is encrypted"), "{error:#}");
    }
}
