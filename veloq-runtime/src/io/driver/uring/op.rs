//! io_uring Platform-Specific Operation Definitions
//!
//! This module defines:
//! - `UringOp`: The Type-Erased operation struct using Unions and VTables
//! - `OpVTable`: The virtual table for dynamic dispatch without enums
//! - `IntoPlatformOp` implementations using blind casting

use crate::io::driver::PlatformOp;
use crate::io::driver::uring::UringDriver;
use crate::io::driver::uring::submit;
use crate::io::op::{
    Accept, Close, Connect, Fallocate, Fsync, IntoPlatformOp, IoFd, Open, ReadFixed, Recv,
    RecvFrom, Send as OpSend, SendTo, SyncFileRange, Timeout, Wakeup, WriteFixed,
};
use io_uring::squeue;
use std::io;
use std::mem::ManuallyDrop;

// ============================================================================
// VTable Definition
// ============================================================================

pub type MakeSqeFn = unsafe fn(op: &mut UringOp) -> squeue::Entry;
pub type OnCompleteFn = unsafe fn(op: &mut UringOp, result: i32) -> io::Result<usize>;
pub type DropFn = unsafe fn(op: &mut UringOp);
pub type GetFdFn = unsafe fn(op: &UringOp) -> Option<IoFd>;

pub struct OpVTable {
    pub make_sqe: MakeSqeFn,
    pub on_complete: OnCompleteFn,
    pub drop: DropFn,
    pub get_fd: GetFdFn,
}

// ============================================================================
// UringOp Struct & Union (Type-Erased)
// ============================================================================

#[repr(C)]
pub struct UringOp {
    /// Virtual Table for dynamic dispatch
    pub vtable: &'static OpVTable,

    /// Type-erased payload
    pub payload: UringOpPayload,
}

impl PlatformOp for UringOp {}

impl UringOp {
    pub fn get_fd(&self) -> Option<IoFd> {
        unsafe { (self.vtable.get_fd)(self) }
    }
}

impl Drop for UringOp {
    fn drop(&mut self) {
        unsafe { (self.vtable.drop)(self) };
    }
}

// Ensure proper alignment
#[repr(C)]
pub union UringOpPayload {
    pub read: ManuallyDrop<ReadFixed>,
    pub write: ManuallyDrop<WriteFixed>,
    pub recv: ManuallyDrop<Recv>,
    pub send: ManuallyDrop<OpSend>,
    pub connect: ManuallyDrop<Connect>,
    pub accept: ManuallyDrop<AcceptPayload>,
    pub send_to: ManuallyDrop<SendToPayload>,
    pub recv_from: ManuallyDrop<RecvFromPayload>,
    pub open: ManuallyDrop<OpenPayload>,
    pub close: ManuallyDrop<Close>,
    pub fsync: ManuallyDrop<Fsync>,
    pub sync_range: ManuallyDrop<SyncFileRange>,
    pub fallocate: ManuallyDrop<Fallocate>,
    pub wakeup: ManuallyDrop<WakeupPayload>,
    pub timeout: ManuallyDrop<TimeoutPayload>,
}

// ============================================================================
// Payload Structures for Complex Ops
// ============================================================================

pub struct AcceptPayload {
    pub op: Accept,
}

pub struct SendToPayload {
    pub op: SendTo,
    pub msg_name: libc::sockaddr_storage,
    pub msg_namelen: libc::socklen_t,
    pub iovec: [libc::iovec; 1],
    pub msghdr: libc::msghdr,
}

pub struct RecvFromPayload {
    pub op: RecvFrom,
    pub msg_name: libc::sockaddr_storage,
    pub msg_namelen: libc::socklen_t,
    pub iovec: [libc::iovec; 1],
    pub msghdr: libc::msghdr,
}

pub struct OpenPayload {
    pub op: Open,
}

pub struct WakeupPayload {
    pub op: Wakeup,
    pub buf: [u8; 8],
}

pub struct TimeoutPayload {
    pub op: Timeout,
    pub ts: [i64; 2],
}

// ============================================================================
// IntoPlatformOp Implementations
// ============================================================================

