//! Where image files and hash lists are kept.
//!
//! Every operation reaches files through an [`ImageStore`]. The default,
//! [`LocalStore`], uses the file system (local drives and UNC paths).
//! Programs built on VolSave can install their own store with
//! [`set_store`], for example one that keeps images elsewhere or transforms
//! their bytes; images keep their format either way.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use volsave_format::{SplitReader, SplitWriter, part_path};

/// An image being written, split into parts or not.
pub trait ImageSink: Write + Send {
    /// Completes the image; returns how many files it occupies.
    fn finish(self: Box<Self>) -> io::Result<u32>;
}

impl ImageSink for SplitWriter {
    fn finish(self: Box<Self>) -> io::Result<u32> {
        SplitWriter::finish(*self)
    }
}

/// An image being read: all its parts as one seekable stream.
pub trait ImageSource: Read + Seek + Send {}

impl<T: Read + Seek + Send> ImageSource for T {}

/// Access to images and hash lists.
pub trait ImageStore: Send + Sync {
    /// Creates an image, in parts of `split_bytes` if given.
    fn create(&self, path: &Path, split_bytes: Option<u64>) -> io::Result<Box<dyn ImageSink>>;
    /// Opens an image with all its parts.
    fn open(&self, path: &Path) -> io::Result<Box<dyn ImageSource>>;
    /// Whether the file (the first part of an image) exists.
    fn exists(&self, path: &Path) -> bool;
    /// Deletes an image with all its parts, as far as possible.
    fn remove_image(&self, path: &Path);
    /// Reads a small file, such as a hash list, completely.
    fn read_file(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// Writes a small file completely; an existing file is only replaced by
    /// a complete new one.
    fn write_file(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    /// The files in `folder`.
    fn list_folder(&self, folder: &Path) -> io::Result<Vec<PathBuf>>;
    /// Where the file lies in the local file system, `None` if it is not a
    /// local file (used to keep an image off the volume being restored, and
    /// to compare paths).
    fn local_path(&self, path: &Path) -> Option<PathBuf> {
        Some(path.to_path_buf())
    }
}

/// Images in the file system.
pub struct LocalStore;

impl ImageStore for LocalStore {
    fn create(&self, path: &Path, split_bytes: Option<u64>) -> io::Result<Box<dyn ImageSink>> {
        Ok(Box::new(SplitWriter::create(path, split_bytes)?))
    }

    fn open(&self, path: &Path) -> io::Result<Box<dyn ImageSource>> {
        Ok(Box::new(SplitReader::open(path)?))
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn remove_image(&self, path: &Path) {
        let mut part = 1;
        while fs::remove_file(part_path(path, part)).is_ok() {
            part += 1;
        }
    }

    fn read_file(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut data = Vec::new();
        File::open(path)?.read_to_end(&mut data)?;
        Ok(data)
    }

    fn write_file(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let mut temp = path.as_os_str().to_owned();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        let mut file = BufWriter::new(File::create(&temp)?);
        file.write_all(data)?;
        file.flush()?;
        file.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        fs::rename(&temp, path)
    }

    fn list_folder(&self, folder: &Path) -> io::Result<Vec<PathBuf>> {
        Ok(fs::read_dir(folder)?
            .flatten()
            .map(|entry| entry.path())
            .collect())
    }
}

static STORE: RwLock<Option<Arc<dyn ImageStore>>> = RwLock::new(None);

/// Makes `store` the store of all following operations.
pub fn set_store(store: Arc<dyn ImageStore>) {
    *STORE.write().unwrap_or_else(|e| e.into_inner()) = Some(store);
}

/// The store installed with [`set_store`], or [`LocalStore`].
pub fn store() -> Arc<dyn ImageStore> {
    STORE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| Arc::new(LocalStore))
}
