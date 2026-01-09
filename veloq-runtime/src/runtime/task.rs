use std::alloc::{self, Layout};
use std::cell::{RefCell, UnsafeCell};
use std::collections::VecDeque;
use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::ptr::NonNull;
use std::rc::Weak;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use crate::io::driver::RemoteWaker;
use crate::runtime::executor::ExecutorShared;

/// A handle to a task.
///
/// This struct wraps a pointer to a heap-allocated task. The task's layout is:
/// [ Header ] [ Future ]
///
/// This allows for a single allocation for both the task metadata and the future itself.
pub(crate) struct Task {
    ptr: NonNull<Header>,
}

unsafe impl Send for Task {}
unsafe impl Sync for Task {}

struct Header {
    owner_id: usize,
    // We strictly access this only if `is_current_worker(owner_id)` is true.
    local_queue: Weak<RefCell<VecDeque<Task>>>,
    shared: Arc<ExecutorShared>,
    vtable: &'static TaskVTable,
    remotes: AtomicUsize, // Reference count
}

struct TaskVTable {
    poll: unsafe fn(NonNull<Header>),
    drop_future: unsafe fn(NonNull<Header>),
    dealloc: unsafe fn(NonNull<Header>),
}

impl Task {
    pub(crate) fn new<F>(
        future: F,
        owner_id: usize,
        local_queue: Weak<RefCell<VecDeque<Task>>>,
        shared: Arc<ExecutorShared>,
    ) -> Self
    where
        F: Future<Output = ()> + 'static,
    {
        let vtable = &TaskVTable {
            poll: poll_future::<F>,
            drop_future: drop_future::<F>,
            dealloc: dealloc_task::<F>,
        };

        // Although new is safe (assuming args are safe), we are calling unsafe alloc_task
        // But alloc_task is unsafe fn, so we need unsafe block.
        // Actually alloc_task is unsafe mainly because of raw pointer handling internally?
        // Let's keep it unsafe.
        unsafe {
            let ptr = alloc_task(
                future,
                Header {
                    owner_id,
                    local_queue,
                    shared,
                    vtable,
                    remotes: AtomicUsize::new(1),
                },
            );
            Task { ptr }
        }
    }

    pub(crate) fn from_boxed(
        future: Pin<Box<dyn Future<Output = ()>>>,
        owner_id: usize,
        local_queue: Weak<RefCell<VecDeque<Task>>>,
        shared: Arc<ExecutorShared>,
    ) -> Self {
        // For boxed futures, we just treat the Pin<Box<...>> as the future type F.
        Self::new(future, owner_id, local_queue, shared)
    }

    pub(crate) fn run(self) {
        // Consume the task handle.
        // The VTable poll function handles logic.
        let ptr = self.ptr;
        mem::forget(self); // Don't drop refcount yet

        unsafe {
            (ptr.as_ref().vtable.poll)(ptr);
        }
    }

    fn wake_task(task: Task) {
        let header = unsafe { task.ptr.as_ref() };
        if crate::runtime::context::is_current_worker(header.owner_id) {
            if let Some(queue) = header.local_queue.upgrade() {
                queue.borrow_mut().push_back(task);
            }
        } else {
            // Send to remote queue
            let _ = header.shared.remote_queue.send(task.clone());
            let _ = header.shared.waker.wake();
        }
    }
}

impl Clone for Task {
    fn clone(&self) -> Self {
        unsafe {
            self.ptr.as_ref().remotes.fetch_add(1, Ordering::Relaxed);
        }
        Self { ptr: self.ptr }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        unsafe {
            let header = self.ptr.as_ref();
            if header.remotes.fetch_sub(1, Ordering::Release) == 1 {
                std::sync::atomic::fence(Ordering::Acquire);
                (header.vtable.drop_future)(self.ptr);
                (header.vtable.dealloc)(self.ptr);
            }
        }
    }
}

// --- Layout & Helper Functions ---

#[repr(C)]
struct TaskRaw<F> {
    header: Header,
    future: UnsafeCell<Option<F>>,
}

