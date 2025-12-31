mod ext;
mod ops;
#[cfg(test)]
mod tests;

use crate::runtime::buffer::{BufferPool, FixedBuf};
use crate::runtime::driver::op_registry::{OpEntry, OpRegistry};
use crate::runtime::driver::Driver;
use crate::runtime::op::IoResources;
use ext::Extensions;
use ops::IocpSubmit;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::io;
use std::task::{Context, Poll};
use std::time::Instant;
use windows_sys::Win32::Foundation::{
    ERROR_HANDLE_EOF, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, SO_UPDATE_ACCEPT_CONTEXT, SO_UPDATE_CONNECT_CONTEXT, SOCKADDR, SOCKADDR_IN,
    SOCKADDR_IN6, SOCKADDR_STORAGE, SOCKET, SOL_SOCKET, setsockopt,
};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED, PostQueuedCompletionStatus,
};

struct TimerEntry {
    deadline: Instant,
    user_data: usize,
}

impl PartialEq for TimerEntry {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline
    }
}

impl Eq for TimerEntry {}

impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimerEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other.deadline.cmp(&self.deadline)
    }
}

pub struct IocpDriver {
    port: HANDLE,
    ops: OpRegistry<Option<Box<OverlappedEntry>>>,
    buffer_pool: BufferPool,
    extensions: Extensions,
    timers: BinaryHeap<TimerEntry>,
}

#[repr(C)]
pub struct OverlappedEntry {
    inner: OVERLAPPED,
    user_data: usize,
}

