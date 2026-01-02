//! io_uring Platform-Specific Operation Definitions
//!
//! This module defines:
//! - `UringAbi`: The platform-specific ABI for io_uring
//! - `UringOp` type alias: The unified operation enum specialized for io_uring
//! - Platform-specific "extra" structures (e.g., `UringSendToExtras`)
//! - `IntoPlatformOp` implementations

use crate::io::buffer::BufPool;
use crate::io::driver::PlatformOp;
use crate::io::driver::uring::UringDriver;
use crate::io::op::{
    Accept, Close, Connect, Fallocate, Fsync, IntoPlatformOp, OpAbi, Open, Operation, ReadFixed,
    Recv, RecvFrom, Send as OpSend, SendTo, SyncFileRange, Timeout, Wakeup, WriteFixed,
};
use std::ffi::CString;
use std::net::SocketAddr;

// ============================================================================
// Platform-Specific Extra State Structures
// ============================================================================

/// Extras for SendTo: needs storage for address, msghdr, and iovec.
pub struct UringSendToExtras {
    pub addr: libc::sockaddr_storage,
    pub addr_len: libc::socklen_t,
    pub msghdr: libc::msghdr,
    pub iovec: [libc::iovec; 1],
}

impl UringSendToExtras {
    pub fn new(target: SocketAddr) -> Self {
        let (addr, addr_len) = crate::io::socket::socket_addr_to_storage(target);
        Self {
            addr,
            addr_len,
            msghdr: unsafe { std::mem::zeroed() },
            iovec: [unsafe { std::mem::zeroed() }],
        }
    }
}

/// Extras for RecvFrom: needs storage for address, msghdr, and iovec.
pub struct UringRecvFromExtras {
    pub addr: libc::sockaddr_storage,
    pub addr_len: libc::socklen_t,
    pub msghdr: libc::msghdr,
    pub iovec: [libc::iovec; 1],
}

impl UringRecvFromExtras {
    pub fn new() -> Self {
        Self {
            addr: unsafe { std::mem::zeroed() },
            addr_len: std::mem::size_of::<libc::sockaddr_storage>() as _,
            msghdr: unsafe { std::mem::zeroed() },
            iovec: [unsafe { std::mem::zeroed() }],
        }
    }
}

/// Extras for Open: needs CString path.
pub struct UringOpenExtras {
    pub path: CString,
}

/// Extras for Timeout: needs timespec.
pub struct UringTimeoutExtras {
    pub ts: [i64; 2],
}

/// Extras for Wakeup: needs buffer.
pub struct UringWakeupExtras {
    pub buf: [u8; 8],
}

// ============================================================================
// UringAbi Implementation
// ============================================================================

pub struct UringAbi;

impl OpAbi for UringAbi {
    type ReadFixed = ();
    type WriteFixed = ();
    type Recv = ();
    type Send = ();
    type Accept = (); // Generic Accept has SockAddrStorage which is sufficient
    type Connect = ();
    type Close = ();
    type Fsync = ();
    type SyncFileRange = ();
    type Fallocate = ();
    type SendTo = UringSendToExtras;
    type RecvFrom = UringRecvFromExtras;
    type Open = UringOpenExtras;
    type Wakeup = UringWakeupExtras;
    type Timeout = UringTimeoutExtras;
}

pub type UringOp<P> = Operation<UringAbi, P>;

impl<P: BufPool> PlatformOp for UringOp<P> {}

// ============================================================================
// IntoPlatformOp Implementations
// ============================================================================

macro_rules! impl_into_uring_op_direct {
    ($Type:ident) => {
        impl<P: BufPool> IntoPlatformOp<UringDriver<P>> for $Type<P> {
            fn into_platform_op(self) -> UringOp<P> {
                UringOp::$Type(self, ())
            }
            fn from_platform_op(op: UringOp<P>) -> Self {
                match op {
                    UringOp::$Type(val, _) => val,
                    _ => panic!(concat!(
                        "Driver returned mismatched Op type: expected ",
                        stringify!($Type)
                    )),
                }
            }
        }
    };
}

