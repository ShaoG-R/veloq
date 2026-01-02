//! io_uring Platform-Specific Operation Definitions
//!
//! This module defines:
//! - `UringOp` enum: The platform-specific operation enum for io_uring
//! - Platform-specific operation state structures with Linux-specific fields
//! - `IntoPlatformOp` implementations for converting generic ops to UringOp

use crate::io::driver::PlatformOp;
use crate::io::driver::uring::UringDriver;
use crate::io::op::{
    Accept, Close, Connect, Fsync, IntoPlatformOp, Open, ReadFixed, Recv, RecvFrom,
    Send as OpSend, SendTo, Timeout, Wakeup, WriteFixed, IoFd, RawHandle,
};
use crate::io::buffer::FixedBuf;
use std::net::SocketAddr;

// ============================================================================
// Platform-Specific Operation State Structures
// ============================================================================

/// io_uring specific state for Accept operation.
/// Stores Linux-specific fields like libc sockaddr buffers.
pub struct UringAccept {
    pub fd: IoFd,
    pub addr: Box<[u8]>,
    pub addr_len: Box<u32>,
    pub remote_addr: Option<SocketAddr>,
}

impl From<Accept> for UringAccept {
    fn from(accept: Accept) -> Self {
        Self {
            fd: accept.fd,
            addr: accept.addr,
            addr_len: accept.addr_len,
            remote_addr: accept.remote_addr,
        }
    }
}

impl From<UringAccept> for Accept {
    fn from(uring_accept: UringAccept) -> Self {
        Self {
            fd: uring_accept.fd,
            addr: uring_accept.addr,
            addr_len: uring_accept.addr_len,
            remote_addr: uring_accept.remote_addr,
        }
    }
}

/// io_uring specific state for SendTo operation.
/// Includes Linux-specific msghdr and iovec structures.
pub struct UringSendTo {
    pub fd: IoFd,
    pub buf: FixedBuf,
    pub addr: Box<[u8]>,
    pub addr_len: u32,
    pub msghdr: Box<libc::msghdr>,
    pub iovec: Box<libc::iovec>,
}

impl UringSendTo {
    pub fn new(fd: RawHandle, buf: FixedBuf, target: SocketAddr) -> Self {
        let (raw_addr, raw_addr_len) = crate::io::socket::socket_addr_trans(target);
        let addr = raw_addr.into_boxed_slice();
        let addr_len = raw_addr_len as u32;

        let mut iovec = Box::new(libc::iovec {
            iov_base: buf.as_slice().as_ptr() as *mut _,
            iov_len: buf.len(),
        });
        let mut msghdr: Box<libc::msghdr> = Box::new(unsafe { std::mem::zeroed() });
        msghdr.msg_name = addr.as_ptr() as *mut _;
        msghdr.msg_namelen = addr_len;
        msghdr.msg_iov = iovec.as_mut() as *mut _;
        msghdr.msg_iovlen = 1;

        Self {
            fd: IoFd::Raw(fd),
            buf,
            addr,
            addr_len,
            msghdr,
            iovec,
        }
    }
}

impl From<SendTo> for UringSendTo {
    fn from(send_to: SendTo) -> Self {
        // Reconstruct msghdr from existing data
        let mut iovec = Box::new(libc::iovec {
            iov_base: send_to.buf.as_slice().as_ptr() as *mut _,
            iov_len: send_to.buf.len(),
        });
        let mut msghdr: Box<libc::msghdr> = Box::new(unsafe { std::mem::zeroed() });
        msghdr.msg_name = send_to.addr.as_ptr() as *mut _;
        msghdr.msg_namelen = send_to.addr_len;
        msghdr.msg_iov = iovec.as_mut() as *mut _;
        msghdr.msg_iovlen = 1;

        Self {
            fd: send_to.fd,
            buf: send_to.buf,
            addr: send_to.addr,
            addr_len: send_to.addr_len,
            msghdr,
            iovec,
        }
    }
}

