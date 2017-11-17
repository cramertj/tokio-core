mod tcp;
pub(crate) use self::tcp::{TcpListener, TcpStream};

mod udp;
pub(crate) use self::udp::UdpSocket;

use futures::Async;
use futures::task::AtomicTask;
use libc;
use zx::{self, AsHandleRef};

use std::io::{self, Read, Write};
use std::mem;
use std::net::{self, SocketAddr};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use reactor::{self, PacketReceiver, ReceiverRegistration};
use is_wouldblock;

const READABLE: usize = libc::EPOLLIN as usize;
const WRITABLE: usize = libc::EPOLLOUT as usize;
const ERROR: usize    = libc::EPOLLERR as usize;
const HUP: usize      = libc::EPOLLHUP as usize;

// Unsafe to use. `receive_packet` must not be called after
// `fdio` is invalidated.
pub(crate) struct EventedFdPacketReceiver {
    fdio: *const syscall::fdio_t,
    signals: AtomicUsize,
    read_task: AtomicTask,
    write_task: AtomicTask,
}

// Needed because of the fdio pointer.
// It is safe to send because the `EventedFdPacketReceiver` must be
// deregistered (and therefore `receive_packet` never called again)
// before `__fdio_release` is called.
unsafe impl Send for EventedFdPacketReceiver {}
unsafe impl Sync for EventedFdPacketReceiver {}

impl PacketReceiver for EventedFdPacketReceiver {
    fn receive_packet(&self, packet: zx::Packet) {
        let observed_signals = if let zx::PacketContents::SignalOne(p) = packet.contents() {
            p.observed()
        } else { return };

        let mut events: u32 = 0;
        unsafe {
            syscall::__fdio_wait_end(self.fdio, observed_signals.bits(), &mut events);
        }
        let events = events as usize;

        let old = self.signals.fetch_or(events, Ordering::SeqCst);
        let became_readable = ((events & READABLE) != 0) && ((old & READABLE) == 0);
        let became_writable = ((events & WRITABLE) != 0) && ((old & WRITABLE) == 0);
        let err_occurred = (events &(ERROR | HUP)) != 0;

        if became_readable || err_occurred {
            self.read_task.notify();
        }
        if became_writable || err_occurred {
            self.write_task.notify();
        }
    }
}

/// A type which can be used for receiving IO events for a file descriptor.
pub(crate) struct EventedFd<T> {
    inner: T,
    // Must be valid, acquired from `__fdio_to_io`
    fdio: *const syscall::fdio_t,
    // Must be dropped before `__fdio_release` is called
    signal_receiver: mem::ManuallyDrop<ReceiverRegistration<EventedFdPacketReceiver>>,
}

unsafe impl<T> Send for EventedFd<T> where T: Send {}
unsafe impl<T> Sync for EventedFd<T> where T: Sync {}

impl<T> Drop for EventedFd<T> {
    fn drop(&mut self) {
        unsafe {
            // Drop the receiver so `packet_receive` may not be called again.
            mem::ManuallyDrop::drop(&mut self.signal_receiver);

            // Release the fdio
            syscall::__fdio_release(self.fdio);
        }

        // Then `inner` gets dropped
    }
}

impl<T> EventedFd<T> where T: AsRawFd {
    /// Creates a new EventedFd.
    ///
    /// For this function to be safe, the underlying file descriptor from `T::as_raw_fd`
    /// must be a valid file descriptor which remains valid for the duration of `T`'s
    /// lifetime.
    pub unsafe fn new(inner: T, remote: &reactor::Remote) -> io::Result<Self> {
        let fdio = syscall::__fdio_fd_to_io(inner.as_raw_fd());
        let signal_receiver = remote.register_packet_receiver(Arc::new(EventedFdPacketReceiver {
            fdio,
            signals: AtomicUsize::new(0),
            read_task: AtomicTask::new(),
            write_task: AtomicTask::new(),
        }));

        let evented_fd = EventedFd {
            inner,
            fdio,
            signal_receiver: mem::ManuallyDrop::new(signal_receiver),
        };

        // Make sure a packet is delivered if an error or closure occurs.
        evented_fd.schedule_packet(ERROR | HUP);

        // Need to schedule packets to maintain the invariant that
        // if !READABLE or !WRITABLE a packet has been scheduled.
        evented_fd.schedule_packet(READABLE);
        evented_fd.schedule_packet(WRITABLE);

        Ok(evented_fd)
    }
    /// Tests to see if this resource is ready to be read from.
    /// If it is not, it arranges for the current task to receive a notification
    /// when a "writable" signal arrives.
    pub fn poll_read(&self) -> Async<()> {
        let receiver = self.signal_receiver.receiver();
        if (receiver.signals.load(Ordering::SeqCst) &
            (READABLE | ERROR | HUP)) != 0
        {
            Async::Ready(())
        } else {
            self.need_read();
            Async::NotReady
        }
    }

    /// Tests to see if this resource is ready to be written to.
    /// If it is not, it arranges for the current task to receive a notification
    /// when a "writable" signal arrives.
    pub fn poll_write(&self) -> Async<()> {
        let receiver = self.signal_receiver.receiver();
        if (receiver.signals.load(Ordering::SeqCst) &
            (WRITABLE | ERROR | HUP)) != 0
        {
            Async::Ready(())
        } else {
            self.need_write();
            Async::NotReady
        }
    }