unsafe fn alloc_task<F>(future: F, header: Header) -> NonNull<Header> {
    let layout = Layout::new::<TaskRaw<F>>();
    unsafe {
        let ptr = alloc::alloc(layout) as *mut TaskRaw<F>;
        if ptr.is_null() {
            alloc::handle_alloc_error(layout);
        }

        ptr.write(TaskRaw {
            header,
            future: UnsafeCell::new(Some(future)),
        });

        NonNull::new_unchecked(ptr as *mut Header)
    }
}

unsafe fn dealloc_task<F>(ptr: NonNull<Header>) {
    unsafe {
        let ptr = ptr.cast::<TaskRaw<F>>().as_ptr(); // cast back
        let layout = Layout::new::<TaskRaw<F>>();
        alloc::dealloc(ptr as *mut u8, layout);
    }
}

unsafe fn drop_future<F>(ptr: NonNull<Header>) {
    unsafe {
        let raw = ptr.cast::<TaskRaw<F>>().as_ref();
        let slot = &mut *raw.future.get();
        *slot = None;
    }
}

unsafe fn poll_future<F: Future<Output = ()>>(ptr: NonNull<Header>) {
    // We already "consumed" one ref count coming into run() via forget(self).
    // Now we must construct a Waker and poll.

    // We need to keep this Task alive during poll, so we make a Waker from it.
    // The Waker will hold a new ref count.

    let slot = unsafe {
        let raw = ptr.cast::<TaskRaw<F>>().as_ref();
        &mut *raw.future.get()
    };

    if let Some(future) = slot {
        // Create waker
        let waker = unsafe { waker_from_ptr(ptr) };
        let mut cx = Context::from_waker(&waker);

        // Safety: F is inside TaskRaw which is stable heap memory.
        let type_erased_future = unsafe { Pin::new_unchecked(future) };

        match type_erased_future.poll(&mut cx) {
            Poll::Ready(_) => {
                *slot = None;
            }
            Poll::Pending => {
                // Pending, waker holds ref.
            }
        }
    }

    // Decrement the ref count we stole from `run(self)`.
    unsafe {
        let header = ptr.as_ref();
        if header.remotes.fetch_sub(1, Ordering::Release) == 1 {
            std::sync::atomic::fence(Ordering::Acquire);
            (header.vtable.drop_future)(ptr);
            (header.vtable.dealloc)(ptr);
        }
    }
}

// --- Waker Implementation ---

const WAKER_VTABLE: RawWakerVTable =
    RawWakerVTable::new(unsafe_clone, unsafe_wake, unsafe_wake_by_ref, unsafe_drop);

unsafe fn waker_from_ptr(ptr: NonNull<Header>) -> Waker {
    unsafe {
        // Increment ref because Waker owns a reference
        ptr.as_ref().remotes.fetch_add(1, Ordering::Relaxed);
        let raw = RawWaker::new(ptr.as_ptr() as *const (), &WAKER_VTABLE);
        Waker::from_raw(raw)
    }
}

unsafe fn unsafe_clone(ptr: *const ()) -> RawWaker {
    unsafe {
        let header = &*(ptr as *const Header);
        header.remotes.fetch_add(1, Ordering::Relaxed);
        RawWaker::new(ptr, &WAKER_VTABLE)
    }
}

unsafe fn unsafe_wake(ptr: *const ()) {
    unsafe {
        let non_null = NonNull::new_unchecked(ptr as *mut Header);
        let task = Task { ptr: non_null };
        Task::wake_task(task);
    }
}

unsafe fn unsafe_wake_by_ref(ptr: *const ()) {
    unsafe {
        let header = &*(ptr as *const Header);
        header.remotes.fetch_add(1, Ordering::Relaxed);
        let task = Task {
            ptr: NonNull::new_unchecked(ptr as *mut Header),
        };
        Task::wake_task(task);
    }
}

unsafe fn unsafe_drop(ptr: *const ()) {
    unsafe {
        let header = &*(ptr as *const Header);
        if header.remotes.fetch_sub(1, Ordering::Release) == 1 {
            std::sync::atomic::fence(Ordering::Acquire);
            (header.vtable.drop_future)(NonNull::new_unchecked(ptr as *mut Header));
            (header.vtable.dealloc)(NonNull::new_unchecked(ptr as *mut Header));
        }
    }
}
