use crate::io::buffer::FixedBuf;
use crate::io::driver::PlatformDriver;
use crate::io::op::{IoFd, Op, RawHandle, RecvFrom, SendTo};
use crate::io::socket::Socket;
use std::cell::RefCell;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::rc::Weak;

pub struct UdpSocket {
    fd: RawHandle,
    driver: Weak<RefCell<PlatformDriver>>,
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = unsafe { Socket::from_raw(self.fd as i32) };
        #[cfg(windows)]
        let _ = unsafe { Socket::from_raw(self.fd as *mut std::ffi::c_void) };
    }
}

impl UdpSocket {
    pub fn bind<A: ToSocketAddrs>(
        addr: A,
        driver: Weak<RefCell<PlatformDriver>>,
    ) -> io::Result<Self> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "No address provided"))?;

        let socket = if addr.is_ipv4() {
            Socket::new_udp_v4()?
        } else {
            Socket::new_udp_v6()?
        };

        socket.bind(addr)?;

        Ok(Self {
            fd: socket.into_raw() as RawHandle,
            driver,
        })
    }

    pub async fn send_to(
        &self,
        buf: FixedBuf,
        target: SocketAddr,
    ) -> (io::Result<usize>, FixedBuf) {
        let (raw_addr, raw_addr_len) = crate::io::socket::socket_addr_trans(target);
        let op = SendTo {
            fd: IoFd::Raw(self.fd),
            buf,
            addr: raw_addr.into_boxed_slice(),
            addr_len: raw_addr_len as u32,
        };
        let future = Op::new(op, self.driver.clone());
        let (res, op_back): (io::Result<usize>, SendTo) = future.await;
        (res, op_back.buf)
    }

    pub async fn recv_from(&self, buf: FixedBuf) -> (io::Result<(usize, SocketAddr)>, FixedBuf) {
        let addr_buf_size = 128usize;
        let addr = vec![0u8; addr_buf_size].into_boxed_slice();
        let addr_len = addr_buf_size as u32;

        let op = RecvFrom {
            fd: IoFd::Raw(self.fd),
            buf,
            addr,
            addr_len,
        };
        let future = Op::new(op, self.driver.clone());
        let (res, op_back): (io::Result<usize>, RecvFrom) = future.await;

        match res {
            Ok(n) => {
                let len = op_back.addr_len as usize;
                let addr = crate::io::socket::to_socket_addr(&op_back.addr[..len])
                    .unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());
                (Ok((n, addr)), op_back.buf)
            }
            Err(e) => (Err(e), op_back.buf),
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        use std::mem::ManuallyDrop;

        #[cfg(unix)]
        let socket = unsafe { ManuallyDrop::new(Socket::from_raw(self.fd as i32)) };
        #[cfg(windows)]
        let socket =
            unsafe { ManuallyDrop::new(Socket::from_raw(self.fd as *mut std::ffi::c_void)) };
        socket.local_addr()
    }
}
