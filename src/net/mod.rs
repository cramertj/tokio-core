//! TCP/UDP bindings for `tokio-core`
//!
//! This module contains the TCP/UDP networking types, similar to the standard
//! library, which can be used to implement networking protocols.

//#![allow(warnings)]

mod tcp;
mod udp;

pub use self::tcp::{TcpStream, TcpStreamNew};
pub use self::tcp::{TcpListener, Incoming};
pub use self::udp::{UdpSocket, UdpCodec, UdpFramed, SendDgram, RecvDgram};

#[cfg(not(target_os = "fuchsia"))]
mod mio;
#[cfg(not(target_os = "fuchsia"))]
use self::mio as sys;

#[cfg(target_os = "fuchsia")]
mod fuchsia;
#[cfg(target_os = "fuchsia")]
use self::fuchsia as sys;
