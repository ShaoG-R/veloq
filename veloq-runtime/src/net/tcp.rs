use crate::io::RawHandle;
use crate::io::buffer::FixedBuf;
use crate::io::driver::Driver;
use crate::io::op::{
    Accept, Connect, IoFd, LocalSubmitter, Op, OpLifecycle, OpSubmitter, ReadFixed,
    SharedSubmitter, WriteFixed,
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
    fn new(handle: RawHandle) -> Self {
        Self(handle)
    }

    fn raw(&self) -> RawHandle {
        self.0
    }

    fn from_bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "No address provided"))?;

        let socket = if addr.is_ipv4() {
            Socket::new_tcp_v4()?
        } else {
            Socket::new_tcp_v6()?
        };

        socket.bind(addr)?;
        socket.listen(1024)?; // backlog

        Ok(Self(socket.into_raw().into()))
    }

    fn from_new(addr: &SocketAddr) -> io::Result<Self> {
        let socket = if addr.is_ipv4() {
            Socket::new_tcp_v4()?
        } else {
            Socket::new_tcp_v6()?
        };
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
// Generic TCP Components
// ============================================================================

pub struct GenericTcpListener<S: OpSubmitter> {
    inner: InnerSocket,
    submitter: S,
}

pub struct GenericTcpStream<S: OpSubmitter> {
    inner: InnerSocket,
    submitter: S,
}

// Type Aliases for user convenience
pub type LocalTcpListener = GenericTcpListener<LocalSubmitter>;
pub type LocalTcpStream = GenericTcpStream<LocalSubmitter>;

pub type TcpListener = GenericTcpListener<SharedSubmitter>;
pub type TcpStream = GenericTcpStream<SharedSubmitter>;

// --- Common Implementation ---

impl<S: OpSubmitter> GenericTcpListener<S> {
    pub async fn accept(&self) -> io::Result<(GenericTcpStream<S>, SocketAddr)> {
        let op = Accept::prepare_op(self.inner.raw())?;

        let (res, op_back) = self.submitter.submit(Op::new(op)).await;
        let (fd, addr) = op_back.into_output(res)?;

        let stream = GenericTcpStream {
            inner: InnerSocket::new(fd),
            submitter: self.submitter.clone(),
        };

        Ok((stream, addr))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}

impl<S: OpSubmitter> GenericTcpStream<S> {
    pub async fn recv(&self, buf: FixedBuf) -> (io::Result<usize>, FixedBuf) {
        let op = ReadFixed {
            fd: IoFd::Raw(self.inner.raw()),
            buf,
            offset: 0,
        };
        let (res, op_back) = self.submitter.submit(Op::new(op)).await;
        (res, op_back.buf)
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
}

impl<S: OpSubmitter> crate::io::AsyncBufRead for GenericTcpStream<S> {
    fn read(
        &self,
        buf: FixedBuf,
    ) -> impl std::future::Future<Output = (io::Result<usize>, FixedBuf)> {
        self.recv(buf)
    }
}

impl<S: OpSubmitter> crate::io::AsyncBufWrite for GenericTcpStream<S> {
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
impl LocalTcpListener {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        Ok(Self {
            inner: InnerSocket::from_bind(addr)?,
            submitter: LocalSubmitter,
        })
    }
}

impl LocalTcpStream {
    pub async fn connect(addr: SocketAddr) -> io::Result<Self> {
        let inner = InnerSocket::from_new(&addr)?;

        let (raw_addr, raw_addr_len) = crate::io::socket::socket_addr_to_storage(addr);
        #[allow(clippy::unnecessary_cast)]
        let op = Connect {
            fd: IoFd::Raw(inner.raw()),
            addr: raw_addr,
            addr_len: raw_addr_len as u32,
        };

        let future = Op::new(op).submit_local().into_thread_bound();
        let (res, _op_back) = future.await;
        res?;

        Ok(Self {
            inner,
            submitter: LocalSubmitter,
        })
    }
}

// --- Shared (Remote-Capable) Implementations ---
impl TcpListener {
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

impl TcpStream {
    pub async fn connect(addr: SocketAddr) -> io::Result<Self> {
        let inner = InnerSocket::from_new(&addr)?;

        let (raw_addr, raw_addr_len) = crate::io::socket::socket_addr_to_storage(addr);
        #[allow(clippy::unnecessary_cast)]
        let op = Connect {
            fd: IoFd::Raw(inner.raw()),
            addr: raw_addr,
            addr_len: raw_addr_len as u32,
        };

        // Note: Connect is *initially* local to register the socket,
        // but we need to capture the runtime context for the SharedSubmitter here.
        let ctx = crate::runtime::context::current();
        let driver_weak = ctx.driver();
        let driver_rc = driver_weak
            .upgrade()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Runtime driver dropped"))?;
        let injector = driver_rc.borrow().injector();

        let future = Op::new(op).submit_local().into_thread_bound();
        let (res, _op_back) = future.await;
        res?;

        Ok(Self {
            inner,
            submitter: SharedSubmitter {
                owner_id: ctx.handle.id,
                injector,
            },
        })
    }
}
