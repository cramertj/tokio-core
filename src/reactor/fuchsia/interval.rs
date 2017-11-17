use std::io;
use std::time::{Duration, Instant};

use reactor;
use reactor::interval::next_interval;
use reactor::sys::Timeout;
use futures::{Poll, Async};

pub(crate) struct Interval {
    timeout: Timeout,
    duration: Duration,
}

impl Interval {
    pub fn new_at(at: Instant, dur: Duration, handle: &reactor::Handle)
        -> io::Result<Interval>
    {
        Ok(Interval {
            timeout: Timeout::new_at(at, handle)?,
            duration: dur,
        })
    }

    pub fn poll_at(&mut self, now: Instant) -> Poll<Option<()>, io::Error> {
        Ok(match self.timeout.poll_at(now)? {
            Async::Ready(()) => {
                let next_timeout = next_interval(self.timeout.next(), now, self.duration);
                self.timeout.reset(next_timeout);
                Async::Ready(Some(()))
            },
            Async::NotReady => Async::NotReady,
        })
    }
}
