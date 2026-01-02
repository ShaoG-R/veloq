mod blocking;
mod ext;
pub mod op;
mod submit;
#[cfg(test)]
mod tests;

use crate::io::driver::op_registry::{OpEntry, OpRegistry};
use crate::io::driver::{Driver, RemoteWaker};
use blocking::ThreadPool;
use ext::Extensions;
use op::IocpOp;
use std::io;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use submit::{IocpSubmit, SubmissionResult};

const WAKEUP_USER_DATA: usize = usize::MAX;

use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_HANDLE_EOF, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED, PostQueuedCompletionStatus,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use veloq_wheel::{TaskId, Wheel, WheelConfig};

use op::OverlappedEntry;

pub enum PlatformData {
    Timer(TaskId),
    None,
}

pub struct IocpDriver {
    port: HANDLE,
    ops: OpRegistry<IocpOp, PlatformData>,
    extensions: Extensions,
    wheel: Wheel<usize>,
    registered_files: Vec<Option<HANDLE>>,
    free_slots: Vec<usize>,
    pool: ThreadPool,
}

struct IocpWaker(HANDLE);

unsafe impl Send for IocpWaker {}
unsafe impl Sync for IocpWaker {}

impl RemoteWaker for IocpWaker {
    fn wake(&self) -> io::Result<()> {
        let res = unsafe {
            PostQueuedCompletionStatus(self.0, 0, WAKEUP_USER_DATA, std::ptr::null_mut())
        };
        if res == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for IocpWaker {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

impl IocpDriver {
    pub fn new(config: &crate::config::Config) -> io::Result<Self> {
        // Create a new completion port.
        let port =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 0) };

        if port.is_null() {
            return Err(io::Error::last_os_error());
        }

        // Load extensions
        let extensions = Extensions::new()?;

        let entries = config.iocp.entries;

        Ok(Self {
            port,
            ops: OpRegistry::with_capacity(entries as usize),
            extensions,
            wheel: Wheel::new(WheelConfig::default()),
            registered_files: Vec::new(),
            free_slots: Vec::new(),
            pool: ThreadPool::new(16, 128, 1024, Duration::from_secs(30)),
        })
    }

    /// Retrieve completion events from the port.
    /// timeout_ms: 0 for poll, u32::MAX for wait.
    fn get_completion(&mut self, timeout_ms: u32) -> io::Result<()> {
        let mut bytes_transferred = 0;
        let mut completion_key = 0;
        let mut overlapped = std::ptr::null_mut();

        // Calculate timeout based on wheel
        let mut wait_ms = timeout_ms;
        if let Some(delay) = self.wheel.next_timeout() {
            let millis = delay.as_millis().min(u32::MAX as u128) as u32;
            wait_ms = std::cmp::min(wait_ms, millis);
        }

        let start = Instant::now();
        let res = unsafe {
            GetQueuedCompletionStatus(
                self.port,
                &mut bytes_transferred,
                &mut completion_key,
                &mut overlapped,
                wait_ms,
            )
        };
        let elapsed = start.elapsed();

        // Process expired timers
        let expired = self.wheel.advance(elapsed);
        for user_data in expired {
            if let Some(op) = self.ops.get_mut(user_data) {
                if !op.cancelled && op.result.is_none() {
                    op.result = Some(Ok(0));
                    if let Some(waker) = op.waker.take() {
                        waker.wake();
                    }
                }
                // Clean up platform data
                op.platform_data = PlatformData::None;
            }
        }

        // Determine user_data from overlapped or completion_key
        let user_data = if !overlapped.is_null() {
            let entry = unsafe { &*(overlapped as *const OverlappedEntry) };
            entry.user_data
        } else {
            if res == 0 {
                let err = unsafe { GetLastError() };
                if err == WAIT_TIMEOUT {
                    return Ok(());
                }
                return Err(io::Error::from_raw_os_error(err as i32));
            }
            if completion_key == WAKEUP_USER_DATA {
                return Ok(());
            }
            completion_key
        };

        if self.ops.contains(user_data) {
            // Check if cancelled
            if self.ops[user_data].cancelled {
                self.ops.remove(user_data);
                return Ok(());
            }

            let op = &mut self.ops[user_data];
            let mut result_is_ready = false;

            if op.result.is_none() {
                // If op.resources is Some(op), we have the op.
                // We should access 'entry' from op.resources.

                if let Some(iocp_op) = op.resources.as_mut() {
                    if let Some(entry) = iocp_op.entry_mut() {
                        if let Some(res) = entry.blocking_result.take() {
                            op.result = Some(res);
                            result_is_ready = true;
                        }
                    }
                }

                if !result_is_ready {
                    // Standard IOCP completion
                    let result = if res == 0 {
                        let err = unsafe { GetLastError() };
                        if err == ERROR_HANDLE_EOF {
                            Ok(bytes_transferred as usize)
                        } else {
                            Err(io::Error::from_raw_os_error(err as i32))
                        }
                    } else {
                        Ok(bytes_transferred as usize)
                    };

                    if result.is_ok() {
                        // Apply post-processing (Accept/Connect fixups)
                        let bytes = result.unwrap();
                        let final_res = op
                            .resources
                            .as_mut()
                            .unwrap()
                            .on_complete(bytes, &self.extensions);
                        op.result = Some(final_res);
                    } else {
                        op.result = Some(result);
                    }
                    result_is_ready = true;
                }
            }

            if result_is_ready {
                // Clean up resources
                op.platform_data = PlatformData::None;
                if let Some(waker) = op.waker.take() {
                    waker.wake();
                }
            }
        }

        Ok(())
    }
}

impl Driver for IocpDriver {
    type Op = IocpOp;

