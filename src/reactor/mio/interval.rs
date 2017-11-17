use std::io;
use std::time::{Duration, Instant};

use futures::{Poll, Async};

use reactor;
use reactor::mio::Remote;
use reactor::mio::timeout_token::TimeoutToken;
use reactor::interval::next_interval;

pub struct Interval {
    token: TimeoutToken,
    next: Instant,
    interval: Duration,
    handle: Remote,
}

impl Interval {
    pub fn new_at(at: Instant, dur: Duration, handle: &reactor::Handle)
        -> io::Result<Interval>
    {
        Ok(Interval {
            token: try!(TimeoutToken::new(at, handle.as_sys())),
            next: at,
            interval: dur,
            handle: handle.remote().clone().into_sys(),
        })
    }

    pub fn poll_at(&mut self, now: Instant) -> Poll<Option<()>, io::Error> {
        if self.next <= now {
            self.next = next_interval(self.next, now, self.interval);
            self.token.reset_timeout(self.next, &self.handle);
            Ok(Async::Ready(Some(())))
        } else {
            self.token.update_timeout(&self.handle);
            Ok(Async::NotReady)
        }
    }
}

impl Drop for Interval {
    fn drop(&mut self) {
        self.token.cancel_timeout(&self.handle);
    }
}

#[cfg(test)]
mod test {
    use std::time::{Instant, Duration};
    use super::next_interval;

    struct Timeline(Instant);

    impl Timeline {
        fn new() -> Timeline {
            Timeline(Instant::now())
        }
        fn at(&self, millis: u64) -> Instant {
            self.0 + Duration::from_millis(millis)
        }
        fn at_ns(&self, sec: u64, nanos: u32) -> Instant {
            self.0 + Duration::new(sec, nanos)
        }
    }

    fn dur(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    // The math around Instant/Duration isn't 100% precise due to rounding
    // errors, see #249 for more info
    fn almost_eq(a: Instant, b: Instant) -> bool {
        if a == b {
            true
        } else if a > b {
            a - b < Duration::from_millis(1)
        } else {
            b - a < Duration::from_millis(1)
        }
    }

    #[test]
    fn norm_next() {
        let tm = Timeline::new();
        assert!(almost_eq(next_interval(tm.at(1), tm.at(2), dur(10)),
                                        tm.at(11)));
        assert!(almost_eq(next_interval(tm.at(7777), tm.at(7788), dur(100)),
                                        tm.at(7877)));
        assert!(almost_eq(next_interval(tm.at(1), tm.at(1000), dur(2100)),
                                        tm.at(2101)));
    }

    #[test]
    fn fast_forward() {
        let tm = Timeline::new();
        assert!(almost_eq(next_interval(tm.at(1), tm.at(1000), dur(10)),
                                        tm.at(1001)));
        assert!(almost_eq(next_interval(tm.at(7777), tm.at(8888), dur(100)),
                                        tm.at(8977)));
        assert!(almost_eq(next_interval(tm.at(1), tm.at(10000), dur(2100)),
                                        tm.at(10501)));
    }

    /// TODO: this test actually should be successful, but since we can't
    ///       multiply Duration on anything larger than u32 easily we decided
    ///       to allow it to fail for now
    #[test]
    #[should_panic(expected = "can't skip more than 4 billion intervals")]
    fn large_skip() {
        let tm = Timeline::new();
        assert_eq!(next_interval(
            tm.at_ns(0, 1), tm.at_ns(25, 0), Duration::new(0, 2)),
            tm.at_ns(25, 1));
    }

}
