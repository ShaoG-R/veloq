use crate::io::RawHandle;
use crate::io::socket::Socket;
use std::io;
use std::mem::ManuallyDrop;
use std::net::SocketAddr;

// ============================================================================
// InnerSocket (RAII Wrapper)
// ============================================================================

pub struct InnerSocket(pub(crate) RawHandle);

impl Drop for InnerSocket {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = unsafe { Socket::from_raw(*self.0) };
        #[cfg(windows)]
        let _ = unsafe { Socket::from_raw(*self.0) };
    }
}

impl InnerSocket {
    pub fn new(handle: RawHandle) -> Self {
        Self(handle)
    }

    pub fn raw(&self) -> RawHandle {
        self.0
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        #[cfg(unix)]
        let socket = unsafe { ManuallyDrop::new(Socket::from_raw(*self.0)) };
        #[cfg(windows)]
        let socket = unsafe { ManuallyDrop::new(Socket::from_raw(*self.0)) };
        socket.local_addr()
    }
}
