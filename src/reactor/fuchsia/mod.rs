//! The core reactor driving all I/O
//!
//! This module contains the `Core` type which is the reactor for all I/O
//! happening in `tokio-core`. This reactor (or event loop) is used to run
//! futures, schedule tasks, issue I/O requests, etc.

use std::cell::RefCell;
use std::{fmt, mem, io};
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicUsize, ATOMIC_USIZE_INIT, Ordering};
use std::time::Duration;

use futures::{Future, IntoFuture, Async};
use futures::future::{Executor, ExecuteError};
use futures::executor::{self, Spawn, Notify};
use futures::sync::mpsc;
use slab::Slab;

mod interval;
mod timeout;

pub(crate) use self::timeout::Timeout;
pub(crate) use self::interval::Interval;

use reactor::{self, CoreId, fuchsia};

use zx;

// Key set aside for the main task in `run`.
const MAIN_TASK_KEY: usize = ::std::usize::MAX;

// Key set aside for the `spawn_task_channel`.
const SPAWN_TASK_KEY: usize = ::std::usize::MAX - 1;

static NEXT_LOOP_ID: AtomicUsize = ATOMIC_USIZE_INIT;
scoped_thread_local!(static CURRENT_LOOP: Core);

type UnitFuture = Box<Future<Item = (), Error = ()>>;

trait SpawnFn: Send + 'static {
    fn call_box(self: Box<Self>, handle: &reactor::Handle) -> UnitFuture;
}

impl<F> SpawnFn for F
    where F: (FnOnce(&reactor::Handle) -> UnitFuture) + Send + 'static
{
    fn call_box(self: Box<Self>, handle: &reactor::Handle) -> UnitFuture {
        (*self)(handle)
    }
}

/// A port-based executor for Fuchsia.
// NOTE: intentionally does not implement `Clone`.
pub(crate) struct Core {
    spawn_task_rx: Spawn<mpsc::UnboundedReceiver<Box<SpawnFn>>>,
    handle: Handle,
}

impl Drop for Core {
    fn drop(&mut self) {
        // Remove all tasks in order to prevent a task a `Handle`
        // from causing a reference cycle and keeping the `Slab`
        // from dropping.
        self.handle.tasks.borrow_mut().clear();
    }
}

/// A handle to an event loop backed by a `Port`.
#[derive(Clone)]
pub(crate) struct Handle {
    tasks: Rc<RefCell<Slab<Option<Spawn<UnitFuture>>>>>,
    remote: reactor::Remote,
}

#[derive(Clone)]
pub(crate) struct Remote {
    io_inner: Arc<IoInner>
}

struct IoInner {
    id: usize,
    port: zx::Port,
    packet_receivers: RwLock<Slab<Arc<PacketReceiver>>>,
    spawn_task_tx: mpsc::UnboundedSender<Box<SpawnFn>>,
}

impl Notify for IoInner {
    fn notify(&self, id: usize) {
        let up = zx::UserPacket::from_u8_array([0; 32]);
        let packet = zx::Packet::from_user_packet(id as u64, 0 /* status */, up);
        self.port.queue(&packet).expect("Failed to queue notify in port");
    }
}

// Implemented as a macro than a function to allow the compiler to observe that
// the borrows of `io_inner`, `tasks`, and `spawn_task_rx` are disjoint.
macro_rules! io_inner {
    ($self:ident) => { &$self.handle.remote.sys.io_inner }
}

/// A trait for handling the arrival of a packet on a `zx::Port`.
///
/// This trait should be implemented by users who wish to write their own
/// types which receive asynchronous notifications from a `zx::Port`.
/// Implementors of this trait generally contain an `AtomicTask` which
/// is used to wake up the task which can make progress due to the arrival of
/// the packet.
///
/// `PacketReceiver`s should be registered with a `Core` using the
/// `register_packet_receiver` method on `Core`, `Handle`, or `Remote`.
/// Upon registration, users will receive a `ReceiverRegistration`
/// which provides `key` and `port` methods which can be used to wait on
/// asynchronous signals.
pub trait PacketReceiver: Send + Sync + 'static {
    /// Receive a packet when one arrives.
    fn receive_packet(&self, packet: zx::Packet);
}

/// A registration of a `PacketReceiver`.
/// When dropped, it will automatically deregister the `PacketReceiver`.
// NOTE: purposefully does not implement `Clone`.
#[derive(Debug)]
pub struct ReceiverRegistration<T: PacketReceiver> {
    receiver: Arc<T>,
    remote: reactor::Remote,
    key: usize,
}

impl<T> ReceiverRegistration<T> where T: PacketReceiver {
    /// The key with which `Packet`s destined for this receiver should be sent on the `Port`.
    pub fn key(&self) -> u64 {
        self.key as u64
    }

    /// The internal `PacketReceiver`.
    pub fn receiver(&self) -> &T {
        &*self.receiver
    }