    fn reserve_op(&mut self) -> usize {
        self.ops.insert(OpEntry::new(None, PlatformData::None))
    }

    fn submit(&mut self, user_data: usize, op: Self::Op) {
        if let IocpOp::Timeout(t, _) = &op {
            // Handle timeout
            let id = self.wheel.insert(user_data, t.duration);
            if let Some(op_entry) = self.ops.get_mut(user_data) {
                op_entry.resources = Some(op);
                op_entry.platform_data = PlatformData::Timer(id);
            }
            return;
        }

        // 1. Move Op into Registry to pin it immediately
        let op_entry = self
            .ops
            .get_mut(user_data)
            .expect("Invalid user_data reserved");
        op_entry.resources = Some(op);

        // 2. Access the stabilized Op and its OverlappedEntry
        // Split borrows manually to satisfy borrow checker
        let port = self.port;
        let extensions = &self.extensions;
        let registered_files = &self.registered_files;

        // We get the stable reference now.
        let op_ref = self
            .ops
            .get_mut(user_data)
            .unwrap()
            .resources
            .as_mut()
            .unwrap();

        // Initialize stable Overlapped within the pinned op
        if let Some(entry) = op_ref.entry_mut() {
            entry.user_data = user_data;
            entry.inner = unsafe { std::mem::zeroed() };
            entry.blocking_result = None;
        }

        // Get pointer to the pinned OVERLAPPED
        let overlapped_ptr = op_ref
            .entry_mut()
            .map(|e| &mut e.inner as *mut OVERLAPPED)
            .unwrap_or(std::ptr::null_mut());

        // 3. Submit to kernel/pool
        let submission_result =
            unsafe { op_ref.submit(port, overlapped_ptr, extensions, registered_files) };

        match submission_result {
            Ok(SubmissionResult::Pending) => {
                // Op is pinned and pending.
            }
            Ok(SubmissionResult::PostToQueue) => unsafe {
                PostQueuedCompletionStatus(port, 0, 0, overlapped_ptr);
            },
            Ok(SubmissionResult::Offload(task)) => {
                // We perform the spawn. self.pool execute might fail.
                let spawn_result = self.pool.execute(task);

                if spawn_result.is_err() {
                    // Pool overloaded or error
                    if let Some(op_entry) = self.ops.get_mut(user_data) {
                        op_entry.result = Some(Err(io::Error::new(
                            io::ErrorKind::Other,
                            "blocking pool overloaded",
                        )));
                    }
                }
            }
            Err(e) => {
                // Submission failed immediately
                if let Some(op_entry) = self.ops.get_mut(user_data) {
                    op_entry.result = Some(Err(e));
                }
            }
        }
    }

    fn poll_op(
        &mut self,
        user_data: usize,
        cx: &mut Context<'_>,
    ) -> Poll<(io::Result<usize>, Self::Op)> {
        self.ops.poll_op(user_data, cx)
    }

