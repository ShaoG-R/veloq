use std::ptr::NonNull;

pub mod buddy;
pub mod hybrid;

pub use buddy::BuddyPool;
pub use hybrid::HybridPool;

// Backward compatibility or default choice
pub type BufferPool = HybridPool;

pub const NO_REGISTRATION_INDEX: u16 = u16::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferSize {
    /// 4KB
    Size4K,
    /// 16KB
    Size16K,
    /// 64KB
    Size64K,
    /// Custom size
    Custom(usize),
}

impl BufferSize {
    pub fn size(&self) -> usize {
        match self {
            BufferSize::Size4K => 4096,
            BufferSize::Size16K => 16384,
            BufferSize::Size64K => 65536,
            BufferSize::Custom(size) => *size,
        }
    }
}

/// Trait for memory pool implementation allows custom memory management
pub trait BufPool: Clone + std::fmt::Debug + 'static {
    fn new() -> Self;
    /// Allocate memory of at least `size` bytes.
    /// Returns (ptr, capacity, global_index, context)
    ///
    /// - `global_index` is the index used for io_uring registration (if applicable)
    /// - `context` is an opaque value passed back to dealloc
    fn alloc_mem(&self, size: usize) -> Option<(NonNull<u8>, usize, u16, usize)>;

    /// Deallocate memory.
    unsafe fn dealloc_mem(&self, ptr: NonNull<u8>, cap: usize, context: usize);

    /// Get all buffers for io_uring registration.
    #[cfg(target_os = "linux")]
    fn get_registration_buffers(&self) -> Vec<libc::iovec>;
}

pub struct FixedBuf<P: BufPool> {
    pool: P,
    ptr: NonNull<u8>,
    len: usize,
    cap: usize,
    global_index: u16,
    context: usize,
}

// Safety: This buffer is generally not Send because it refers to thread-local pool logic
// but in Thread-per-Core it stays on thread.

impl<P: BufPool> FixedBuf<P> {
    pub fn new(pool: P, ptr: NonNull<u8>, cap: usize, global_index: u16, context: usize) -> Self {
        Self {
            pool,
            ptr,
            len: cap,
            cap,
            global_index,
            context,
        }
    }

    pub fn buf_index(&self) -> u16 {
        self.global_index
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    pub fn as_slice_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    /// Access the full capacity as a mutable slice for writing data before set_len is called.
    pub fn spare_capacity_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.cap) }
    }

    // Pointer to start of capacity
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn set_len(&mut self, len: usize) {
        assert!(len <= self.cap);
        self.len = len;
    }
}

impl<P: BufPool> Drop for FixedBuf<P> {
    fn drop(&mut self) {
        unsafe {
            self.pool.dealloc_mem(self.ptr, self.cap, self.context);
        }
    }
}