    /// The `Remote` with which the `PacketReceiver` is registered.
    pub fn remote(&self) -> &reactor::Remote {
        &self.remote
    }

    /// The `Port` on which packets destined for this `PacketReceiver` should be queued.
    pub fn port(&self) -> &zx::Port {
        self.remote.port()
    }
}

impl<T> Drop for ReceiverRegistration<T> where T: PacketReceiver {
    fn drop(&mut self) {
        self.remote.sys.deregister_packet_receiver(self.key);
    }
}

impl Core {
    pub fn new() -> io::Result<Core> {
        let (spawn_task_tx, spawn_task_rx) = mpsc::unbounded();
        let spawn_task_rx = executor::spawn(spawn_task_rx);

        let io_inner = Arc::new(IoInner {
            id: NEXT_LOOP_ID.fetch_add(1, Ordering::Relaxed),
            port: zx::Port::create()?,
            packet_receivers: RwLock::new(Slab::new()),
            spawn_task_tx,
        });

        // Start out with spawn_task_rx notified so that it will be polled once
        io_inner.notify(SPAWN_TASK_KEY);

        Ok(Core {
            spawn_task_rx,
            handle: Handle {
                tasks: Rc::new(RefCell::new(Slab::new())),
                remote: reactor::Remote { sys: Remote { io_inner } },
            },
        })
    }

    pub fn port(&self) -> &zx::Port {
        self.handle.port()
    }

