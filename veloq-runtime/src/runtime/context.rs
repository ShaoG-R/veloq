//! Explicit context for the async runtime.
//!
//! This module provides the `RuntimeContext` which is passed to tasks
//! allowing them to spawn new tasks and access runtime resources.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::rc::{Rc, Weak};

use crate::io::driver::PlatformDriver;
use crate::runtime::executor::Spawner;
use crate::runtime::join::{JoinHandle, LocalJoinHandle};
use crate::runtime::task::Task;

use crate::io::buffer::BufPool;

// No global thread-local storage anymore.

/// Context passed to runtime tasks.
///
/// This provides access to the executor's facilities like spawning tasks
/// and accessing the IO driver.
#[derive(Clone)]
pub struct RuntimeContext<P: BufPool> {
    pub(crate) driver: Weak<RefCell<PlatformDriver>>,
    pub(crate) queue: Weak<RefCell<VecDeque<Rc<Task>>>>,
    pub(crate) buffer_pool: Weak<P>,
    pub(crate) spawner: Option<Spawner>,
}

impl<P: BufPool> RuntimeContext<P> {
    /// Create a new RuntimeContext.
    pub(crate) fn new(
        driver: Weak<RefCell<PlatformDriver>>,
        queue: Weak<RefCell<VecDeque<Rc<Task>>>>,
        buffer_pool: Weak<P>,
        spawner: Option<Spawner>,
    ) -> Self {
        Self {
            driver,
            queue,
            buffer_pool,
            spawner,
        }
    }

    /// Spawn a new task on the current executor.
    ///
    /// # Panics
    /// Panics if the current executor does not have a global spawner (e.g. some local-only configurations).
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let spawner = self
            .spawner
            .as_ref()
            .expect("spawn() called on a context without a global spawner");

        spawner.spawn(future)
    }

    /// Spawn a new local task on the current executor.
    ///
    /// Local tasks are not Send and are guaranteed to run on the current thread.
    pub fn spawn_local<F, T>(&self, future: F) -> LocalJoinHandle<T>
    where
        F: Future<Output = T> + 'static,
        T: 'static,
    {
        let queue = self.queue.upgrade().expect("executor has been dropped");

        let (handle, producer) = LocalJoinHandle::new();
        let task = Task::new(
            async move {
                let output = future.await;
                producer.set(output);
            },
            self.queue.clone(),
        );
        queue.borrow_mut().push_back(task);
        handle
    }

    /// Get a weak reference to the current driver.
    pub fn driver(&self) -> Weak<RefCell<PlatformDriver>> {
        self.driver.clone()
    }

    /// Get a weak reference to the current buffer pool.
    pub fn buffer_pool(&self) -> Weak<P> {
        self.buffer_pool.clone()
    }
}

/// Yields execution back to the executor, allowing other tasks to run.
///
/// This is useful when you want to give other spawned tasks a chance to execute.
pub fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}

/// Future returned by `yield_now()`.
pub struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if self.yielded {
            std::task::Poll::Ready(())
        } else {
            self.yielded = true;
            // Wake ourselves so we get polled again
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}