impl IocpDriver {
    pub fn new(entries: u32) -> io::Result<Self> {
        // Create a new completion port.
        let port =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 0) };

        if port.is_null() {
            return Err(io::Error::last_os_error());
        }

        // Load extensions
        let extensions = Extensions::new()?;

        Ok(Self {
            port,
            ops: OpRegistry::with_capacity(entries as usize),
            buffer_pool: BufferPool::new(),
            extensions,
            timers: BinaryHeap::new(),
        })
    }

    /// Retrieve completion events from the port.
    /// timeout_ms: 0 for poll, u32::MAX for wait.
    fn get_completion(&mut self, timeout_ms: u32) -> io::Result<()> {
        let mut bytes_transferred = 0;
        let mut completion_key = 0;
        let mut overlapped = std::ptr::null_mut();

        let now = Instant::now();
        let mut wait_ms = timeout_ms;
        if let Some(timer) = self.timers.peek() {
            let delay = if timer.deadline > now {
                (timer.deadline - now).as_millis() as u32
            } else {
                0
            };
            wait_ms = std::cmp::min(wait_ms, delay);
        }

        let res = unsafe {
            GetQueuedCompletionStatus(
                self.port,
                &mut bytes_transferred,
                &mut completion_key,
                &mut overlapped,
                wait_ms,
            )
        };

        // Process expired timers
        let now = Instant::now();
        while let Some(timer) = self.timers.peek() {
            if timer.deadline > now {
                break;
            }
            let entry = self.timers.pop().unwrap();
            let user_data = entry.user_data;
            if let Some(op) = self.ops.get_mut(user_data) {
                if !op.cancelled && op.result.is_none() {
                    op.result = Some(Ok(0));
                    if let Some(waker) = op.waker.take() {
                        waker.wake();
                    }
                }
            }
        }

        if overlapped.is_null() {
            if res == 0 {
                let err = unsafe { GetLastError() };
                if err == WAIT_TIMEOUT {
                    return Ok(());
                }
                return Err(io::Error::from_raw_os_error(err as i32));
            }
            return Ok(());
        }

        let entry = unsafe { &*(overlapped as *const OverlappedEntry) };
        let user_data = entry.user_data;

        if self.ops.contains(user_data) {
            let op = &mut self.ops[user_data];

            if op.result.is_none() {
                let result = if res == 0 {
                    let err = unsafe { GetLastError() };
                    if err == ERROR_HANDLE_EOF {
                        Ok(bytes_transferred)
                    } else {
                        Err(io::Error::from_raw_os_error(err as i32))
                    }
                } else {
                    Ok(bytes_transferred)
                };

                if result.is_ok() {
                    match &mut op.resources {
                        IoResources::Accept(accept_op) => {
                            if let Some(fd) = accept_op.fd.raw() {
                                let accept_socket = accept_op.accept_socket;
                                let listen_socket = fd as SOCKET;
                                let ret = unsafe {
                                    setsockopt(
                                        accept_socket as SOCKET,
                                        SOL_SOCKET,
                                        SO_UPDATE_ACCEPT_CONTEXT,
                                        &listen_socket as *const _ as *const _,
                                        std::mem::size_of_val(&listen_socket) as i32,
                                    )
                                };
                                if ret != 0 {
                                    op.result = Some(Err(io::Error::last_os_error()));
                                } else {
                                    // Parse addresses and backfill
                                    const MIN_ADDR_LEN: usize =
                                        std::mem::size_of::<SOCKADDR_STORAGE>() + 16;
                                    let split = MIN_ADDR_LEN;

                                    let mut local_sockaddr: *mut SOCKADDR = std::ptr::null_mut();
                                    let mut remote_sockaddr: *mut SOCKADDR = std::ptr::null_mut();
                                    let mut local_len: i32 = 0;
                                    let mut remote_len: i32 = 0;

                                    unsafe {
                                        (self.extensions.get_accept_ex_sockaddrs)(
                                            accept_op.addr.as_ptr() as *const _,
                                            0,
                                            split as u32,
                                            split as u32,
                                            &mut local_sockaddr,
                                            &mut local_len,
                                            &mut remote_sockaddr,
                                            &mut remote_len,
                                        );
                                    }

                                    if !remote_sockaddr.is_null() && remote_len > 0 {
                                        unsafe {
                                            let family = (*remote_sockaddr).sa_family;
                                            if family == AF_INET as u16 {
                                                let addr_in =
                                                    &*(remote_sockaddr as *const SOCKADDR_IN);
                                                let ip = std::net::Ipv4Addr::from(
                                                    addr_in.sin_addr.S_un.S_addr.to_ne_bytes(),
                                                );
                                                let port = u16::from_be(addr_in.sin_port);
                                                accept_op.remote_addr =
                                                    Some(std::net::SocketAddr::V4(
                                                        std::net::SocketAddrV4::new(ip, port),
                                                    ));
                                            } else if family == AF_INET6 as u16 {
                                                let addr_in6 =
                                                    &*(remote_sockaddr as *const SOCKADDR_IN6);
                                                let ip = std::net::Ipv6Addr::from(
                                                    addr_in6.sin6_addr.u.Byte,
                                                );
                                                let port = u16::from_be(addr_in6.sin6_port);
                                                let flowinfo = addr_in6.sin6_flowinfo;
                                                let scope_id = addr_in6.Anonymous.sin6_scope_id;
                                                accept_op.remote_addr =
                                                    Some(std::net::SocketAddr::V6(
                                                        std::net::SocketAddrV6::new(
                                                            ip, port, flowinfo, scope_id,
                                                        ),
                                                    ));
                                            }
                                        }
                                    }
                                }
                            } else {
                                op.result = Some(Err(io::Error::new(
                                    io::ErrorKind::Other,
                                    "Invalid listen socket fd",
                                )));
                            }
                        }
                        IoResources::Connect(connect_op) => {
                            if let Some(fd) = connect_op.fd.raw() {
                                let ret = unsafe {
                                    setsockopt(
                                        fd as SOCKET,
                                        SOL_SOCKET,
                                        SO_UPDATE_CONNECT_CONTEXT,
                                        std::ptr::null(),
                                        0,
                                    )
                                };
                                if ret != 0 {
                                    op.result = Some(Err(io::Error::last_os_error()));
                                }
                            } else {
                                op.result = Some(Err(io::Error::new(
                                    io::ErrorKind::Other,
                                    "Invalid socket fd",
                                )));
                            }
                        }
                        _ => {}
                    }
                }

                if op.result.is_none() {
                    op.result = Some(result);
                }
            }

            if let Some(waker) = op.waker.take() {
                waker.wake();
            }
        }

        Ok(())
    }

    pub fn register_files(
        &mut self,
        files: &[crate::runtime::op::SysRawOp],
    ) -> io::Result<Vec<crate::runtime::op::IoFd>> {
        Ok(files
            .iter()
            .map(|&h| crate::runtime::op::IoFd::Raw(h))
            .collect())
    }

    pub fn unregister_files(&mut self, _files: Vec<crate::runtime::op::IoFd>) -> io::Result<()> {
        Ok(())
    }

    pub fn alloc_fixed_buffer(&self) -> Option<FixedBuf> {
        self.buffer_pool.alloc()
    }
}

