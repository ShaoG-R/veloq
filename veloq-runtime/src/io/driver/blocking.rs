use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug)]
pub enum ThreadPoolError {
    Overloaded,
}

struct PoolState {
    queue: Mutex<VecDeque<Job>>,
    cond: Condvar,
    active_workers: AtomicUsize,
    idle_workers: AtomicUsize,
}

pub struct ThreadPool {
    state: Arc<PoolState>,
    core_threads: usize,
    max_threads: usize,
    queue_capacity: usize,
    keep_alive: Duration,
}

struct WorkerGuard {
    state: Arc<PoolState>,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.state.active_workers.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ThreadPool {
    pub fn new(
        core_threads: usize,
        max_threads: usize,
        queue_capacity: usize,
        keep_alive: Duration,
    ) -> Self {
        assert!(core_threads <= max_threads);

        let state = Arc::new(PoolState {
            queue: Mutex::new(VecDeque::with_capacity(queue_capacity)),
            cond: Condvar::new(),
            active_workers: AtomicUsize::new(0),
            idle_workers: AtomicUsize::new(0),
        });

        let pool = Self {
            state,
            core_threads,
            max_threads,
            queue_capacity,
            keep_alive,
        };

        pool
    }

    pub fn execute<F>(&self, f: F) -> Result<(), ThreadPoolError>
    where
        F: FnOnce() + Send + 'static,
    {
        let job = Box::new(f);
        let state = &self.state;

        let mut queue = state.queue.lock().unwrap();

        // 1. Try to wake an idle worker
        if state.idle_workers.load(Ordering::SeqCst) > 0 {
            queue.push_back(job);
            state.cond.notify_one();
            return Ok(());
        }

        // 2. Try to spawn a new worker if under limit
        let active = state.active_workers.load(Ordering::SeqCst);
        if active < self.max_threads {
            // Spawn new
            state.active_workers.fetch_add(1, Ordering::SeqCst);
            let state_clone = state.clone();
            let keep_alive = self.keep_alive;
            let core_threads = self.core_threads;

            // Push job first so new worker picks it up
            queue.push_back(job);

            // Spawn detached
            let _ = thread::Builder::new()
                .name("veloq-blocking-worker".into())
                .spawn(move || Self::worker_loop(state_clone, keep_alive, core_threads));

            return Ok(());
        }

        // 3. Queue if capable
        if queue.len() < self.queue_capacity {
            queue.push_back(job);
            // Notify just in case
            state.cond.notify_one();
            Ok(())
        } else {
            Err(ThreadPoolError::Overloaded)
        }
    }

    fn worker_loop(state: Arc<PoolState>, keep_alive: Duration, core_threads: usize) {
        // Guard decrements active_workers on exit (panic or normal)
        let _guard = WorkerGuard {
            state: state.clone(),
        };

        loop {
            let job = {
                let mut queue = state.queue.lock().unwrap();

                // Wait for job
                while queue.is_empty() {
                    state.idle_workers.fetch_add(1, Ordering::SeqCst);

                    let (guard, result) = state.cond.wait_timeout(queue, keep_alive).unwrap();

                    state.idle_workers.fetch_sub(1, Ordering::SeqCst);
                    queue = guard;

                    if result.timed_out() && queue.is_empty() {
                        // Check if we should terminate
                        if state.active_workers.load(Ordering::SeqCst) > core_threads {
                            return; // Guard will decrement active_count
                        }
                    }
                }
                queue.pop_front()
            };

            if let Some(job) = job {
                // Catch panics to provide resilience
                let _ = catch_unwind(AssertUnwindSafe(|| (job)()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn test_dynamic_scaling() {
        let pool = ThreadPool::new(1, 5, 10, Duration::from_secs(1));
        let (tx, rx) = channel();
        let tx = Arc::new(Mutex::new(tx));

        // Push 5 slow tasks, should scale to 5 workers
        for _ in 0..5 {
            let tx = tx.clone();
            pool.execute(move || {
                thread::sleep(Duration::from_millis(100));
                tx.lock().unwrap().send(1).unwrap();
            })
            .unwrap();
        }

        // Assert we used multiple threads (hard to test deterministically without introspection,
        // but we can ensure all complete).
        for _ in 0..5 {
            rx.recv().unwrap();
        }
    }

    #[test]
    fn test_backpressure() {
        // Max 1 thread, queue size 1
        let pool = ThreadPool::new(1, 1, 1, Duration::from_secs(1));
        let (tx, rx) = channel();

        // 1. Occupy thread and wait for it to start processing
        // This ensures the queue is empty before we try to fill it
        pool.execute(move || {
            tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(500));
        })
        .unwrap();

        // Wait for the task to be picked up
        rx.recv().unwrap();

        // 2. Fill queue
        pool.execute(|| {}).unwrap();

        // 3. Reject
        match pool.execute(|| {}) {
            Err(ThreadPoolError::Overloaded) => assert!(true),
            _ => panic!("Should have been overloaded"),
        }
    }

    #[test]
    fn test_resilience() {
        // Use larger queue to avoid random Overloaded errors if worker is slow to pop
        let pool = ThreadPool::new(1, 1, 10, Duration::from_secs(1));
        let (tx, rx) = channel();

        // Panic task
        pool.execute(|| panic!("Oops")).unwrap();

        // Next task should still work
        pool.execute(move || tx.send(1).unwrap()).unwrap();

        assert_eq!(rx.recv().unwrap(), 1);
    }
}
