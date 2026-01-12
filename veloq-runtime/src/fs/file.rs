use super::open_options::OpenOptions; // Use generic OpenOptions
use crate::io::RawHandle;
use crate::io::buffer::FixedBuf;
use crate::io::op::{
    Fallocate, Fsync, IoFd, LocalSubmitter, Op, OpSubmitter, ReadFixed, SharedSubmitter,
    SyncFileRange, WriteFixed,
};

use std::cell::RefCell;
use std::future::{Future, IntoFuture};
use std::io;
use std::path::Path;
use std::pin::Pin;

#[cfg(not(unix))]
macro_rules! ignore {
    ($($x:expr),* $(,)?) => {
        $(
            let _ = $x;
        )*
    };
}

// ============================================================================
// Internal Helper: InnerFile (RAII Wrapper)
// ============================================================================

pub(crate) struct InnerFile(pub(crate) RawHandle);

impl Drop for InnerFile {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::close(self.0.fd);
        }
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0.handle);
        }
    }
}

// ============================================================================
// Generic File Components
// ============================================================================

pub struct GenericFile<S: OpSubmitter> {
    pub(crate) inner: InnerFile,
    pub(crate) submitter: S,
    pub(crate) pos: RefCell<u64>,
}

pub type LocalFile = GenericFile<LocalSubmitter>;
pub type File = GenericFile<SharedSubmitter>;

pub struct SyncRangeBuilder<'a, S: OpSubmitter> {
    file: &'a GenericFile<S>,
    offset: u64,
    nbytes: u64,
    flags: u32,
}

impl<'a, S: OpSubmitter> SyncRangeBuilder<'a, S> {
    fn new(file: &'a GenericFile<S>, offset: u64, nbytes: u64) -> Self {
        #[cfg(unix)]
        let flags = libc::SYNC_FILE_RANGE_WAIT_BEFORE
            | libc::SYNC_FILE_RANGE_WRITE
            | libc::SYNC_FILE_RANGE_WAIT_AFTER;
        #[cfg(not(unix))]
        let flags = 0;

        Self {
            file,
            offset,
            nbytes,
            flags,
        }
    }

    pub fn wait_before(mut self, wait: bool) -> Self {
        #[cfg(unix)]
        if wait {
            self.flags |= libc::SYNC_FILE_RANGE_WAIT_BEFORE;
        } else {
            self.flags &= !libc::SYNC_FILE_RANGE_WAIT_BEFORE;
        }
        #[cfg(not(unix))]
        ignore!(wait, &mut self);
        self
    }

    pub fn write(mut self, write: bool) -> Self {
        #[cfg(unix)]
        if write {
            self.flags |= libc::SYNC_FILE_RANGE_WRITE;
        } else {
            self.flags &= !libc::SYNC_FILE_RANGE_WRITE;
        }
        #[cfg(not(unix))]
        ignore!(write, &mut self);
        self
    }

    pub fn wait_after(mut self, wait: bool) -> Self {
        #[cfg(unix)]
        if wait {
            self.flags |= libc::SYNC_FILE_RANGE_WAIT_AFTER;
        } else {
            self.flags &= !libc::SYNC_FILE_RANGE_WAIT_AFTER;
        }
        #[cfg(not(unix))]
        ignore!(wait, &mut self);
        self
    }
}

impl<'a, S: OpSubmitter> IntoFuture for SyncRangeBuilder<'a, S> {
    type Output = io::Result<()>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        let op = SyncFileRange {
            fd: IoFd::Raw(self.file.inner.0),
            offset: self.offset,
            nbytes: self.nbytes,
            flags: self.flags,
        };

        let submitter = self.file.submitter.clone();
        Box::pin(async move {
            let (res, _) = submitter.submit(Op::new(op)).await;
            res.map(|_| ())
        })
    }
}

impl<S: OpSubmitter> GenericFile<S> {
    pub fn options() -> OpenOptions {
        OpenOptions::new()
    }

    pub fn seek(&self, pos: u64) {
        *self.pos.borrow_mut() = pos;
    }

    pub fn stream_position(&self) -> u64 {
        *self.pos.borrow()
    }

    pub async fn read_at(&self, buf: FixedBuf, offset: u64) -> (io::Result<usize>, FixedBuf) {
        let op = ReadFixed {
            fd: IoFd::Raw(self.inner.0),
            buf,
            offset,
        };

        let (res, op) = self.submitter.submit(Op::new(op)).await;
        (res, op.buf)
    }

    pub async fn write_at(&self, buf: FixedBuf, offset: u64) -> (io::Result<usize>, FixedBuf) {
        let op = WriteFixed {
            fd: IoFd::Raw(self.inner.0),
            buf,
            offset,
        };

        let (res, op) = self.submitter.submit(Op::new(op)).await;
        (res, op.buf)
    }

    pub async fn sync_all(&self) -> io::Result<()> {
        let op = Fsync {
            fd: IoFd::Raw(self.inner.0),
            datasync: false,
        };

        let (res, _) = self.submitter.submit(Op::new(op)).await;
        res.map(|_| ())
    }

    pub async fn sync_data(&self) -> io::Result<()> {
        let op = Fsync {
            fd: IoFd::Raw(self.inner.0),
            datasync: true,
        };

        let (res, _) = self.submitter.submit(Op::new(op)).await;
        res.map(|_| ())
    }

    /// Sync a file range.
    ///
    /// On Windows, this falls back to `FlushFileBuffers` which syncs the entire file, ignoring the range.
    ///
    /// Returns a specific Future (Builder) that allows configuring flags.
    /// Usage: `file.sync_range(0, 100).wait_before(false).write(true).await`
    pub fn sync_range(&self, offset: u64, nbytes: u64) -> SyncRangeBuilder<'_, S> {
        SyncRangeBuilder::new(self, offset, nbytes)
    }

    pub async fn fallocate(&self, offset: u64, len: u64) -> io::Result<()> {
        let op = Fallocate {
            fd: IoFd::Raw(self.inner.0),
            mode: 0, // Default mode
            offset,
            len,
        };

        let (res, _) = self.submitter.submit(Op::new(op)).await;
        res.map(|_| ())
    }
}

impl<S: OpSubmitter> crate::io::AsyncBufRead for GenericFile<S> {
    async fn read(&self, buf: FixedBuf) -> (io::Result<usize>, FixedBuf) {
        let offset = *self.pos.borrow();
        let (res, buf) = self.read_at(buf, offset).await;
        if let Ok(n) = res {
            *self.pos.borrow_mut() += n as u64;
        }
        (res, buf)
    }
}

impl<S: OpSubmitter> crate::io::AsyncBufWrite for GenericFile<S> {
    async fn write(&self, buf: FixedBuf) -> (io::Result<usize>, FixedBuf) {
        let offset = *self.pos.borrow();
        let (res, buf) = self.write_at(buf, offset).await;
        if let Ok(n) = res {
            *self.pos.borrow_mut() += n as u64;
        }
        (res, buf)
    }

    fn flush(&self) -> impl std::future::Future<Output = io::Result<()>> {
        self.sync_data()
    }

    fn shutdown(&self) -> impl std::future::Future<Output = io::Result<()>> {
        self.sync_all()
    }
}

// --- Specific Implementations ---

impl LocalFile {
    pub async fn open(path: impl AsRef<Path>) -> io::Result<LocalFile> {
        OpenOptions::new().read(true).open_local(path).await
    }

    pub async fn create(path: impl AsRef<Path>) -> io::Result<LocalFile> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open_local(path)
            .await
    }
}

impl File {
    pub async fn open(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new().read(true).open(path).await
    }

    pub async fn create(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await
    }
}
