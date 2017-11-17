//! Support for creating futures that represent timeouts.
//!
//! This module contains the `Timeout` type which is a future that will resolve
//! at a particular point in the future.

use std::io;
use std::time::{Duration, Instant};
use std::sync::Arc;

use futures::{Poll, Async};
use futures::task::AtomicTask;

use reactor::{self, ReceiverRegistration, PacketReceiver};

use zx;
use zx::prelude::*;

#[derive(Debug)]
pub(crate) struct TimeoutReceiver(AtomicTask);

impl PacketReceiver for TimeoutReceiver {
    fn receive_packet(&self, packet: zx::Packet) {
        if let zx::PacketContents::SignalRep(signals) = packet.contents() {
            if signals.observed().contains(zx::Signals::TIMER_SIGNALED) {
                self.0.notify();
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Timeout {
    timer: zx::Timer,
    instant: Instant,
    timeout_receiver: ReceiverRegistration<TimeoutReceiver>,
}

impl Timeout {
    pub fn new_at(instant: Instant, handle: &reactor::Handle) -> io::Result<Timeout> {
        let zx_dur = duration_until_instant(instant);

        let timeout_receiver = handle.register_packet_receiver(
            Arc::new(TimeoutReceiver(AtomicTask::new()))
        );

        let timer = zx::Timer::create(zx::ClockId::Monotonic)?;

        timer.wait_async_handle(
            handle.port(),
            timeout_receiver.key(),
            zx::Signals::TIMER_SIGNALED,
            zx::WaitAsyncOpts::Repeating
        )?;

        timer.set(zx_dur.after_now(), 0.nanos())?;

        Ok(Timeout {
            timer,
            instant,
            timeout_receiver,
        })
    }

    pub fn next(&self) -> Instant {
        self.instant
    }

    pub fn reset(&mut self, at: Instant) {
        self.instant = at;
        if let Err(e) = self.timer.set(duration_until_instant(at).after_now(), 0.nanos()) {
            error!("tokio_core error setting timer: {:?}", e);
        }
    }

    pub fn poll_at(&mut self, now: Instant) -> Poll<(), io::Error> {
        if self.instant <= now {
            Ok(Async::Ready(()))
        } else {
            self.timeout_receiver.receiver().0.register();
            Ok(Async::NotReady)
        }
    }
}

fn duration_until_instant(at: Instant) -> zx::Duration {
    let start = Instant::now();
    let dur = if at < start {
        Duration::new(0, 0)
    } else {
        at - start
    };

    zx::Duration::from(dur)
}
