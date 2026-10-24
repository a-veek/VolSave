use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;

use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, HANDLE};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_NO_BUFFERING, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::core::GUID;

/// NUL-terminated UTF-16 for Win32 calls.
pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// UTF-16 up to the first NUL.
pub(crate) fn from_wide(text: &[u16]) -> String {
    let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..len])
}

/// UTF-16 multi-string (`a\0b\0\0`) as a list.
pub(crate) fn from_multi_wide(text: &[u16]) -> Vec<String> {
    text.split(|&c| c == 0)
        .take_while(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// Converts a Windows API error into `io::Error`, keeping Win32 error codes.
pub(crate) fn io_error(error: windows::core::Error) -> io::Error {
    let code = error.code().0 as u32;
    if code & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xFFFF) as i32)
    } else {
        io::Error::other(error)
    }
}

pub(crate) fn handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// Metadata queries only; works without administrator rights.
    Query,
    Read,
    ReadWrite,
}

/// Opens a device, volume or file with full sharing.
pub(crate) fn open(path: &str, access: Access, unbuffered: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    match access {
        Access::Query => options.access_mode(0),
        Access::Read => options.read(true),
        Access::ReadWrite => options.read(true).write(true),
    };
    let mut flags = FILE_FLAG_BACKUP_SEMANTICS.0;
    if unbuffered {
        flags |= FILE_FLAG_NO_BUFFERING.0;
    }
    options
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags(flags)
        .open(path)
}

/// Runs an IOCTL with a fixed output buffer. Returns the bytes written and
/// whether the driver reported more data (`ERROR_MORE_DATA`).
pub(crate) fn ioctl_into(
    file: &File,
    code: u32,
    input: &[u8],
    output: &mut [u8],
) -> io::Result<(usize, bool)> {
    let mut returned = 0u32;
    let input_ptr = (!input.is_empty()).then_some(input.as_ptr() as *const c_void);
    // SAFETY: both buffers outlive the synchronous call and their lengths are passed along.
    let result = unsafe {
        DeviceIoControl(
            handle(file),
            code,
            input_ptr,
            input.len() as u32,
            Some(output.as_mut_ptr() as *mut c_void),
            output.len() as u32,
            Some(&mut returned),
            None,
        )
    };
    match result {
        Ok(()) => Ok((returned as usize, false)),
        Err(error) if error.code() == ERROR_MORE_DATA.to_hresult() => Ok((returned as usize, true)),
        Err(error) => Err(io_error(error)),
    }
}

/// Runs an IOCTL whose output size is unknown, growing the buffer as needed.
pub(crate) fn ioctl(file: &File, code: u32, input: &[u8], initial: usize) -> io::Result<Vec<u8>> {
    let mut output = vec![0u8; initial.max(64)];
    loop {
        let mut returned = 0u32;
        let input_ptr = (!input.is_empty()).then_some(input.as_ptr() as *const c_void);
        // SAFETY: as in `ioctl_into`.
        let result = unsafe {
            DeviceIoControl(
                handle(file),
                code,
                input_ptr,
                input.len() as u32,
                Some(output.as_mut_ptr() as *mut c_void),
                output.len() as u32,
                Some(&mut returned),
                None,
            )
        };
        match result {
            Ok(()) => {
                output.truncate(returned as usize);
                return Ok(output);
            }
            Err(error)
                if (error.code() == ERROR_MORE_DATA.to_hresult()
                    || error.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult())
                    && output.len() < 64 << 20 =>
            {
                output.resize(output.len() * 4, 0);
            }
            Err(error) => return Err(io_error(error)),
        }
    }
}

/// Reads a plain-old-data struct from a byte buffer.
///
/// # Safety
/// `T` must be valid for any bit pattern (a C struct of integers and arrays).
pub(crate) unsafe fn read_struct<T: Copy>(buf: &[u8], at: usize) -> io::Result<T> {
    if buf.len() < at + size_of::<T>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IOCTL result too short",
        ));
    }
    // SAFETY: bounds checked above; the caller guarantees `T` accepts any bytes.
    Ok(unsafe { std::ptr::read_unaligned(buf.as_ptr().add(at) as *const T) })
}

pub(crate) fn i64_at(buf: &[u8], at: usize) -> io::Result<i64> {
    let bytes = buf
        .get(at..at + 8)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "IOCTL result too short"))?;
    Ok(i64::from_le_bytes(
        bytes.try_into().expect("slice has 8 bytes"),
    ))
}

pub(crate) fn u32_at(buf: &[u8], at: usize) -> io::Result<u32> {
    let bytes = buf
        .get(at..at + 4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "IOCTL result too short"))?;
    Ok(u32::from_le_bytes(
        bytes.try_into().expect("slice has 4 bytes"),
    ))
}

/// Canonical `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` form.
pub fn format_guid(guid: &GUID) -> String {
    let d4 = guid.data4;
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        guid.data1, guid.data2, guid.data3, d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]
    )
}

static DEBUG_HOOK: std::sync::OnceLock<fn(&str)> = std::sync::OnceLock::new();

/// Routes internal details (e.g. every VSS call) to the caller's log.
pub fn set_debug_hook(hook: fn(&str)) {
    let _ = DEBUG_HOOK.set(hook);
}

/// Passes a detail to the hook; without one it is printed if `VOLSAVE_DEBUG` is set.
pub(crate) fn debug(message: &str) {
    match DEBUG_HOOK.get() {
        Some(hook) => hook(message),
        None if std::env::var_os("VOLSAVE_DEBUG").is_some() => eprintln!("[debug] {message}"),
        None => {}
    }
}
