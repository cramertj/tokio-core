//! The core reactor driving all I/O
//!
//! This module contains the `Core` type which is the reactor for all I/O
//! happening in `tokio-core`. This reactor (or event loop) is used to run
//! futures, schedule tasks, issue I/O requests, etc.

use std::fmt;
use std::io;
use std::time::Duration;

use futures::{Future, IntoFuture};
use futures::future::{self, Executor, ExecuteError};

#[cfg(not(target_os = "fuchsia"))]
mod mio;
#[cfg(not(target_os = "fuchsia"))]
use self::mio as sys;

#[cfg(target_os = "fuchsia")]
mod fuchsia;
#[cfg(target_os = "fuchsia")]
use self::fuchsia as sys;
#[cfg(target_os = "fuchsia")]
use std::sync::Arc;

mod timeout;
mod interval;

#[cfg(not(target_os = "fuchsia"))]
pub use self::mio::PollEvented;

#[cfg(target_os = "fuchsia")]
pub use self::fuchsia::{PacketReceiver, ReceiverRegistration};

pub use self::timeout::Timeout;
pub use self::interval::Interval;

/// An event loop.
///
/// The event loop is the main source of blocking in an application which drives
/// all other I/O events and notifications happening. Each event loop can have
/// multiple handles pointing to it, each of which can then be used to create
/// various I/O objects to interact with the event loop in interesting ways.
// TODO: expand this
pub struct Core {
    sys: sys::Core,
}

/// An unique ID for a Core
///
/// An ID by which different cores may be distinguished. Can be compared and used as an index in
/// a `HashMap`.
///
/// The ID is globally unique and never reused.
#[derive(Clone,Copy,Eq,PartialEq,Hash,Debug)]
pub struct CoreId(usize);

/// Handle to an event loop, used to construct I/O objects, send messages, and
/// otherwise interact indirectly with the event loop itself.
///
/// Handles can be cloned, and when cloned they will still refer to the
/// same underlying event loop.
#[derive(Clone)]
pub struct Remote {
    sys: sys::Remote,
}

/// A non-sendable handle to an event loop, useful for manufacturing instances
/// of `LoopData`.
#[derive(Clone)]
pub struct Handle {
    sys: sys::Handle
}

impl Core {
    /// Creates a new event loop, returning any error that happened during the
    /// creation.
    pub fn new() -> io::Result<Core> {
        Ok(Core { sys: sys::Core::new()? })
    }

    /// Returns a handle to this event loop which cannot be sent across threads
    /// but can be used as a proxy to the event loop itself.
    ///
    /// Handles are cloneable and clones always refer to the same event loop.
    /// This handle is typically passed into functions that create I/O objects
    /// to bind them to this event loop.
    pub fn handle(&self) -> Handle {
        Handle { sys: self.sys.handle() }
    }

    /// Returns a reference to the underlying Zircon `Port` through which packet
    /// notifications are scheduled.
    ///
    /// This method is only available on Fuchsia OS.
    #[cfg(target_os = "fuchsia")]
    pub fn port(&self) -> &::zx::Port {
        self.sys.port()
    }

    /// Registers the provided `PacketReceiver` with the reactor.
    /// Returns a `ReceiverRegistration` containing the registered `PacketReceiver` as
    /// well as a the key on which the `PacketReceiver` is registered.
    /// 
    /// After this function is called, users should use the `key` method on `ReceiverRegistration`
    /// as the key for async waits on the `zx::Port` obtained by calling the `port` function.
    /// Incoming packets will be delivered to the registered `PacketReceiver` with the matching key
    #[cfg(target_os = "fuchsia")]
    pub fn register_packet_receiver<T>(&self, packet_receiver: Arc<T>) -> ReceiverRegistration<T>
        where T: PacketReceiver
    {
        self.sys.register_packet_receiver(packet_receiver)
    }

    /// Generates a remote handle to this event loop which can be used to spawn
    /// tasks from other threads into this event loop.
    pub fn remote(&self) -> Remote {
        Remote { sys: self.sys.remote() }
    }

