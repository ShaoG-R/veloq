use crate::io::RawHandle;
use crate::io::buffer::FixedBuf;
use crate::io::driver::Driver;
use crate::io::op::{
    Connect, IoFd, LocalSubmitter, Op, OpSubmitter, ReadFixed, RecvFrom, SendTo, SharedSubmitter,
    WriteFixed,
};
use crate::io::socket::Socket;
use std::io;
use std::mem::ManuallyDrop;
use std::net::{SocketAddr, ToSocketAddrs};

// ============================================================================
// Internal Helper: InnerSocket (RAII Wrapper)
// ============================================================================

struct InnerSocket(RawHandle);

impl Drop for InnerSocket {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = unsafe { Socket::from_raw(*self.0) };
        #[cfg(windows)]
        let _ = unsafe { Socket::from_raw(*self.0) };
    }
}

impl InnerSocket {
    fn raw(&self) -> RawHandle {
        self.0
    }

    fn from_bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "No address provided"))?;

        let socket = if addr.is_ipv4() {
            Socket::new_udp_v4()?
        } else {
            Socket::new_udp_v6()?
        };

        socket.bind(addr)?;

        Ok(Self(socket.into_raw().into()))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        #[cfg(unix)]
        let socket = unsafe { ManuallyDrop::new(Socket::from_raw(*self.0)) };
        #[cfg(windows)]
        let socket = unsafe { ManuallyDrop::new(Socket::from_raw(*self.0)) };
        socket.local_addr()
    }
}

// ============================================================================
// Generic UDP Components
// ============================================================================

pub struct GenericUdpSocket<S: OpSubmitter> {
    inner: InnerSocket,
    submitter: S,
}

// Type Aliases
pub type LocalUdpSocket = GenericUdpSocket<LocalSubmitter>;
pub type UdpSocket = GenericUdpSocket<SharedSubmitter>;

// --- Common Implementation ---

impl<S: OpSubmitter> GenericUdpSocket<S> {
    pub async fn send_to(
        &self,
        buf: FixedBuf,
        target: SocketAddr,
    ) -> (io::Result<usize>, FixedBuf) {
        let op = SendTo {
            fd: IoFd::Raw(self.inner.raw()),
            buf,
            addr: target,
        };
        let (res, op_back) = self.submitter.submit(Op::new(op)).await;
        (res, op_back.buf)
    }

    pub async fn recv_from(&self, buf: FixedBuf) -> (io::Result<(usize, SocketAddr)>, FixedBuf) {
        let op = RecvFrom {
            fd: IoFd::Raw(self.inner.raw()),
            buf,
            addr: None,
        };
        let (res, op_back) = self.submitter.submit(Op::new(op)).await;

        match res {
            Ok(n) => {
                let addr = op_back.addr.unwrap_or_else(|| "0.0.0.0:0".parse().unwrap());
                (Ok((n, addr)), op_back.buf)
            }
            Err(e) => (Err(e), op_back.buf),
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    pub async fn connect(&self, addr: SocketAddr) -> io::Result<()> {
        let (raw_addr, raw_addr_len) = crate::io::socket::socket_addr_to_storage(addr);
        #[allow(clippy::unnecessary_cast)]
        let op = Connect {
            fd: IoFd::Raw(self.inner.raw()),
            addr: raw_addr,
            addr_len: raw_addr_len as u32,
        };

        let (res, _) = self.submitter.submit(Op::new(op)).await;
        res.map(|_| ())
    }

    pub async fn send(&self, buf: FixedBuf) -> (io::Result<usize>, FixedBuf) {
        let op = WriteFixed {
            fd: IoFd::Raw(self.inner.raw()),
            buf,
            offset: 0,
        };
        let (res, op_back) = self.submitter.submit(Op::new(op)).await;
        (res, op_back.buf)
    }

    pub async fn recv(&self, buf: FixedBuf) -> (io::Result<usize>, FixedBuf) {
        let op = ReadFixed {
            fd: IoFd::Raw(self.inner.raw()),
            buf,
            offset: 0,
        };
        let (res, op_back) = self.submitter.submit(Op::new(op)).await;
        (res, op_back.buf)
    }
}

impl<S: OpSubmitter> crate::io::AsyncBufRead for GenericUdpSocket<S> {
    fn read(
        &self,
        buf: FixedBuf,
    ) -> impl std::future::Future<Output = (io::Result<usize>, FixedBuf)> {
        self.recv(buf)
    }
}

impl<S: OpSubmitter> crate::io::AsyncBufWrite for GenericUdpSocket<S> {
    fn write(
        &self,
        buf: FixedBuf,
    ) -> impl std::future::Future<Output = (io::Result<usize>, FixedBuf)> {
        self.send(buf)
    }

    fn flush(&self) -> impl std::future::Future<Output = io::Result<()>> {
        std::future::ready(Ok(()))
    }

    fn shutdown(&self) -> impl std::future::Future<Output = io::Result<()>> {
        std::future::ready(Ok(()))
    }
}

// ============================================================================
// Specific Construction Logic
// ============================================================================

// --- Local Implementations ---
impl LocalUdpSocket {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        Ok(Self {
            inner: InnerSocket::from_bind(addr)?,
            submitter: LocalSubmitter,
        })
    }
}

// --- Shared (Remote-Capable) Implementations ---
impl UdpSocket {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let inner = InnerSocket::from_bind(addr)?;

        let ctx = crate::runtime::context::current();
        let driver_weak = ctx.driver();
        let driver_rc = driver_weak
            .upgrade()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Runtime driver dropped"))?;
        let injector = driver_rc.borrow().injector();

        Ok(Self {
            inner,
            submitter: SharedSubmitter {
                owner_id: ctx.handle.id,
                injector,
            },
        })
    }
}
