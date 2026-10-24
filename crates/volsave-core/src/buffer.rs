use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// Alignment of [`AlignedBuf`]; covers 512-byte and 4K-sector devices.
pub const BUFFER_ALIGNMENT: usize = 4096;

/// Zero-initialised heap buffer aligned to [`BUFFER_ALIGNMENT`], as required
/// for unbuffered device I/O on Windows.
pub struct AlignedBuf {
    ptr: NonNull<u8>,
    len: usize,
    layout: Layout,
}

// The buffer exclusively owns its allocation.
unsafe impl Send for AlignedBuf {}
unsafe impl Sync for AlignedBuf {}

impl AlignedBuf {
    pub fn zeroed(len: usize) -> Self {
        let layout =
            Layout::from_size_align(len.max(1), BUFFER_ALIGNMENT).expect("buffer size overflows");
        // SAFETY: the layout has a non-zero size.
        let ptr = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(ptr).unwrap_or_else(|| handle_alloc_error(layout));
        Self { ptr, len, layout }
    }
}

impl Deref for AlignedBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: `ptr` points to `len` initialised bytes owned by `self`.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl DerefMut for AlignedBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `deref`, and `&mut self` guarantees exclusive access.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: allocated in `zeroed` with this layout.
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_is_aligned_and_zeroed() {
        let mut buf = AlignedBuf::zeroed(10_000);
        assert_eq!(buf.as_ptr() as usize % BUFFER_ALIGNMENT, 0);
        assert!(buf.iter().all(|&b| b == 0));
        buf[9_999] = 7;
        assert_eq!(buf.len(), 10_000);
        assert_eq!(AlignedBuf::zeroed(0).len(), 0);
    }
}
