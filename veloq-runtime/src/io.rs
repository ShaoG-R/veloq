pub mod buffer;
pub mod driver;
pub mod op;
pub(crate) mod socket;

use crate::io::buffer::FixedBuf;
use std::future::Future;
use std::io;
#[cfg(windows)]
use std::os::raw::c_void;

/// Async buffered reading trait.
/// Suitable for underlying asynchronous read operations that require passing FixedBuf ownership.
pub trait AsyncBufRead {
    /// Read data into the buffer.
    /// Returns the number of bytes read and the original buffer.
    fn read(&self, buf: FixedBuf) -> impl Future<Output = (io::Result<usize>, FixedBuf)>;
}

/// Async buffered writing trait.
/// Suitable for underlying asynchronous write operations that require passing FixedBuf ownership.
pub trait AsyncBufWrite {
    /// Write data from the buffer.
    /// Returns the number of bytes written and the original buffer.
    fn write(&self, buf: FixedBuf) -> impl Future<Output = (io::Result<usize>, FixedBuf)>;

    /// Flush the buffer (e.g., sync file to disk).
    fn flush(&self) -> impl Future<Output = io::Result<()>>;

    /// Close the writing end (e.g., TCP shutdown).
    fn shutdown(&self) -> impl Future<Output = io::Result<()>>;
}

// ============================================================================
// Core Types (Platform-Agnostic)
// ============================================================================

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawHandle {
    // i32 (4 bytes)
    pub fd: std::os::fd::RawFd,
}

#[cfg(unix)]
impl std::ops::Deref for RawHandle {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.fd
    }
}

#[cfg(unix)]
impl From<RawHandle> for std::os::fd::RawFd {
    fn from(handle: RawHandle) -> Self {
        handle.fd
    }
}

#[cfg(unix)]
impl From<RawHandle> for i32 {
    fn from(handle: RawHandle) -> Self {
        handle.fd as i32
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawHandle {
    // usually isize/ptr (8 bytes)
    pub handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl From<*mut c_void> for RawHandle {
    fn from(handle: *mut c_void) -> Self {
        RawHandle {
            handle: handle as _,
        }
    }
}

#[cfg(windows)]
impl From<usize> for RawHandle {
    fn from(handle: usize) -> Self {
        RawHandle {
            handle: handle as _,
        }
    }
}

#[cfg(windows)]
#[cfg(target_pointer_width = "64")]
impl From<u64> for RawHandle {
    fn from(handle: u64) -> Self {
        RawHandle {
            handle: handle as _,
        }
    }
}

#[cfg(windows)]
#[cfg(target_pointer_width = "32")]
impl From<u32> for RawHandle {
    fn from(handle: u32) -> Self {
        RawHandle {
            handle: handle as _,
        }
    }
}

#[cfg(windows)]
impl std::ops::Deref for RawHandle {
    type Target = windows_sys::Win32::Foundation::HANDLE;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

#[cfg(windows)]
impl From<RawHandle> for windows_sys::Win32::Foundation::HANDLE {
    fn from(handle: RawHandle) -> Self {
        handle.handle
    }
}

#[cfg(windows)]
impl From<RawHandle> for usize {
    fn from(handle: RawHandle) -> Self {
        handle.handle as usize
    }
}

unsafe impl std::marker::Send for RawHandle {}
unsafe impl Sync for RawHandle {}
