use std::cell::RefCell;
use std::cell::UnsafeCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::task::{Context, Poll, Waker};

/// A handle to a spawned task.
pub struct LocalJoinHandle<T> {
    state: Rc<RefCell<LocalJoinState<T>>>,
}

struct LocalJoinState<T> {
    value: Option<T>,
    waker: Option<Waker>,
}

impl<T> LocalJoinHandle<T> {
    pub(crate) fn new() -> (Self, LocalJoinProducer<T>) {
        let state = Rc::new(RefCell::new(LocalJoinState {
            value: None,
            waker: None,
        }));
        (
            Self {
                state: state.clone(),
            },
            LocalJoinProducer { state },
        )
    }
}

impl<T> Future for LocalJoinHandle<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.state.borrow_mut();
        if let Some(val) = state.value.take() {
            Poll::Ready(val)
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

pub(crate) struct LocalJoinProducer<T> {
    state: Rc<RefCell<LocalJoinState<T>>>,
}

impl<T> LocalJoinProducer<T> {
    pub(crate) fn set(self, value: T) {
        let mut state = self.state.borrow_mut();
        state.value = Some(value);
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }
}

/// A handle to a spawned task (Send).
pub struct JoinHandle<T> {
    state: Arc<JoinState<T>>,
}

// States for JoinState
const IDLE: u8 = 0;
const WAITING: u8 = 1;
const READY: u8 = 2;

struct JoinState<T> {
    state: AtomicU8,
    value: UnsafeCell<Option<T>>,
    waker: UnsafeCell<Option<Waker>>,
}

unsafe impl<T: Send> Sync for JoinState<T> {}
unsafe impl<T: Send> Send for JoinState<T> {}

impl<T> JoinHandle<T> {
    pub(crate) fn new() -> (Self, JoinProducer<T>) {
        let state = Arc::new(JoinState {
            state: AtomicU8::new(IDLE),
            value: UnsafeCell::new(None),
            waker: UnsafeCell::new(None),
        });
        (
            Self {
                state: state.clone(),
            },
            JoinProducer { state },
        )
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let state = &self.state;

        loop {
            let current = state.state.load(Ordering::Acquire);

            if current == READY {
                // SAFETY: State is READY, producer has finished writing value.
                // We are the only consumer.
                let val = unsafe { (*state.value.get()).take() };
                if let Some(v) = val {
                    return Poll::Ready(v);
                } else {
                    // Task already consumed or empty. Mimics original behavior of hanging/pending.
                    return Poll::Pending;
                }
            }

            // If we are currently WAITING, try to reset to IDLE to update the waker.
            // This is necessary if poll is called multiple times with different wakers.
            if current == WAITING {
                if state
                    .state
                    .compare_exchange(WAITING, IDLE, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
                {
                    // State changed (likely to READY), retry loop
                    continue;
                }
            }

            // Now state is effectively IDLE (either we saw IDLE or we successfully transitioned WAITING -> IDLE).
            // It is safe to update the waker because Producer only reads waker if it sees WAITING.
            // Since we are single-consumer, no other polling thread is writing.

            unsafe {
                *state.waker.get() = Some(cx.waker().clone());
            }

            // Publish our waiting state.
            // We use compare_exchange because Producer might have transitioned IDLE -> READY while we were working.
            if state
                .state
                .compare_exchange(IDLE, WAITING, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                return Poll::Pending;
            }

            // If CAS failed, state must have changed to READY. Loop will handle it.
        }
    }
}

pub(crate) struct JoinProducer<T> {
    state: Arc<JoinState<T>>,
}

impl<T> JoinProducer<T> {
    pub(crate) fn set(self, value: T) {
        let state = &self.state;

        // Write value first
        unsafe {
            *state.value.get() = Some(value);
        }

        // Publish READY state
        let old_state = state.state.swap(READY, Ordering::AcqRel);

        if old_state == WAITING {
            // Consumer was waiting, wake them up.
            // SAFETY: We observed WAITING, so Consumer has finished writing waker.
            // We transitioned to READY, so Consumer will not touch waker again until they see READY.
            let waker = unsafe { (*state.waker.get()).take() };
            if let Some(w) = waker {
                w.wake();
            }
        }
    }
}