    fn submit_queue(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn wait(&mut self) -> io::Result<()> {
        if self.ops.is_empty() {
            return Ok(());
        }
        self.get_completion(u32::MAX)
    }

    fn process_completions(&mut self) {
        let _ = self.get_completion(1);
    }

    fn cancel_op(&mut self, user_data: usize) {
        if let Some(op) = self.ops.get_mut(user_data) {
            op.cancelled = true;

            match &mut op.platform_data {
                PlatformData::Timer(id) => {
                    self.wheel.cancel(*id);
                    // Timer removal logic?
                }
                PlatformData::None => {
                    // Try to CancelIoEx if resources exist
                    if let Some(res) = &mut op.resources {
                        if let Some(fd) = res.get_fd() {
                            if let Ok(handle) = submit::resolve_fd(fd, &self.registered_files) {
                                if let Some(entry) = res.entry_mut() {
                                    unsafe {
                                        use windows_sys::Win32::System::IO::CancelIoEx;
                                        let _ =
                                            CancelIoEx(handle, &entry.inner as *const _ as *mut _);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let should_remove = if let Some(op) = self.ops.get_mut(user_data) {
            match op.platform_data {
                PlatformData::Timer(_) => true,
                PlatformData::None => {
                    // For IO ops, we can remove if the result is already available
                    // (either completed or failed submission).
                    // If result is None, we must wait for the completion packet.
                    op.result.is_some()
                }
            }
        } else {
            false
        };

        if should_remove {
            self.ops.remove(user_data);
        }
    }

    fn register_buffer_pool(&mut self, _pool: &crate::io::buffer::BufferPool) -> io::Result<()> {
        Ok(())
    }

    fn register_files(
        &mut self,
        files: &[crate::io::op::RawHandle],
    ) -> io::Result<Vec<crate::io::op::IoFd>> {
        let mut registered = Vec::with_capacity(files.len());
        for &handle in files {
            let idx = if let Some(idx) = self.free_slots.pop() {
                self.registered_files[idx] = Some(handle as HANDLE);
                idx
            } else {
                self.registered_files.push(Some(handle as HANDLE));
                self.registered_files.len() - 1
            };
            registered.push(crate::io::op::IoFd::Fixed(idx as u32));
        }
        Ok(registered)
    }

    fn unregister_files(&mut self, files: Vec<crate::io::op::IoFd>) -> io::Result<()> {
        for fd in files {
            if let crate::io::op::IoFd::Fixed(idx) = fd {
                let idx = idx as usize;
                if idx < self.registered_files.len() {
                    if self.registered_files[idx].is_some() {
                        self.registered_files[idx] = None;
                        self.free_slots.push(idx);
                    }
                }
            }
        }
        Ok(())
    }

    fn submit_background(&mut self, op: Self::Op) -> io::Result<()> {
        match op {
            IocpOp::Close(data, _) => {
                if let Some(fd) = data.fd.raw() {
                    let fd_val = fd as usize;
                    // Fire-and-forget close. No completion info (passed set to 0).
                    let task = blocking::BlockingTask::Close {
                        handle: fd_val,
                        completion: blocking::CompletionInfo {
                            port: 0,
                            user_data: 0,
                            overlapped: 0,
                        },
                    };
                    self.pool.execute(task).map_err(|_| {
                        io::Error::new(io::ErrorKind::Other, "blocking pool overloaded")
                    })?;
                    Ok(())
                } else {
                    Ok(())
                }
            }
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported background op",
            )),
        }
    }

    fn wake(&mut self) -> io::Result<()> {
        let res = unsafe {
            PostQueuedCompletionStatus(self.port, 0, WAKEUP_USER_DATA, std::ptr::null_mut())
        };
        if res == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn create_waker(&self) -> std::sync::Arc<dyn RemoteWaker> {
        let process = unsafe { GetCurrentProcess() };
        let mut new_handle = INVALID_HANDLE_VALUE;
        let res = unsafe {
            DuplicateHandle(
                process,
                self.port,
                process,
                &mut new_handle,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if res == 0 {
            panic!("Failed to dup handle");
        }
        std::sync::Arc::new(IocpWaker(new_handle))
    }
}

impl Drop for IocpDriver {
    fn drop(&mut self) {
        // 1. Cancel all pending operations
        let mut pending_count = 0;
        for (_user_data, op) in self.ops.iter_mut() {
            // We only care about operations that have resources (submitted) AND have no result yet.
            // If result is Some, the completion packet was already processed, so don't wait for it.
            if op.resources.is_some() && op.result.is_none() {
                if !op.cancelled {
                    // Try to CancelIoEx
                    if let Some(res) = op.resources.as_mut() {
                        // We only attempt to cancel if we can resolve the handle.
                        // If we can't resolve (e.g. invalid handle), good luck,
                        // but we still count it as pending because the Overlapped is out there.
                        if let Some(fd) = res.get_fd() {
                            if let Ok(handle) = submit::resolve_fd(fd, &self.registered_files) {
                                if let Some(entry) = res.entry_mut() {
                                    unsafe {
                                        use windows_sys::Win32::System::IO::CancelIoEx;
                                        let _ =
                                            CancelIoEx(handle, &entry.inner as *const _ as *mut _);
                                    }
                                }
                            }
                        }
                    }
                }
                pending_count += 1;
            }
        }

        // 2. Wait for all pending operations to drain
        // We do this by calling GetQueuedCompletionStatus until we see enough completions.
        // We rely on the fact that CancelIoEx will trigger a completion packet (usually with ERROR_OPERATION_ABORTED).
        // Note: For 'Offload' tasks in thread pool, they might not be cancellable via CancelIoEx,
        // but our Offload implementation posts a completion status when done.
        // We might be waiting a while if a blocking task is stuck.
        // This is a trade-off: safe shutdown vs hanging shutdown. Safe is preferred here.

        let mut ops_drained = 0;

        while ops_drained < pending_count {
            // Failsafe timeout: if we stuck for too long (e.g. 3s), we panic or leak?
            // Leaking is better than UAF. But here we try to wait.
            // We wait indefinitely for completions to avoid Use-After-Free.
            // If the kernel never returns the completion, we hang, which is safer than memory corruption.

            let mut bytes = 0;
            let mut key = 0;
            let mut overlapped = std::ptr::null_mut();

            let res = unsafe {
                GetQueuedCompletionStatus(self.port, &mut bytes, &mut key, &mut overlapped, 100)
            };

            // If we got a packet (overlapped != null), it counts as a completion
            if !overlapped.is_null() {
                ops_drained += 1;
            } else if res == 0 {
                let err = unsafe { GetLastError() };
                if err == WAIT_TIMEOUT {
                    continue;
                }
                // Some other error, but no packet.
            }
        }

        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.port) };
    }
}