impl Driver for IocpDriver {
    fn reserve_op(&mut self) -> usize {
        self.ops.insert(OpEntry::new(IoResources::None, None))
    }

    fn submit_op_resources(&mut self, user_data: usize, mut resources: IoResources) {
        // 1. Prepare stable Overlapped
        let mut entry = Box::new(OverlappedEntry {
            inner: unsafe { std::mem::zeroed() },
            user_data,
        });

        let overlapped_ptr = &mut entry.inner as *mut OVERLAPPED;

        let (res_err, post_completion) = unsafe {
            resources.submit(self.port, overlapped_ptr, &self.extensions)
        };

        if let IoResources::Timeout(op) = &resources {
            let deadline = Instant::now() + op.duration;
            self.timers.push(TimerEntry {
                deadline,
                user_data,
            });
        }

        if let Some(op) = self.ops.get_mut(user_data) {
            op.resources = resources;
            op.platform_data = Some(entry);

            if let Some(err) = res_err {
                op.result = Some(Err(err));
            }

            if post_completion {
                unsafe {
                    PostQueuedCompletionStatus(self.port, 0, 0, overlapped_ptr);
                }
            }
        }
    }

    fn poll_op(
        &mut self,
        user_data: usize,
        cx: &mut Context<'_>,
    ) -> Poll<(io::Result<u32>, IoResources)> {
        self.ops.poll_op(user_data, cx)
    }

    fn submit(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn wait(&mut self) -> io::Result<()> {
        if self.ops.is_empty() {
            return Ok(());
        }
        self.get_completion(u32::MAX)
    }

    fn process_completions(&mut self) {
        let _ = self.get_completion(0);
    }

    fn cancel_op(&mut self, user_data: usize) {
        if let Some(op) = self.ops.get_mut(user_data) {
            op.cancelled = true;

            // Extract fd from resources
            let fd = match &op.resources {
                IoResources::ReadFixed(r) => r.fd.raw().map(|h| h as HANDLE),
                IoResources::WriteFixed(r) => r.fd.raw().map(|h| h as HANDLE),
                IoResources::Recv(r) => r.fd.raw().map(|h| h as HANDLE),
                IoResources::Send(r) => r.fd.raw().map(|h| h as HANDLE),
                IoResources::Accept(r) => r.fd.raw().map(|h| h as HANDLE),
                IoResources::Connect(r) => r.fd.raw().map(|h| h as HANDLE),
                IoResources::SendTo(r) => r.fd.raw().map(|h| h as HANDLE),
                IoResources::RecvFrom(r) => r.fd.raw().map(|h| h as HANDLE),
                _ => None,
            };

            // Call CancelIoEx if we have both fd and overlapped
            if let (Some(fd), Some(overlapped)) = (fd, &op.platform_data) {
                unsafe {
                    use windows_sys::Win32::System::IO::CancelIoEx;
                    let _ = CancelIoEx(fd, &overlapped.inner as *const _ as *mut _);
                }
            }
        }
    }

    fn register_files(
        &mut self,
        files: &[crate::runtime::op::SysRawOp],
    ) -> io::Result<Vec<crate::runtime::op::IoFd>> {
        self.register_files(files)
    }

    fn unregister_files(&mut self, files: Vec<crate::runtime::op::IoFd>) -> io::Result<()> {
        self.unregister_files(files)
    }

    fn alloc_fixed_buffer(&self) -> Option<FixedBuf> {
        self.buffer_pool.alloc()
    }
}

impl Drop for IocpDriver {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.port) };
    }
}