    /// Runs a future until completion, driving the event loop while we're
    /// otherwise waiting for the future to complete.
    ///
    /// This function will begin executing the event loop and will finish once
    /// the provided future is resolved. Note that the future argument here
    /// crucially does not require the `'static` nor `Send` bounds. As a result
    /// the future will be "pinned" to not only this thread but also this stack
    /// frame.
    ///
    /// This function will return the value that the future resolves to once
    /// the future has finished. If the future never resolves then this function
    /// will never return.
    ///
    /// # Panics
    ///
    /// This method will **not** catch panics from polling the future `f`. If
    /// the future panics then it's the responsibility of the caller to catch
    /// that panic and handle it as appropriate.
    pub fn run<F>(&mut self, f: F) -> Result<F::Item, F::Error>
        where F: Future,
    {
        self.sys.run(f)
    }

    /// Performs one iteration of the event loop, blocking on waiting for events
    /// for at most `max_wait` (forever if `None`).
    ///
    /// It only makes sense to call this method if you've previously spawned
    /// a future onto this event loop.
    ///
    /// `loop { lp.turn(None) }` is equivalent to calling `run` with an
    /// empty future (one that never finishes).
    pub fn turn(&mut self, max_wait: Option<Duration>) {
        self.sys.turn(max_wait)
    }

    /// Get the ID of this loop
    pub fn id(&self) -> CoreId {
        self.sys.id()
    }
}

impl<F> Executor<F> for Core
    where F: Future<Item = (), Error = ()> + 'static,
{
    fn execute(&self, future: F) -> Result<(), ExecuteError<F>> {
        self.handle().execute(future)
    }
}

impl fmt::Debug for Core {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Core")
         .field("id", &self.id())
         .finish()
    }
}

impl Remote {
    /// Returns a reference to the underlying Zircon `Port` through which packet
    /// notifications are scheduled.
    ///
    /// This method is only available on Fuchsia OS.
    #[cfg(target_os = "fuchsia")]
    pub fn port(&self) -> &::zx::Port {
        self.sys.port()
    }

    /// Registers the provided `PacketReceiver` with the reactor.
    /// Returns a `ReceiverRegistration` containing the registered `PacketReceiver` as
    /// well as a the key on which the `PacketReceiver` is registered.
    /// 
    /// After this function is called, users should use the `key` method on `ReceiverRegistration`
    /// as the key for async waits on the `zx::Port` obtained by calling the `port` function.
    /// Incoming packets will be delivered to the registered `PacketReceiver` with the matching key.
    #[cfg(target_os = "fuchsia")]
    pub fn register_packet_receiver<T>(&self, packet_receiver: Arc<T>) -> ReceiverRegistration<T>
        where T: PacketReceiver
    {
        self.sys.register_packet_receiver(packet_receiver)
    }

    /// Spawns a new future into the event loop this remote is associated with.
    ///
    /// This function takes a closure which is executed within the context of
    /// the I/O loop itself. The future returned by the closure will be
    /// scheduled on the event loop and run to completion.
    ///
    /// Note that while the closure, `F`, requires the `Send` bound as it might
    /// cross threads, the future `R` does not.
    ///
    /// # Panics
    ///
    /// This method will **not** catch panics from polling the future `f`. If
    /// the future panics then it's the responsibility of the caller to catch
    /// that panic and handle it as appropriate.
    pub fn spawn<F, R>(&self, f: F)
        where F: FnOnce(&Handle) -> R + Send + 'static,
              R: IntoFuture<Item=(), Error=()>,
              R::Future: 'static,
    {
        self.sys.spawn(f)
    }

    /// Return the ID of the represented Core
    pub fn id(&self) -> CoreId {
        self.sys.id()
    }

