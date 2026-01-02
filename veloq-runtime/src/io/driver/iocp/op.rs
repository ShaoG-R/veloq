//! IOCP Platform-Specific Operation Definitions
//!
//! This module defines:
//! - `IocpOp` enum: The platform-specific operation enum for Windows IOCP
//! - Platform-specific operation state structures with Windows-specific fields
//! - `IntoPlatformOp` implementations for converting generic ops to IocpOp

use crate::io::buffer::FixedBuf;
use crate::io::driver::PlatformOp;
use crate::io::driver::iocp::IocpDriver;
use crate::io::op::{
    Accept, Close, Connect, Fsync, IntoPlatformOp, IoFd, Open, RawHandle, ReadFixed, Recv,
    RecvFrom, Send as OpSend, SendTo, Timeout, Wakeup, WriteFixed,
};
use std::io;
use std::net::SocketAddr;
use windows_sys::Win32::Networking::WinSock::WSABUF;

// ============================================================================
// Platform-Specific Operation State Structures
// ============================================================================

/// IOCP specific state for Accept operation.
/// Stores the pre-created accept socket required by AcceptEx.
pub struct IocpAccept {
    pub fd: IoFd,
    pub addr: Box<[u8]>,
    pub addr_len: Box<u32>,
    /// Pre-created socket for the accepted connection (Windows-specific).
    pub accept_socket: RawHandle,
    pub remote_addr: Option<SocketAddr>,
}

impl From<Accept> for IocpAccept {
    fn from(accept: Accept) -> Self {
        // accept_socket is now properly passed from Accept::pre_alloc
        Self {
            fd: accept.fd,
            addr: accept.addr,
            addr_len: accept.addr_len,
            accept_socket: accept.accept_socket,
            remote_addr: accept.remote_addr,
        }
    }
}

impl From<IocpAccept> for Accept {
    fn from(iocp_accept: IocpAccept) -> Self {
        Self {
            fd: iocp_accept.fd,
            addr: iocp_accept.addr,
            addr_len: iocp_accept.addr_len,
            remote_addr: iocp_accept.remote_addr,
            accept_socket: iocp_accept.accept_socket,
        }
    }
}

/// IOCP specific state for SendTo operation.
/// Includes Windows-specific WSABUF structure.
pub struct IocpSendTo {
    pub fd: IoFd,
    pub buf: FixedBuf,
    pub addr: Box<[u8]>,
    pub addr_len: u32,
    pub wsabuf: Box<WSABUF>,
}

impl IocpSendTo {
    pub fn new(fd: RawHandle, buf: FixedBuf, target: SocketAddr) -> Self {
        let (raw_addr, raw_addr_len) = crate::io::socket::socket_addr_trans(target);
        let addr = raw_addr.into_boxed_slice();
        let addr_len = raw_addr_len as u32;

        let wsabuf = Box::new(WSABUF {
            len: buf.len() as u32,
            buf: buf.as_slice().as_ptr() as *mut u8,
        });

        Self {
            fd: IoFd::Raw(fd),
            buf,
            addr,
            addr_len,
            wsabuf,
        }
    }
}

impl From<SendTo> for IocpSendTo {
    fn from(send_to: SendTo) -> Self {
        let wsabuf = Box::new(WSABUF {
            len: send_to.buf.len() as u32,
            buf: send_to.buf.as_slice().as_ptr() as *mut u8,
        });

        Self {
            fd: send_to.fd,
            buf: send_to.buf,
            addr: send_to.addr,
            addr_len: send_to.addr_len,
            wsabuf,
        }
    }
}

impl From<IocpSendTo> for SendTo {
    fn from(iocp_send_to: IocpSendTo) -> Self {
        Self {
            fd: iocp_send_to.fd,
            buf: iocp_send_to.buf,
            addr: iocp_send_to.addr,
            addr_len: iocp_send_to.addr_len,
        }
    }
}

/// IOCP specific state for RecvFrom operation.
/// Includes Windows-specific WSABUF structure and flags.
pub struct IocpRecvFrom {
    pub fd: IoFd,
    pub buf: FixedBuf,
    pub addr: Box<[u8]>,
    pub addr_len: Box<i32>,
    pub flags: Box<u32>,
    pub wsabuf: Box<WSABUF>,
}