impl From<UringSendTo> for SendTo {
    fn from(uring_send_to: UringSendTo) -> Self {
        Self {
            fd: uring_send_to.fd,
            buf: uring_send_to.buf,
            addr: uring_send_to.addr,
            addr_len: uring_send_to.addr_len,
        }
    }
}

/// io_uring specific state for RecvFrom operation.
/// Includes Linux-specific msghdr and iovec structures.
pub struct UringRecvFrom {
    pub fd: IoFd,
    pub buf: FixedBuf,
    pub addr: Box<[u8]>,
    pub addr_len: Box<u32>,
    pub msghdr: Box<libc::msghdr>,
    pub iovec: Box<libc::iovec>,
}

impl UringRecvFrom {
    pub fn new(fd: RawHandle, mut buf: FixedBuf) -> Self {
        let addr_buf_size = std::mem::size_of::<libc::sockaddr_storage>();
        let addr = vec![0u8; addr_buf_size].into_boxed_slice();
        let addr_len = Box::new(addr_buf_size as u32);

        let mut iovec = Box::new(libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut _,
            iov_len: buf.capacity(),
        });
        let mut msghdr: Box<libc::msghdr> = Box::new(unsafe { std::mem::zeroed() });
        msghdr.msg_name = addr.as_ptr() as *mut _;
        msghdr.msg_namelen = *addr_len;
        msghdr.msg_iov = iovec.as_mut() as *mut _;
        msghdr.msg_iovlen = 1;

        Self {
            fd: IoFd::Raw(fd),
            buf,
            addr,
            addr_len,
            msghdr,
            iovec,
        }
    }

    pub fn get_addr_len(&self) -> usize {
        self.msghdr.msg_namelen as usize
    }
}

impl From<RecvFrom> for UringRecvFrom {
    fn from(recv_from: RecvFrom) -> Self {
        let mut buf = recv_from.buf;
        let mut iovec = Box::new(libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut _,
            iov_len: buf.capacity(),
        });
        let mut msghdr: Box<libc::msghdr> = Box::new(unsafe { std::mem::zeroed() });
        msghdr.msg_name = recv_from.addr.as_ptr() as *mut _;
        msghdr.msg_namelen = *recv_from.addr_len;
        msghdr.msg_iov = iovec.as_mut() as *mut _;
        msghdr.msg_iovlen = 1;

        Self {
            fd: recv_from.fd,
            buf,
            addr: recv_from.addr,
            addr_len: recv_from.addr_len,
            msghdr,
            iovec,
        }
    }
}

impl From<UringRecvFrom> for RecvFrom {
    fn from(uring_recv_from: UringRecvFrom) -> Self {
        Self {
            fd: uring_recv_from.fd,
            buf: uring_recv_from.buf,
            addr: uring_recv_from.addr,
            addr_len: uring_recv_from.addr_len,
        }
    }
}

/// io_uring specific state for Timeout operation.
/// Includes the timespec buffer needed by the kernel.
pub struct UringTimeout {
    pub duration: std::time::Duration,
    /// Timespec buffer: [seconds, nanoseconds]
    pub ts: [i64; 2],
}

impl From<Timeout> for UringTimeout {
    fn from(timeout: Timeout) -> Self {
        Self {
            duration: timeout.duration,
            ts: [0, 0],
        }
    }
}

impl From<UringTimeout> for Timeout {
    fn from(uring_timeout: UringTimeout) -> Self {
        Self {
            duration: uring_timeout.duration,
        }
    }
}

/// io_uring specific state for Wakeup operation.
/// Uses eventfd read buffer.
pub struct UringWakeup {
    pub fd: IoFd,
    pub buf: Box<[u8; 8]>,
}

impl UringWakeup {
    pub fn new(fd: RawHandle) -> Self {
        Self {
            fd: IoFd::Raw(fd),
            buf: Box::new([0u8; 8]),
        }
    }
}

impl From<Wakeup> for UringWakeup {
    fn from(wakeup: Wakeup) -> Self {
        Self {
            fd: wakeup.fd,
            buf: Box::new([0u8; 8]),
        }
    }
}