    // Returns a reference to the underlying IO object.
    pub fn get_ref(&self) -> &T {
        &self.inner
    }

    // Returns a mutable reference to the underlying IO object.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Arranges for the current task to receive a notification when a "readable"
    /// signal arrives.
    pub fn need_read(&self) {
        let receiver = self.signal_receiver.receiver();
        receiver.read_task.register();
        let old = receiver.signals.fetch_and(!READABLE, Ordering::SeqCst);
        // We only need to schedule a new packet if one isn't already scheduled.
        // If READABLE was already false, a packet was already scheduled.
        if (old & READABLE) != 0 {
            self.schedule_packet(READABLE);
        }
    }

    /// Arranges for the current task to receive a notification when a "writable"
    /// signal arrives.
    pub fn need_write(&self) {
        let receiver = self.signal_receiver.receiver();
        receiver.write_task.register();
        let old = receiver.signals.fetch_and(!WRITABLE, Ordering::SeqCst);
        // We only need to schedule a new packet if one isn't already scheduled.
        // If WRITABLE was already false, a packet was already scheduled.
        if (old & WRITABLE) != 0 {
            self.schedule_packet(WRITABLE);
        }
    }

    fn schedule_packet(&self, signals: usize) {
        unsafe {
            let (mut raw_handle, mut raw_signals) = mem::uninitialized();
            syscall::__fdio_wait_begin(
                self.fdio,
                signals as u32,
                &mut raw_handle,
                &mut raw_signals,
            );

            let handle = zx::Handle::from_raw(raw_handle);
            let signals = zx::Signals::from_bits_truncate(raw_signals);

            let res = handle.wait_async_handle(
                self.signal_receiver.remote().port(),
                self.signal_receiver.key(),
                signals,
                zx::WaitAsyncOpts::Once,
            );

            // The handle is borrowed, so we cannot drop it.
            mem::forget(handle);
            res.expect("Error scheduling EventedFd notification");
        }
    }

    /// Clears all incoming signals.
    pub fn clear(&self) {
        self.signal_receiver.receiver().signals.store(0, Ordering::SeqCst);
    }

    /// Get a reference to the event loop with which this `EventedFd` was created.
    pub fn remote(&self) -> &reactor::Remote {
        self.signal_receiver.remote()
    }
}

impl<T: AsRawFd> AsRawFd for EventedFd<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.get_ref().as_raw_fd()
    }
}

impl<T: Read + AsRawFd> Read for EventedFd<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Async::NotReady = self.poll_read() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let r = self.get_mut().read(buf);
        if is_wouldblock(&r) {
            self.need_read();
        }
        r
    }
}

impl<T: Write + AsRawFd> Write for EventedFd<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Async::NotReady = self.poll_write() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let r = self.get_mut().write(buf);
        if is_wouldblock(&r) {
            self.need_write();
        }
        r
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Async::NotReady = self.poll_write() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let r = self.get_mut().flush();
        if is_wouldblock(&r) {
            self.need_write();
        }
        r
    }
}

impl<'a, T: AsRawFd> Read for &'a EventedFd<T>
    where &'a T: Read
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Async::NotReady = self.poll_read() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let r = self.get_ref().read(buf);
        if is_wouldblock(&r) {
            self.need_read();
        }
        r
    }
}

impl<'a, T: AsRawFd> Write for &'a EventedFd<T>
    where &'a T: Write
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Async::NotReady = self.poll_write() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let r = self.get_ref().write(buf);
        if is_wouldblock(&r) {
            self.need_write();
        }
        r
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Async::NotReady = self.poll_write() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let r = self.get_ref().flush();
        if is_wouldblock(&r) {
            self.need_write();
        }
        r
    }
}

mod syscall {
    #![allow(non_camel_case_types, improper_ctypes)]
    use std::os::unix::io::RawFd;
    pub use zx::sys::{zx_handle_t, zx_signals_t};

    // This is the "improper" c type
    pub type fdio_t = ();

    #[link(name = "fdio")]
    extern {
        pub fn __fdio_fd_to_io(fd: RawFd) -> *const fdio_t;
        pub fn __fdio_release(io: *const fdio_t);

        pub fn __fdio_wait_begin(
            io: *const fdio_t,
            events: u32,
            handle_out: &mut zx_handle_t,
            signals_out: &mut zx_signals_t,
        );

        pub fn __fdio_wait_end(
            io: *const fdio_t,
            signals: zx_signals_t,
            events_out: &mut u32,
        );
    }
}

// Set non-blocking (workaround since the std version doesn't work in fuchsia)
// TODO: fix the std version and replace this
pub fn set_nonblock(fd: RawFd) -> io::Result<()> {
    let res = unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
    if res == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Workaround until fuchsia's recv_from is fixed
unsafe fn recv_from(fd: RawFd, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
    let flags = 0;

    let n = 
        libc::recv(fd,
                   buf.as_mut_ptr() as *mut libc::c_void,
                   buf.len(),
                   flags);

    if n == -1 {
        return Err(io::Error::last_os_error());
    }

    // random address-- we don't use it
    let addr = SocketAddr::new(net::IpAddr::V4(net::Ipv4Addr::new(127, 0, 0, 1)), 8080);
    Ok((n as usize, addr))
}
