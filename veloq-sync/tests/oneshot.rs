#![cfg(not(feature = "loom"))]
use veloq_sync::oneshot;

#[tokio::test]
async fn test_oneshot_send_recv() {
    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        tx.send(42).unwrap();
    });

    let val = rx.await.unwrap();
    assert_eq!(val, 42);
}

#[tokio::test]
async fn test_oneshot_drop_sender() {
    let (tx, rx) = oneshot::channel::<i32>();

    tokio::spawn(async move {
        drop(tx);
    });

    let err = rx.await.unwrap_err();
    assert!(matches!(err, oneshot::error::RecvError(_)));
}

#[tokio::test]
async fn test_oneshot_try_recv() {
    let (tx, mut rx) = oneshot::channel();

    assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty));

    tx.send(100).unwrap();

    assert_eq!(rx.try_recv(), Ok(100));
    assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Closed));
}

#[tokio::test]
async fn test_oneshot_drop_receiver_notify() {
    let (tx, rx) = oneshot::channel::<i32>();

    let t = tokio::spawn(async move {
        drop(rx);
    });

    // waiting for rx drop
    t.await.unwrap();

    assert!(tx.is_closed());
    // sending should fail
    assert_eq!(tx.send(1), Err(1));
}

#[tokio::test]
async fn test_oneshot_poll_closed() {
    let (mut tx, rx) = oneshot::channel::<()>();

    let handle = tokio::spawn(async move {
        drop(rx);
    });

    tx.closed().await;
    handle.await.unwrap();
    assert!(tx.is_closed());
}
