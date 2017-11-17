use std::io;
use std::net::{self, SocketAddr};
use std::ops::Deref;

use reactor::Handle;
use net::fuchsia::{recv_from, set_nonblock, EventedFd};
use std::os::unix::io::AsRawFd;

/// An I/O object representing a UDP socket.
pub(crate) struct UdpSocket(EventedFd<net::UdpSocket>);

impl Deref for UdpSocket {
    type Target = EventedFd<net::UdpSocket>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl UdpSocket {
    pub fn bind(addr: &SocketAddr, handle: &Handle) -> io::Result<UdpSocket> {
        let socket = net::UdpSocket::bind(addr)?;
        UdpSocket::from_socket(socket, handle)
    }

    pub fn from_socket(socket: net::UdpSocket, handle: &Handle) -> io::Result<UdpSocket> {
        set_nonblock(socket.as_raw_fd())?;

        unsafe {
            Ok(UdpSocket(EventedFd::new(socket, handle.remote())?))
       }
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        unsafe { recv_from(self.0.as_raw_fd(), buf) }
    }
}
