#[macro_export]
macro_rules! for_all_io_ops {
    ($mac:ident) => {
        $mac! (
            ReadFixed(ReadFixed),
            WriteFixed(WriteFixed),
            Send(Send),
            Recv(Recv),
            Timeout(Timeout),
            Accept(Accept),
            Connect(Connect),
            SendTo(SendTo),
            RecvFrom(RecvFrom),
            Wakeup(Wakeup),
            Open(Open),
            Close(Close),
            Fsync(Fsync),
        );
    };
}
