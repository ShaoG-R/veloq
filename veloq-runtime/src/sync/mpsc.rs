use atomic_waker::AtomicWaker;
use crossbeam_queue::SegQueue;
use futures_core::stream::Stream;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SendError, TryRecvError};
use std::task::{Context, Poll};

/// A multi-producer, single-consumer channel for sending values across threads
/// to a local executor task.
///
/// This implementation uses `crossbeam_queue::SegQueue` for efficient lock-free queuing
/// and `atomic_waker` for async notification.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        queue: SegQueue::new(),
        waker: AtomicWaker::new(),
        sender_count: AtomicUsize::new(1),
        receiver_active: AtomicBool::new(true),
    });

    (
        Sender {
            shared: shared.clone(),
        },
        Receiver { shared },
    )
}

struct Shared<T> {
    queue: SegQueue<T>,
    waker: AtomicWaker,
    /// Number of active senders. Used to determine when to wake the receiver
    /// upon the last sender disconnecting.
    sender_count: AtomicUsize,
    /// Indicates if the receiver is still active.
    receiver_active: AtomicBool,
}

pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.shared.sender_count.fetch_add(1, Ordering::Relaxed);
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        // Decrement count. If we were the last sender (count went from 1 to 0),
        // wake the receiver so it can see the closed state.
        if self.shared.sender_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shared.waker.wake();
        }
    }
}

impl<T> Sender<T> {
    /// Sends a value to the channel.
    ///
    /// Returns an error if the receiver has been dropped.
    pub fn send(&self, val: T) -> Result<(), SendError<T>> {
        // Check if receiver is active
        if !self.shared.receiver_active.load(Ordering::Relaxed) {
            return Err(SendError(val));
        }

        self.shared.queue.push(val);
        self.shared.waker.wake();
        Ok(())
    }
}

pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.shared.receiver_active.store(false, Ordering::Release);
    }
}

impl<T> Receiver<T> {
    /// Async receive method.
    ///
    /// Returns `None` if the channel is closed (all senders dropped).
    pub async fn recv(&mut self) -> Option<T> {
        RecvFuture { receiver: self }.await
    }

    /// Try to receive a value without waiting.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        if let Some(msg) = self.shared.queue.pop() {
            Ok(msg)
        } else if self.shared.sender_count.load(Ordering::Acquire) == 0 {
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }
}

struct RecvFuture<'a, T> {
    receiver: &'a mut Receiver<T>,
}

impl<'a, T> Future for RecvFuture<'a, T> {
    type Output = Option<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut *self.receiver).poll_next(cx)
    }
}

impl<T> Stream for Receiver<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Register waker using AtomicWaker.
        self.shared.waker.register(cx.waker());

        // Check the queue
        if let Some(val) = self.shared.queue.pop() {
            return Poll::Ready(Some(val));
        }

        // If queue is empty, check if all senders are disconnected
        if self.shared.sender_count.load(Ordering::Acquire) == 0 {
            return Poll::Ready(None);
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_simple_send_recv() {
        let (tx, mut rx) = channel();
        tx.send(1).unwrap();
        tx.send(2).unwrap();

        assert_eq!(rx.try_recv(), Ok(1));
        assert_eq!(rx.try_recv(), Ok(2));
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn test_threaded_send() {
        let (tx, mut rx) = channel();
        let tx = Arc::new(tx);

        let mut handles = vec![];
        for i in 0..10 {
            let tx = tx.clone();
            handles.push(thread::spawn(move || {
                for j in 0..100 {
                    tx.send(i * 100 + j).unwrap();
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // Wait for all messages
        let mut count = 0;
        // Drain the channel
        while let Ok(_) = rx.try_recv() {
            count += 1;
        }
        assert_eq!(count, 1000);
    }
}