macro_rules! impl_into_uring_op_direct_no_generic {
    ($Type:ident) => {
        impl<P: BufPool> IntoPlatformOp<UringDriver<P>> for $Type {
            fn into_platform_op(self) -> UringOp<P> {
                UringOp::$Type(self, ())
            }
            fn from_platform_op(op: UringOp<P>) -> Self {
                match op {
                    UringOp::$Type(val, _) => val,
                    _ => panic!(concat!(
                        "Driver returned mismatched Op type: expected ",
                        stringify!($Type)
                    )),
                }
            }
        }
    };
}

impl_into_uring_op_direct!(ReadFixed);
impl_into_uring_op_direct!(WriteFixed);
impl_into_uring_op_direct!(Recv);

impl_into_uring_op_direct_no_generic!(Connect);
impl_into_uring_op_direct_no_generic!(Close);
impl_into_uring_op_direct_no_generic!(Fsync);
impl_into_uring_op_direct_no_generic!(SyncFileRange);
impl_into_uring_op_direct_no_generic!(Fallocate);
impl_into_uring_op_direct_no_generic!(Accept);

// Manual implementations for ops with extras

impl<P: BufPool> IntoPlatformOp<UringDriver<P>> for SendTo<P> {
    fn into_platform_op(self) -> UringOp<P> {
        let extras = UringSendToExtras::new(self.addr);
        UringOp::SendTo(self, extras)
    }

    fn from_platform_op(op: UringOp<P>) -> Self {
        match op {
            UringOp::SendTo(val, _) => val,
            _ => panic!("Driver returned mismatched Op type: expected SendTo"),
        }
    }
}

impl<P: BufPool> IntoPlatformOp<UringDriver<P>> for RecvFrom<P> {
    fn into_platform_op(self) -> UringOp<P> {
        let extras = UringRecvFromExtras::new();
        UringOp::RecvFrom(self, extras)
    }

    fn from_platform_op(op: UringOp<P>) -> Self {
        match op {
            UringOp::RecvFrom(mut val, extras) => {
                // Update the address from the msghdr/addr in extras
                let len = extras.msghdr.msg_namelen as usize;
                let addr_bytes = unsafe {
                    std::slice::from_raw_parts(&extras.addr as *const _ as *const u8, len)
                };
                val.addr = crate::io::socket::to_socket_addr(addr_bytes).ok();
                val
            }
            _ => panic!("Driver returned mismatched Op type: expected RecvFrom"),
        }
    }
}

impl<P: BufPool> IntoPlatformOp<UringDriver<P>> for Open {
    fn into_platform_op(self) -> UringOp<P> {
        // Convert the generic path (Vec<u8> -> CString)
        // Note generically we store raw bytes. On Linux it should be null-terminated or we add it.
        // CString::new checks for internal nulls and adds one at end.
        let path = CString::new(self.path.clone()).unwrap_or_else(|_| CString::new("").unwrap());
        UringOp::Open(self, UringOpenExtras { path })
    }

    fn from_platform_op(op: UringOp<P>) -> Self {
        match op {
            UringOp::Open(val, _) => val,
            _ => panic!("Driver returned mismatched Op type: expected Open"),
        }
    }
}

impl<P: BufPool> IntoPlatformOp<UringDriver<P>> for Timeout {
    fn into_platform_op(self) -> UringOp<P> {
        UringOp::Timeout(self, UringTimeoutExtras { ts: [0, 0] })
    }

    fn from_platform_op(op: UringOp<P>) -> Self {
        match op {
            UringOp::Timeout(val, _) => val,
            _ => panic!("Driver returned mismatched Op type: expected Timeout"),
        }
    }
}

impl<P: BufPool> IntoPlatformOp<UringDriver<P>> for Wakeup {
    fn into_platform_op(self) -> UringOp<P> {
        UringOp::Wakeup(self, UringWakeupExtras { buf: [0; 8] })
    }

    fn from_platform_op(op: UringOp<P>) -> Self {
        match op {
            UringOp::Wakeup(val, _) => val,
            _ => panic!("Driver returned mismatched Op type: expected Wakeup"),
        }
    }
}

// Manual implementation for Send because of name conflict
impl<P: BufPool> IntoPlatformOp<UringDriver<P>> for OpSend<P> {
    fn into_platform_op(self) -> UringOp<P> {
        UringOp::Send(self, ())
    }
    fn from_platform_op(op: UringOp<P>) -> Self {
        match op {
            UringOp::Send(val, _) => val,
            _ => panic!("Driver returned mismatched Op type: expected Send"),
        }
    }
}
