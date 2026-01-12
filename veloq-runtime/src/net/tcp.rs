use crate::io::RawHandle;
use crate::io::buffer::FixedBuf;
use crate::io::driver::{Driver, PlatformDriver};
use crate::io::op::{Accept, Connect, IoFd, Op, OpLifecycle, ReadFixed, WriteFixed};
use crate::io::socket::Socket;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;

// ============================================================================
// LocalTcpListener / LocalTcpStream (Thread-Bound, High Performance)
// ============================================================================

pub struct LocalTcpListener {
    fd: RawHandle,
}

pub struct LocalTcpStream {
    fd: RawHandle,
}

impl Drop for LocalTcpListener {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = unsafe { Socket::from_raw(*self.fd) };
        #[cfg(windows)]
        let _ = unsafe { Socket::from_raw(*self.fd) };
    }
}

impl Drop for LocalTcpStream {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = unsafe { Socket::from_raw(*self.fd) };
        #[cfg(windows)]
        let _ = unsafe { Socket::from_raw(*self.fd) };
    }
}

impl LocalTcpListener {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        // Resolve address (take first one)
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

        Ok(Self {
            fd: socket.into_raw().into(),
        })
    }

    pub async fn accept(&self) -> io::Result<(LocalTcpStream, SocketAddr)> {
        // Pre-allocate resources (platform specific)
        #[cfg(target_os = "windows")]
        let pre_alloc = Accept::pre_alloc(self.fd)?;

        // Create the Op
        #[cfg(target_os = "windows")]
        let op = Accept::into_op(self.fd, pre_alloc);

        #[cfg(target_os = "linux")]
        let op = Accept::into_op(self.fd, ());

        // Submit and Await
        let future = Op::new(op).submit_local().into_thread_bound();
        let (res, op_back): (io::Result<usize>, Accept) = future.await;

        // Post-process to get output
        let (fd, addr) = op_back.into_output(res)?;

        let stream = LocalTcpStream { fd };

        Ok((stream, addr))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        use std::mem::ManuallyDrop;

        #[cfg(unix)]
        let socket = unsafe { ManuallyDrop::new(Socket::from_raw(*self.fd)) };
        #[cfg(windows)]
        let socket = unsafe { ManuallyDrop::new(Socket::from_raw(*self.fd)) };
        socket.local_addr()
    }
}

impl LocalTcpStream {
    pub async fn connect(addr: SocketAddr) -> io::Result<Self> {
        let socket = if addr.is_ipv4() {
            Socket::new_tcp_v4()?
        } else {
            Socket::new_tcp_v6()?
        };
        let fd = socket.into_raw().into();

        let (raw_addr, raw_addr_len) = crate::io::socket::socket_addr_to_storage(addr);
        #[allow(clippy::unnecessary_cast)]
        let op = Connect {
            fd: IoFd::Raw(fd),
            addr: raw_addr,
            addr_len: raw_addr_len as u32,
        };

        let future = Op::new(op).submit_local().into_thread_bound();
        let (res, _op_back) = future.await;
        res?;

        Ok(Self { fd })
    }

    pub async fn recv(&self, buf: FixedBuf) -> (io::Result<usize>, FixedBuf) {
        let op = ReadFixed {
            fd: IoFd::Raw(self.fd),
            buf,
            offset: 0,
        };
        let future = Op::new(op).submit_local();
        let (res, op_back) = future.await;
        (res, op_back.buf)
    }

    pub async fn send(&self, buf: FixedBuf) -> (io::Result<usize>, FixedBuf) {
        let op = WriteFixed {
            fd: IoFd::Raw(self.fd),
            buf,
            offset: 0,
        };
        let future = Op::new(op).submit_local();
        let (res, op_back) = future.await;
        (res, op_back.buf)
    }
}

impl crate::io::AsyncBufRead for LocalTcpStream {
    fn read(
        &self,
        buf: FixedBuf,
    ) -> impl std::future::Future<Output = (io::Result<usize>, FixedBuf)> {
        self.recv(buf)
    }
}