impl IocpRecvFrom {
    pub fn new(fd: RawHandle, mut buf: FixedBuf) -> Self {
        let addr_buf_size = 128usize;
        let addr = vec![0u8; addr_buf_size].into_boxed_slice();

        let addr_len = Box::new(addr_buf_size as i32);
        let wsabuf = Box::new(WSABUF {
            len: buf.capacity() as u32,
            buf: buf.as_mut_ptr(),
        });
        let flags = Box::new(0u32);

        Self {
            fd: IoFd::Raw(fd),
            buf,
            addr,
            addr_len,
            flags,
            wsabuf,
        }
    }

    pub fn get_addr_len(&self) -> usize {
        *self.addr_len as usize
    }
}

impl From<RecvFrom> for IocpRecvFrom {
    fn from(recv_from: RecvFrom) -> Self {
        let mut buf = recv_from.buf;
        let wsabuf = Box::new(WSABUF {
            len: buf.capacity() as u32,
            buf: buf.as_mut_ptr(),
        });

        Self {
            fd: recv_from.fd,
            buf,
            addr: recv_from.addr,
            addr_len: Box::new(*recv_from.addr_len as i32),
            flags: Box::new(0u32),
            wsabuf,
        }
    }
}

impl From<IocpRecvFrom> for RecvFrom {
    fn from(iocp_recv_from: IocpRecvFrom) -> Self {
        Self {
            fd: iocp_recv_from.fd,
            buf: iocp_recv_from.buf,
            addr: iocp_recv_from.addr,
            addr_len: Box::new(*iocp_recv_from.addr_len as u32),
        }
    }
}

/// IOCP specific state for Timeout operation.
pub struct IocpTimeout {
    pub duration: std::time::Duration,
}

impl From<Timeout> for IocpTimeout {
    fn from(timeout: Timeout) -> Self {
        Self {
            duration: timeout.duration,
        }
    }
}

impl From<IocpTimeout> for Timeout {
    fn from(iocp_timeout: IocpTimeout) -> Self {
        Self {
            duration: iocp_timeout.duration,
        }
    }
}

/// IOCP specific state for Wakeup operation.
/// Uses PostQueuedCompletionStatus, no additional fields needed.
pub struct IocpWakeup;

impl From<Wakeup> for IocpWakeup {
    fn from(_wakeup: Wakeup) -> Self {
        Self
    }
}

impl From<IocpWakeup> for Wakeup {
    fn from(_iocp_wakeup: IocpWakeup) -> Self {
        Self { fd: IoFd::Raw(0) }
    }
}

/// IOCP specific state for Open operation.
/// Uses UTF-16 encoding for Windows paths.
pub struct IocpOpen {
    pub path: Vec<u16>,
    pub flags: i32,
    pub mode: u32,
}

impl From<Open> for IocpOpen {
    fn from(open: Open) -> Self {
        // The path bytes are already packed UTF-16 (2 bytes per char, little-endian)
        // Convert back to Vec<u16>
        let path: Vec<u16> = open
            .path
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();

        Self {
            path,
            flags: open.flags,
            mode: open.mode,
        }
    }
}

impl From<IocpOpen> for Open {
    fn from(iocp_open: IocpOpen) -> Self {
        // Convert UTF-16 back to UTF-8
        let path_without_null: Vec<u16> =
            iocp_open.path.into_iter().take_while(|&c| c != 0).collect();
        let path = String::from_utf16_lossy(&path_without_null).into_bytes();

        Self {
            path,
            flags: iocp_open.flags,
            mode: iocp_open.mode,
        }
    }
}

// ============================================================================
// IocpOp Enum Definition
// ============================================================================