macro_rules! impl_into_uring_op {
    ($Type:ident, $Field:ident, $MakeSqe:ident, $OnComplete:ident, $Drop:ident, $GetFd:ident) => {
        impl IntoPlatformOp<UringDriver> for $Type {
            fn into_platform_op(self) -> UringOp {
                const TABLE: OpVTable = OpVTable {
                    make_sqe: submit::$MakeSqe,
                    on_complete: submit::$OnComplete,
                    drop: submit::$Drop,
                    get_fd: submit::$GetFd,
                };

                UringOp {
                    vtable: &TABLE,
                    payload: UringOpPayload {
                        $Field: ManuallyDrop::new(self),
                    },
                }
            }
            fn from_platform_op(op: UringOp) -> Self {
                let op = ManuallyDrop::new(op);
                unsafe { ManuallyDrop::into_inner(std::ptr::read(&op.payload.$Field)) }
            }
        }
    };
    ($Type:ident, $Field:ident, $MakeSqe:ident, $OnComplete:ident, $Drop:ident, $GetFd:ident, $Payload:ident, $Constructor:expr) => {
        impl IntoPlatformOp<UringDriver> for $Type {
            fn into_platform_op(self) -> UringOp {
                const TABLE: OpVTable = OpVTable {
                    make_sqe: submit::$MakeSqe,
                    on_complete: submit::$OnComplete,
                    drop: submit::$Drop,
                    get_fd: submit::$GetFd,
                };

                let construct: fn($Type) -> $Payload = $Constructor;
                let payload = construct(self);

                UringOp {
                    vtable: &TABLE,
                    payload: UringOpPayload {
                        $Field: ManuallyDrop::new(payload),
                    },
                }
            }
            fn from_platform_op(op: UringOp) -> Self {
                let op = ManuallyDrop::new(op);
                unsafe { ManuallyDrop::into_inner(std::ptr::read(&op.payload.$Field)).op }
            }
        }
    };
}

impl_into_uring_op!(
    ReadFixed,
    read,
    make_sqe_read_fixed,
    on_complete_read_fixed,
    drop_read_fixed,
    get_fd_read_fixed
);
impl_into_uring_op!(
    WriteFixed,
    write,
    make_sqe_write_fixed,
    on_complete_write_fixed,
    drop_write_fixed,
    get_fd_write_fixed
);
impl_into_uring_op!(
    Recv,
    recv,
    make_sqe_recv,
    on_complete_recv,
    drop_recv,
    get_fd_recv
);
impl_into_uring_op!(
    OpSend,
    send,
    make_sqe_send,
    on_complete_send,
    drop_send,
    get_fd_send
);
impl_into_uring_op!(
    Connect,
    connect,
    make_sqe_connect,
    on_complete_connect,
    drop_connect,
    get_fd_connect
);

impl_into_uring_op!(
    Close,
    close,
    make_sqe_close,
    on_complete_close,
    drop_close,
    get_fd_close
);
impl_into_uring_op!(
    Fsync,
    fsync,
    make_sqe_fsync,
    on_complete_fsync,
    drop_fsync,
    get_fd_fsync
);
impl_into_uring_op!(
    SyncFileRange,
    sync_range,
    make_sqe_sync_range,
    on_complete_sync_range,
    drop_sync_range,
    get_fd_sync_range
);
impl_into_uring_op!(
    Fallocate,
    fallocate,
    make_sqe_fallocate,
    on_complete_fallocate,
    drop_fallocate,
    get_fd_fallocate
);

// Manual implementations for ops with extras
impl_into_uring_op!(
    Accept,
    accept,
    make_sqe_accept,
    on_complete_accept,
    drop_accept,
    get_fd_accept,
    AcceptPayload,
    |op| AcceptPayload { op }
);

impl_into_uring_op!(
    SendTo,
    send_to,
    make_sqe_send_to,
    on_complete_send_to,
    drop_send_to,
    get_fd_send_to,
    SendToPayload,
    |op| {
        let (msg_name, msg_namelen) = crate::io::socket::socket_addr_to_storage(op.addr);
        SendToPayload {
            op,
            msg_name,
            msg_namelen: msg_namelen as libc::socklen_t,
            iovec: [unsafe { std::mem::zeroed() }],
            msghdr: unsafe { std::mem::zeroed() },
        }
    }
);

impl_into_uring_op!(
    RecvFrom,
    recv_from,
    make_sqe_recv_from,
    on_complete_recv_from,
    drop_recv_from,
    get_fd_recv_from,
    RecvFromPayload,
    |op| RecvFromPayload {
        op,
        msg_name: unsafe { std::mem::zeroed() },
        msg_namelen: std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t,
        iovec: [unsafe { std::mem::zeroed() }],
        msghdr: unsafe { std::mem::zeroed() },
    }
);

impl_into_uring_op!(
    Open,
    open,
    make_sqe_open,
    on_complete_open,
    drop_open,
    get_fd_open,
    OpenPayload,
    |op| OpenPayload { op }
);

impl_into_uring_op!(
    Wakeup,
    wakeup,
    make_sqe_wakeup,
    on_complete_wakeup,
    drop_wakeup,
    get_fd_wakeup,
    WakeupPayload,
    |op| WakeupPayload { op, buf: [0; 8] }
);

impl_into_uring_op!(
    Timeout,
    timeout,
    make_sqe_timeout,
    on_complete_timeout,
    drop_timeout,
    get_fd_timeout,
    TimeoutPayload,
    |op| TimeoutPayload { op, ts: [0; 2] }
);
