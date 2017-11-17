use std::io;
use std::net::{self, SocketAddr};
use std::fmt;
use std::ops::Deref;

use mio;

use reactor::{Handle, PollEvented};

/// An I/O object representing a UDP socket.
pub struct UdpSocket(PollEvented<mio::net::UdpSocket>);

impl Deref for UdpSocket {
    type Target = PollEvented<mio::net::UdpSocket>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl UdpSocket {
    pub fn bind(addr: &SocketAddr, handle: &Handle) -> io::Result<UdpSocket> {
        let udp = try!(mio::net::UdpSocket::bind(addr));
        UdpSocket::new(udp, handle)
    }

    fn new(socket: mio::net::UdpSocket, handle: &Handle) -> io::Result<UdpSocket> {
        let io = try!(PollEvented::new(socket, handle));
        Ok(UdpSocket(io))
    }

    pub fn from_socket(socket: net::UdpSocket,
                       handle: &Handle) -> io::Result<UdpSocket> {
        let udp = try!(mio::net::UdpSocket::from_socket(socket));
        UdpSocket::new(udp, handle)
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.get_ref().recv_from(buf)
    }
}

impl fmt::Debug for UdpSocket {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.get_ref().fmt(f)
    }
}
