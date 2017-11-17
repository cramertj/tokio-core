use std::io;
use std::net::{self, SocketAddr};
use std::ops::{Deref, DerefMut};

use bytes::{Buf, BufMut};
use futures::sync::oneshot;
use futures::{Future, Poll, Async};
use iovec::IoVec;
use mio;

use reactor::{Handle, PollEvented};

pub struct TcpListener {
    io: PollEvented<mio::net::TcpListener>,
    pending_accept: Option<oneshot::Receiver<io::Result<(TcpStream, SocketAddr)>>>,
}

impl Deref for TcpListener {
    type Target = PollEvented<mio::net::TcpListener>;

    fn deref(&self) -> &Self::Target {
        &self.io
    }
}

impl TcpListener {
    pub fn bind(addr: &SocketAddr, handle: &Handle) -> io::Result<TcpListener> {
        let l = try!(mio::net::TcpListener::bind(addr));
        TcpListener::new(l, handle)
    }

    fn new(listener: mio::net::TcpListener, handle: &Handle)
           -> io::Result<TcpListener> {
        Ok(TcpListener {
            io: PollEvented::new(listener, handle)?,
            pending_accept: None,
        })
    } 

    pub fn from_listener(listener: net::TcpListener, addr: &SocketAddr, handle: &Handle) ->
        io::Result<TcpListener>
    {
        TcpListener::new(mio::net::TcpListener::from_listener(listener, addr)?, handle)
    }

    pub fn accept(&mut self) -> io::Result<(TcpStream, SocketAddr)> {
        loop {
            if let Some(mut pending) = self.pending_accept.take() {
                match pending.poll().expect("shouldn't be canceled") {
                    Async::NotReady => {
                        self.pending_accept = Some(pending);
                        return Err(io::ErrorKind::WouldBlock.into())
                    },
                    Async::Ready(r) => return r,
                }
            }

            if let Async::NotReady = self.io.poll_read() {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "not ready"))
            }

            match self.io.get_ref().accept() {
                Err(e) => {
                    if e.kind() == io::ErrorKind::WouldBlock {
                        self.io.need_read();
                    }
                    return Err(e)
                },
                Ok((sock, addr)) => {
                    // Fast path if we haven't left the event loop
                    if let Some(handle) = self.io.remote().handle() {
                        return PollEvented::new(sock, &handle)
                           .map(|sock| (TcpStream(sock), addr));
                    }

                    // If we're off the event loop then send the socket back
                    // over there to get registered and then we'll get it back
                    // eventually.
                    let (tx, rx) = oneshot::channel();
                    let remote = self.io.remote().clone();
                    remote.spawn(move |handle| {
                        let res = PollEvented::new(sock, handle)
                            .map(move |io| {
                                (TcpStream(io), addr)
                            });
                        drop(tx.send(res));
                        Ok(())
                    });
                    self.pending_accept = Some(rx);
                    // continue to polling the `rx` at the beginning of the loop
                }
            }
        }
    }
}

pub struct TcpStream(PollEvented<mio::net::TcpStream>);

impl Deref for TcpStream {
    type Target = PollEvented<mio::net::TcpStream>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for TcpStream {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl TcpStream {
    pub fn connect(addr: &SocketAddr, handle: &Handle) -> io::Result<TcpStream> {
        Ok(TcpStream(PollEvented::new(mio::net::TcpStream::connect(addr)?, handle)?))
    }

    pub fn connect_stream(stream: net::TcpStream, addr: &SocketAddr, handle: &Handle)
        -> Result<TcpStream, io::Error>
    {
        Ok(TcpStream(PollEvented::new(mio::net::TcpStream::connect_stream(stream, addr)?, handle)?))
    }

    pub fn from_stream(stream: net::TcpStream, handle: &Handle) -> io::Result<TcpStream> {
        Ok(TcpStream(PollEvented::new(mio::net::TcpStream::from_stream(stream)?, handle)?))
    }

    pub fn read_buf<B: BufMut>(&self, buf: &mut B) -> Poll<usize, io::Error> {
        if let Async::NotReady = self.poll_read() {
            return Ok(Async::NotReady)
        }
        let r = unsafe {
            // The `IoVec` type can't have a 0-length size, so we create a bunch
            // of dummy versions on the stack with 1 length which we'll quickly
            // overwrite.
            let b1: &mut [u8] = &mut [0];
            let b2: &mut [u8] = &mut [0];
            let b3: &mut [u8] = &mut [0];
            let b4: &mut [u8] = &mut [0];
            let b5: &mut [u8] = &mut [0];
            let b6: &mut [u8] = &mut [0];
            let b7: &mut [u8] = &mut [0];
            let b8: &mut [u8] = &mut [0];
            let b9: &mut [u8] = &mut [0];
            let b10: &mut [u8] = &mut [0];
            let b11: &mut [u8] = &mut [0];
            let b12: &mut [u8] = &mut [0];
            let b13: &mut [u8] = &mut [0];
            let b14: &mut [u8] = &mut [0];
            let b15: &mut [u8] = &mut [0];
            let b16: &mut [u8] = &mut [0];
            let mut bufs: [&mut IoVec; 16] = [
                b1.into(), b2.into(), b3.into(), b4.into(),
                b5.into(), b6.into(), b7.into(), b8.into(),
                b9.into(), b10.into(), b11.into(), b12.into(),
                b13.into(), b14.into(), b15.into(), b16.into(),
            ];
            let n = buf.bytes_vec_mut(&mut bufs);
            self.get_ref().read_bufs(&mut bufs[..n])
        };

        match r {
            Ok(n) => {
                unsafe { buf.advance_mut(n); }
                Ok(Async::Ready(n))
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.need_read();
                Ok(Async::NotReady)
            }
            Err(e) => Err(e),
        }
    }

    pub fn write_buf<B: Buf>(&self, buf: &mut B) -> Poll<usize, io::Error> {
        if let Async::NotReady = self.poll_write() {
            return Ok(Async::NotReady)
        }
        let r = {
            // The `IoVec` type can't have a zero-length size, so create a dummy
            // version from a 1-length slice which we'll overwrite with the
            // `bytes_vec` method.
            static DUMMY: &[u8] = &[0];
            let iovec = <&IoVec>::from(DUMMY);
            let mut bufs = [
                iovec, iovec, iovec, iovec,
                iovec, iovec, iovec, iovec,
                iovec, iovec, iovec, iovec,
                iovec, iovec, iovec, iovec,
            ];
            let n = buf.bytes_vec(&mut bufs);
            self.0.get_ref().write_bufs(&bufs[..n])
        };
        match r {
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