    /// Attempts to "promote" this remote to a handle, if possible.
    ///
    /// This function is intended for structures which typically work through a
    /// `Remote` but want to optimize runtime when the remote doesn't actually
    /// leave the thread of the original reactor. This will attempt to return a
    /// handle if the `Remote` is on the same thread as the event loop and the
    /// event loop is running.
    ///
    /// If this `Remote` has moved to a different thread or if the event loop is
    /// running, then `None` may be returned. If you need to guarantee access to
    /// a `Handle`, then you can call this function and fall back to using
    /// `spawn` above if it returns `None`.
    pub fn handle(&self) -> Option<Handle> {
        self.sys.handle().map(|h| Handle { sys: h })
    }

    #[cfg(not(target_os = "fuchsia"))]
    fn into_sys(self) -> sys::Remote {
        self.sys
    }

    #[cfg(not(target_os = "fuchsia"))]
    fn as_sys(&self) -> &sys::Remote {
        &self.sys
    }
}

impl<F> Executor<F> for Remote
    where F: Future<Item = (), Error = ()> + Send + 'static,
{
    fn execute(&self, future: F) -> Result<(), ExecuteError<F>> {
        self.spawn(|_| future);
        Ok(())
    }
}

impl fmt::Debug for Remote {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Remote")
         .field("id", &self.id())
         .finish()
    }
}

impl Handle {
    /// Returns a reference to the underlying Zircon `Port` through which packet
    /// notifications are scheduled.
    ///
    /// This method is only available on Fuchsia OS.
    #[cfg(target_os = "fuchsia")]
    pub fn port(&self) -> &::zx::Port {
        self.sys.port()
    }

    /// Registers the provided `PacketReceiver` with the reactor.
    /// Returns a `ReceiverRegistration` containing the registered `PacketReceiver` as
    /// well as a the key on which the `PacketReceiver` is registered.
    /// 
    /// After this function is called, users should use the `key` method on `ReceiverRegistration`
    /// as the key for async waits on the `zx::Port` obtained by calling the `port` function.
    /// Incoming packets will be delivered to the registered `PacketReceiver` with the matching key
    #[cfg(target_os = "fuchsia")]
    pub fn register_packet_receiver<T>(&self, packet_receiver: Arc<T>) -> ReceiverRegistration<T>
        where T: PacketReceiver
    {
        self.sys.register_packet_receiver(packet_receiver)
    } 

    /// Returns a reference to the underlying remote handle to the event loop.
    pub fn remote(&self) -> &Remote {
        self.sys.remote()
    }

    /// Spawns a new future on the event loop this handle is associated with.
    ///
    /// # Panics
    ///
    /// This method will **not** catch panics from polling the future `f`. If
    /// the future panics then it's the responsibility of the caller to catch
    /// that panic and handle it as appropriate.
    pub fn spawn<F>(&self, f: F)
        where F: Future<Item=(), Error=()> + 'static,
    {
        self.sys.spawn(f)
    }

    /// Spawns a closure on this event loop.
    ///
    /// This function is a convenience wrapper around the `spawn` function above
    /// for running a closure wrapped in `futures::lazy`. It will spawn the
    /// function `f` provided onto the event loop, and continue to run the
    /// future returned by `f` on the event loop as well.
    ///
    /// # Panics
    ///
    /// This method will **not** catch panics from polling the future `f`. If
    /// the future panics then it's the responsibility of the caller to catch
    /// that panic and handle it as appropriate.
    pub fn spawn_fn<F, R>(&self, f: F)
        where F: FnOnce() -> R + 'static,
              R: IntoFuture<Item=(), Error=()> + 'static,
    {
        self.spawn(future::lazy(f))
    }

    /// Return the ID of the represented Core
    pub fn id(&self) -> CoreId {
        self.sys.id()
    }

    /// Borrows this handle as its system-specific representation.
    #[cfg(not(target_os = "fuchsia"))]
    pub(crate) fn as_sys(&self) -> &sys::Handle {
        &self.sys
    }
}

impl<F> Executor<F> for Handle
    where F: Future<Item = (), Error = ()> + 'static,
{
    fn execute(&self, future: F) -> Result<(), ExecuteError<F>> {
        self.spawn(future);
        Ok(())
    }
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Handle")
         .field("id", &self.id())
         .finish()
    }
}