impl crate::io::AsyncBufWrite for LocalTcpStream {
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
// TcpListener / TcpStream (Send + Sync, Smart Routing)
// ============================================================================

pub struct TcpListener {
    fd: RawHandle,
    owner_id: usize,
    injector: Arc<<PlatformDriver as Driver>::RemoteInjector>,
}

pub struct TcpStream {
    fd: RawHandle,
    owner_id: usize,
    injector: Arc<<PlatformDriver as Driver>::RemoteInjector>,
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = unsafe { Socket::from_raw(*self.fd) };
        #[cfg(windows)]
        let _ = unsafe { Socket::from_raw(*self.fd) };
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = unsafe { Socket::from_raw(*self.fd) };
        #[cfg(windows)]
        let _ = unsafe { Socket::from_raw(*self.fd) };
    }
}

impl TcpListener {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
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
        socket.listen(1024)?;

        let ctx = crate::runtime::context::current();
        let driver_weak = ctx.driver();
        let driver_rc = driver_weak
            .upgrade()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Runtime driver dropped"))?;
        let injector = driver_rc.borrow().injector();

        Ok(Self {
            fd: socket.into_raw().into(),
            owner_id: ctx.handle.id,
            injector,
        })
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        #[cfg(target_os = "windows")]
        let pre_alloc = Accept::pre_alloc(self.fd)?;

        #[cfg(target_os = "windows")]
        let op = Accept::into_op(self.fd, pre_alloc);
        #[cfg(target_os = "linux")]
        let op = Accept::into_op(self.fd, ());

        let is_local = {
            let ctx = crate::runtime::context::current();
            ctx.handle.id == self.owner_id
        };

        let (res, op_back) = if is_local {
            // Local Submission
            Op::new(op).submit_local().into_thread_bound().await
        } else {
            // Remote Submission
            Op::new(op)
                .submit_remote::<PlatformDriver>(&self.injector)
                .await
        };

        let (fd, addr) = op_back.into_output(res)?;

        // Inherit ownership from listener
        let stream = TcpStream {
            fd,
            owner_id: self.owner_id,
            injector: self.injector.clone(),
        };

        Ok((stream, addr))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        use std::mem::ManuallyDrop;

        #[cfg(unix)]
        let socket = unsafe { ManuallyDrop::new(Socket::from_raw(*self.fd)) };
        #[cfg(windows)]
        let socket = unsafe { ManuallyDrop::new(Socket::from_raw(*self.fd)) };
        socket.local_addr()
    }
}

impl TcpStream {
    pub async fn connect(addr: SocketAddr) -> io::Result<Self> {
        let socket = if addr.is_ipv4() {
            Socket::new_tcp_v4()?
        } else {
            Socket::new_tcp_v6()?
        };
        let fd = socket.into_raw().into();

        let (raw_addr, raw_addr_len) = crate::io::socket::socket_addr_to_storage(addr);
        #[allow(clippy::unnecessary_cast)]
        let op = Connect {
            fd: IoFd::Raw(fd),
            addr: raw_addr,
            addr_len: raw_addr_len as u32,
        };

        // Connect is always submitted to LOCAL driver initially (registering it)
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
            fd,
            owner_id: ctx.handle.id,
            injector,
        })
    }

    pub async fn recv(&self, buf: FixedBuf) -> (io::Result<usize>, FixedBuf) {
        let op = ReadFixed {
            fd: IoFd::Raw(self.fd),
            buf,
            offset: 0,
        };

        let is_local = {
            let ctx = crate::runtime::context::current();
            ctx.handle.id == self.owner_id
        };

        let (res, op_back) = if is_local {
            Op::new(op).submit_local().into_thread_bound().await
        } else {
            Op::new(op)
                .submit_remote::<PlatformDriver>(&self.injector)
                .await
        };

        (res, op_back.buf)
    }

    pub async fn send(&self, buf: FixedBuf) -> (io::Result<usize>, FixedBuf) {
        let op = WriteFixed {
            fd: IoFd::Raw(self.fd),
            buf,
            offset: 0,
        };

        let is_local = {
            let ctx = crate::runtime::context::current();
            ctx.handle.id == self.owner_id
        };

        let (res, op_back) = if is_local {
            Op::new(op).submit_local().into_thread_bound().await
        } else {
            Op::new(op)
                .submit_remote::<PlatformDriver>(&self.injector)
                .await
        };

        (res, op_back.buf)
    }
}

impl crate::io::AsyncBufRead for TcpStream {
    fn read(
        &self,
        buf: FixedBuf,
    ) -> impl std::future::Future<Output = (io::Result<usize>, FixedBuf)> {
        self.recv(buf)
    }
}

impl crate::io::AsyncBufWrite for TcpStream {
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