impl From<UringWakeup> for Wakeup {
    fn from(uring_wakeup: UringWakeup) -> Self {
        Self {
            fd: uring_wakeup.fd,
        }
    }
}

/// io_uring specific state for Open operation.
/// Uses CString for path on Linux.
pub struct UringOpen {
    pub path: std::ffi::CString,
    pub flags: i32,
    pub mode: u32,
}

impl From<Open> for UringOpen {
    fn from(open: Open) -> Self {
        // Convert UTF-8 bytes to CString
        let path = std::ffi::CString::new(open.path)
            .unwrap_or_else(|_| std::ffi::CString::new("").unwrap());
        Self {
            path,
            flags: open.flags,
            mode: open.mode,
        }
    }
}

impl From<UringOpen> for Open {
    fn from(uring_open: UringOpen) -> Self {
        Self {
            path: uring_open.path.into_bytes(),
            flags: uring_open.flags,
            mode: uring_open.mode,
        }
    }
}

// ============================================================================
// UringOp Enum Definition
// ============================================================================

/// The io_uring platform-specific operation enum.
/// Each variant wraps either a cross-platform op struct or a platform-specific state.
pub enum UringOp {
    ReadFixed(ReadFixed),
    WriteFixed(WriteFixed),
    Recv(Recv),
    Send(OpSend),
    Accept(UringAccept),
    Connect(Connect),
    RecvFrom(UringRecvFrom),
    SendTo(UringSendTo),
    Open(UringOpen),
    Close(Close),
    Fsync(Fsync),
    Wakeup(UringWakeup),
    Timeout(UringTimeout),
}

impl PlatformOp for UringOp {}

// ============================================================================
// IntoPlatformOp Implementations
// ============================================================================

macro_rules! impl_into_uring_op_direct {
    ($Type:ident) => {
        impl IntoPlatformOp<UringDriver> for $Type {
            fn into_platform_op(self) -> UringOp {
                UringOp::$Type(self)
            }
            fn from_platform_op(op: UringOp) -> Self {
                match op {
                    UringOp::$Type(val) => val,
                    _ => panic!(concat!(
                        "Driver returned mismatched Op type: expected ",
                        stringify!($Type)
                    )),
                }
            }
        }
    };
}

macro_rules! impl_into_uring_op_convert {
    ($GenericType:ident, $UringType:ident, $Variant:ident) => {
        impl IntoPlatformOp<UringDriver> for $GenericType {
            fn into_platform_op(self) -> UringOp {
                UringOp::$Variant(self.into())
            }
            fn from_platform_op(op: UringOp) -> Self {
                match op {
                    UringOp::$Variant(val) => val.into(),
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
impl_into_uring_op_direct!(ReadFixed);
impl_into_uring_op_direct!(WriteFixed);
impl_into_uring_op_direct!(Recv);
impl_into_uring_op_direct!(Connect);
impl_into_uring_op_direct!(Close);
impl_into_uring_op_direct!(Fsync);

// Conversions for platform-specific state
impl_into_uring_op_convert!(Accept, UringAccept, Accept);
impl_into_uring_op_convert!(SendTo, UringSendTo, SendTo);
impl_into_uring_op_convert!(RecvFrom, UringRecvFrom, RecvFrom);
impl_into_uring_op_convert!(Open, UringOpen, Open);
impl_into_uring_op_convert!(Timeout, UringTimeout, Timeout);
impl_into_uring_op_convert!(Wakeup, UringWakeup, Wakeup);

// Manual implementation for Send because of name conflict with OpSend
impl IntoPlatformOp<UringDriver> for OpSend {
    fn into_platform_op(self) -> UringOp {
        UringOp::Send(self)
    }
    fn from_platform_op(op: UringOp) -> Self {
        match op {
            UringOp::Send(val) => val,
            _ => panic!("Driver returned mismatched Op type: expected Send"),
        }
    }
}