/// The IOCP platform-specific operation enum.
/// Each variant wraps either a cross-platform op struct or a platform-specific state.
pub enum IocpOp {
    ReadFixed(ReadFixed),
    WriteFixed(WriteFixed),
    Recv(Recv),
    Send(OpSend),
    Accept(IocpAccept),
    Connect(Connect),
    RecvFrom(IocpRecvFrom),
    SendTo(IocpSendTo),
    Open(IocpOpen),
    Close(Close),
    Fsync(Fsync),
    Wakeup(IocpWakeup),
    Timeout(IocpTimeout),
    /// Offload operation for synchronous tasks run in thread pool.
    Offload(Option<Box<dyn FnOnce() -> io::Result<usize> + Send>>),
}

// IocpOp needs to be Send so it can be passed to the Driver
unsafe impl Send for IocpOp {}

impl PlatformOp for IocpOp {}

// ============================================================================
// IntoPlatformOp Implementations
// ============================================================================

macro_rules! impl_into_iocp_op_direct {
    ($Type:ident) => {
        impl IntoPlatformOp<IocpDriver> for $Type {
            fn into_platform_op(self) -> IocpOp {
                IocpOp::$Type(self)
            }
            fn from_platform_op(op: IocpOp) -> Self {
                match op {
                    IocpOp::$Type(val) => val,
                    _ => panic!(concat!(
                        "Driver returned mismatched Op type: expected ",
                        stringify!($Type)
                    )),
                }
            }
        }
    };
}

macro_rules! impl_into_iocp_op_convert {
    ($GenericType:ident, $IocpType:ident, $Variant:ident) => {
        impl IntoPlatformOp<IocpDriver> for $GenericType {
            fn into_platform_op(self) -> IocpOp {
                IocpOp::$Variant(self.into())
            }
            fn from_platform_op(op: IocpOp) -> Self {
                match op {
                    IocpOp::$Variant(val) => val.into(),
                    _ => panic!(concat!(
                        "Driver returned mismatched Op type: expected ",
                        stringify!($Variant)
                    )),
                }
            }
        }
    };
}

// Direct mappings (no conversion needed)
impl_into_iocp_op_direct!(ReadFixed);
impl_into_iocp_op_direct!(WriteFixed);
impl_into_iocp_op_direct!(Recv);
impl_into_iocp_op_direct!(Connect);
impl_into_iocp_op_direct!(Close);
impl_into_iocp_op_direct!(Fsync);

// Conversions for platform-specific state
impl_into_iocp_op_convert!(Accept, IocpAccept, Accept);
impl_into_iocp_op_convert!(SendTo, IocpSendTo, SendTo);
impl_into_iocp_op_convert!(RecvFrom, IocpRecvFrom, RecvFrom);
impl_into_iocp_op_convert!(Open, IocpOpen, Open);
impl_into_iocp_op_convert!(Timeout, IocpTimeout, Timeout);
impl_into_iocp_op_convert!(Wakeup, IocpWakeup, Wakeup);

// Manual implementation for Send because of name conflict with OpSend
impl IntoPlatformOp<IocpDriver> for OpSend {
    fn into_platform_op(self) -> IocpOp {
        IocpOp::Send(self)
    }
    fn from_platform_op(op: IocpOp) -> Self {
        match op {
            IocpOp::Send(val) => val,
            _ => panic!("Driver returned mismatched Op type: expected Send"),
        }
    }
}

impl IocpOp {
    /// Get the file descriptor associated with this operation (if any).
    pub fn get_fd(&self) -> Option<IoFd> {
        match self {
            IocpOp::ReadFixed(op) => Some(op.fd),
            IocpOp::WriteFixed(op) => Some(op.fd),
            IocpOp::Recv(op) => Some(op.fd),
            IocpOp::Send(op) => Some(op.fd),
            IocpOp::Accept(op) => Some(op.fd),
            IocpOp::Connect(op) => Some(op.fd),
            IocpOp::RecvFrom(op) => Some(op.fd),
            IocpOp::SendTo(op) => Some(op.fd),
            IocpOp::Close(op) => Some(op.fd),
            IocpOp::Fsync(op) => Some(op.fd),
            IocpOp::Open(_) | IocpOp::Wakeup(_) | IocpOp::Timeout(_) | IocpOp::Offload(_) => None,
        }
    }
}
