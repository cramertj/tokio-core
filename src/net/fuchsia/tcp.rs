use std::io::{self, Read, Write};
use std::net::{self, SocketAddr};
use std::ops::Deref;
use bytes::{Buf, BufMut};
use futures::{Poll, Async};

use std::os::unix::io::AsRawFd;

use libc;
use net2::{TcpBuilder, TcpStreamExt};

use net::fuchsia::{EventedFd, set_nonblock};

use reactor::{Handle, Remote};

/// An I/O object representing a TCP socket listening for incoming connections.
///
/// This object can be converted into a stream of incoming connections for
/// various forms of processing.
pub(crate) struct TcpListener(EventedFd<net::TcpListener>);

impl Deref for TcpListener {
    type Target = EventedFd<net::TcpListener>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TcpListener {
    pub fn bind(addr: &SocketAddr, handle: &Handle) -> io::Result<TcpListener> {
        let sock = match *addr {
            SocketAddr::V4(..) => TcpBuilder::new_v4(),
            SocketAddr::V6(..) => TcpBuilder::new_v6(),
        }?;

        sock.reuse_address(true)?;
        sock.bind(addr)?;
        let listener = sock.listen(1024)?;
        TcpListener::new(listener, handle)
    }

    pub fn new(listener: net::TcpListener, handle: &Handle) -> io::Result<TcpListener> {
        set_nonblock(listener.as_raw_fd())?;

        unsafe {
            Ok(TcpListener(EventedFd::new(listener, handle.remote())?))
       }
    }

    pub fn accept(&mut self) -> io::Result<(TcpStream, SocketAddr)> {
        if let Async::NotReady = self.0.poll_read() {
            return Err(io::ErrorKind::WouldBlock.into());
        }

        match self.0.get_ref().accept() {
            Err(e) => {
                if e.kind() == io::ErrorKind::WouldBlock {
                    self.0.need_read();
                }
                return Err(e)
            },
            Ok((sock, addr)) => {
                return TcpStream::from_stream_remote(sock, self.remote())
                    .map(|sock| (sock, addr))
            }
        }
    }

    pub fn from_listener(listener: net::TcpListener,
                         _addr: &SocketAddr,
                         handle: &Handle) -> io::Result<TcpListener> {
        TcpListener::new(listener, handle)
    }
}

pub(crate) struct TcpStream(EventedFd<net::TcpStream>);

impl Deref for TcpStream {
    type Target = EventedFd<net::TcpStream>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TcpStream {
    pub fn connect(addr: &SocketAddr, handle: &Handle) -> io::Result<TcpStream> {
        let sock = match *addr {
            SocketAddr::V4(..) => TcpBuilder::new_v4(),
            SocketAddr::V6(..) => TcpBuilder::new_v6(),
        }?;

        TcpStream::connect_stream(sock.to_tcp_stream()?, addr, handle)
    }

    pub fn from_stream_remote(stream: net::TcpStream, remote: &Remote)
        -> io::Result<TcpStream>
    {
        set_nonblock(stream.as_raw_fd())?;

        let stream = unsafe { EventedFd::new(stream, remote)? } ;

        Ok(TcpStream(stream))
    }

    pub fn from_stream(stream: net::TcpStream, handle: &Handle)
        -> io::Result<TcpStream>
    {
        TcpStream::from_stream_remote(stream, handle.remote())
    }

    pub fn connect_stream(stream: net::TcpStream,
                          addr: &SocketAddr,
                          handle: &Handle) -> io::Result<TcpStream> {
        set_nonblock(stream.as_raw_fd())?;

        let connected = stream.connect(addr);
        match connected {
            Ok(..) => {}
            Err(ref e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
            Err(e) => return Err(e),
        }

        let stream = unsafe { EventedFd::new(stream, handle.remote())? } ;

        Ok(TcpStream(stream))
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.0).read(buf)
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        (&self.0).write(buf)
    }

    pub fn read_buf<B: BufMut>(&self, buf: &mut B) -> Poll<usize, io::Error> {
        match (&self.0).read(unsafe { buf.bytes_mut() }) {
            Ok(n) => {
                unsafe { buf.advance_mut(n); }
                Ok(Async::Ready(n))
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.0.need_read();
                Ok(Async::NotReady)
            }
            Err(e) => Err(e)
        }
    }

    pub fn write_buf<B: Buf>(&self, buf: &mut B) -> Poll<usize, io::Error> {
        match (&self.0).write(buf.bytes()) {
            Ok(n) => {
                buf.advance(n);
                Ok(Async::Ready(n))
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.0.need_write();
                Ok(Async::NotReady)
            }
            Err(e) => Err(e),
        }
    }
}