    pub fn register_packet_receiver<T>(&self, packet_receiver: Arc<T>) -> ReceiverRegistration<T>
        where T: fuchsia::PacketReceiver
    {
        self.handle.register_packet_receiver(packet_receiver)
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    pub fn remote(&self) -> Remote {
        self.handle.remote.sys.clone()
    }

    pub fn run<F>(&mut self, f: F) -> Result<F::Item, F::Error>
        where F: Future,
    {
        let mut task = executor::spawn(f);
        let mut future_fired = true;

        loop {
            if future_fired {
                let res = CURRENT_LOOP.set(self, || {
                   task.poll_future_notify(io_inner!(self), MAIN_TASK_KEY)
                })?;
                if let Async::Ready(e) = res {
                    return Ok(e);
                }
            }
            future_fired = self.poll(None);
        }
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
        self.poll(max_wait);
    }

    fn poll(&mut self, max_wait: Option<Duration>) -> bool {
        let deadline = match max_wait {
            Some(duration) => {
                let duration = zx::Duration::from_seconds(duration.as_secs()) +
                               zx::Duration::from_nanos(duration.subsec_nanos() as u64);
                zx::Time::after(duration)
            },
            None => zx::Time::INFINITE,
        };
        let packet = match self.port().wait(deadline) {
            Ok(p) => p,
            Err(zx::Status::TIMED_OUT) |
            Err(zx::Status::INTERRUPTED_RETRY) => return false,
            Err(e) => panic!("Error polling tokio_core::reactor::Core's port: {:?}", e),
        };

        // This cast is a no-op because Fuchsia only supports 64-bit platforms.
        let notify_key = packet.key() as usize;

        if notify_key == MAIN_TASK_KEY {
            return true;
        }

        if notify_key == SPAWN_TASK_KEY {
            // Spawn all available tasks
            loop {
                let task_async = self.spawn_task_rx.poll_stream_notify(
                                    io_inner!(self), SPAWN_TASK_KEY).unwrap();

                match task_async {
                    Async::Ready(Some(task_fn)) => {
                        let handle = reactor::Handle { sys: self.handle.clone() };
                        let box_future = task_fn.call_box(&handle);
                        let key = self.handle.tasks.borrow_mut()
                                      .insert(Some(executor::spawn(box_future)));
                        io_inner!(self).notify(key);
                    },
                    Async::NotReady |
                    Async::Ready(None) => return false,
                }
            }
        }

		let (key, key_kind) = notify_key_to_key(notify_key);
		match key_kind {
			KeyKind::Task => {
                self.dispatch_task(key);
			}
            KeyKind::Receiver => {
                let lock = self.handle.remote.sys.io_inner.packet_receivers.read()
                               .expect("tokio_core packet_receivers poisoned");

                if let Some(packet_receiver) = lock.get(key) {
                    packet_receiver.receive_packet(packet);
                }
            }
		}

        false
    }

    fn dispatch_task(&self, key: usize) {
        let mut task = {
            let mut mut_tasks = self.handle.tasks.borrow_mut();
            if let Some(task) = mut_tasks.get_mut(key).and_then(|ref mut t| t.take()) {
                task
            } else {
                return
            }
        };

        let is_finished = match task.poll_future_notify(io_inner!(self), key) {
            Ok(Async::NotReady) => false,
            Ok(Async::Ready(())) | Err(()) => true,
        };

        let mut mut_tasks = self.handle.tasks.borrow_mut();
        if is_finished {
            mut_tasks.remove(key);
        } else {
            mut_tasks[key] = Some(task);
        }
    }

    /// Get the ID of this loop
    pub fn id(&self) -> CoreId {
        self.handle.id()
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum KeyKind {
    Task,
    Receiver,
}

#[inline(always)]
fn usize_high_bit() -> usize {
    1usize << (::std::usize::MAX.count_ones() - 1)
}

fn notify_key_to_key(notify_key: usize) -> (usize, KeyKind) {
    (
        notify_key & !usize_high_bit(),
        match notify_key & usize_high_bit() {
            0 => KeyKind::Task,
            _ => KeyKind::Receiver,
        }
    )
}

fn key_to_notify_key(key: usize, kind: KeyKind) -> usize {
    if key & usize_high_bit() != 0 {
        panic!("ID flowed into usize top bit");
    }
    key | match kind {
        KeyKind::Task => 0,
        KeyKind::Receiver => usize_high_bit(),
    }
}

impl<F> Executor<F> for Core
    where F: Future<Item = (), Error = ()> + 'static,
{
    fn execute(&self, future: F) -> Result<(), ExecuteError<F>> {
        self.handle.spawn(future);
        Ok(())
    }
}

impl Remote {
    // Returns a reference to the underlying Zircon `Port` through which packet
    // notifications are scheduled.
    pub fn port(&self) -> &zx::Port {
        &self.io_inner.port
    }

    pub fn register_packet_receiver<T>(&self, packet_receiver: Arc<T>) -> ReceiverRegistration<T>
        where T: fuchsia::PacketReceiver
    {
        let key = self.io_inner.packet_receivers
                      .write().expect("tokio_core packet recievers were poisoned")
                      .insert(packet_receiver.clone());

        let notify_key = key_to_notify_key(key, KeyKind::Receiver);

        ReceiverRegistration {
            remote: reactor::Remote { sys: self.clone() },
            key: notify_key,
            receiver: packet_receiver,
        }
    }

    fn deregister_packet_receiver(&self, key: usize) {
        let (key, key_kind) = notify_key_to_key(key);
        if let KeyKind::Task = key_kind {
            error!("Attempted to deregister packet receiver with task key");
            return;
        }

        self.io_inner.packet_receivers
            .write().expect("tokio_core packet receivers were poisoned")
            .remove(key);
    }


    pub fn spawn<F, R>(&self, f: F)
        where F: FnOnce(&reactor::Handle) -> R + Send + 'static,
              R: IntoFuture<Item=(), Error=()>,
              R::Future: 'static,
    {
        match self.io_inner.spawn_task_tx.unbounded_send(Box::new(|h: &_| {
            Box::new(f(h).into_future()) as UnitFuture
        })) {
            Ok(()) => {},
            Err(e) => mem::drop(e), // see tokio-core#17-- error should bubble
        }
    }

    /// Return the ID of the represented Core
    pub fn id(&self) -> CoreId {
        CoreId(self.io_inner.id)
    }

    /// Attempts to "promote" this remote to a handle, if possible.
    ///
    /// This function is intended for structures which typically work through a
    /// `Remote` but want to optimize runtime when the remote doesn't actually
    /// leave the thread of the original reactor.
    /// 
    /// However, on Fuchsia, `Handle` and `Remote` have the same representation
    /// so we can just `clone` the original type.
    pub fn handle(&self) -> Option<Handle> {
        if CURRENT_LOOP.is_set() {
            CURRENT_LOOP.with(|lp| {
                let same = lp.id() == self.id();
                if same {
                    Some(lp.handle())
                } else {
                    None
                }
            })
        } else {
            None
        }
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
    // Returns a reference to the underlying Zircon `Port` through which packet
    // notifications are be scheduled.
    pub fn port(&self) -> &zx::Port {
        self.remote.port()
    }

    pub fn register_packet_receiver<T>(&self, packet_receiver: Arc<T>) -> ReceiverRegistration<T>
        where T: fuchsia::PacketReceiver
    {
        self.remote.register_packet_receiver(packet_receiver)
    }

    /// Returns a reference to the underlying remote handle to the event loop.
    pub fn remote(&self) -> &reactor::Remote {
        &self.remote
    }

    /// Spawns a new future on the event loop this handle is associated with.
    pub fn spawn<F>(&self, f: F)
        where F: Future<Item=(), Error=()> + 'static,
    {
        let key = self.tasks.borrow_mut()
                      .insert(Some(executor::spawn(Box::new(f))));
        self.remote.sys.io_inner.notify(key);
    }

    /// Return the ID of the represented Core
    pub fn id(&self) -> CoreId {
        self.remote.id()
    }
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Handle")
         .field("id", &self.id())
         .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn id_token_can_store_kind() {
        for id in 0..1000 {
            for &kind in [KeyKind::Task, KeyKind::Receiver].iter() {
                let notify_id = key_to_notify_key(id, kind);
                let (out_id, out_kind) = notify_key_to_key(notify_id);
                assert_eq!((id, kind), (out_id, out_kind));
            }
        }
	}
}
