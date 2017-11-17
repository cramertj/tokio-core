//! Support for creating futures that represent timeouts.
//!
//! This module contains the `Timeout` type which is a future that will resolve
//! at a particular point in the future.

use std::io;
use std::time::Instant;

use futures::{Poll, Async};

use reactor;
use reactor::mio::Remote;
use reactor::mio::timeout_token::TimeoutToken;

#[derive(Debug)]
pub struct Timeout {
    token: TimeoutToken,
    when: Instant,
    handle: Remote,
}

impl Timeout {
    /// Creates a new timeout which will fire at the time specified by `at`.
    ///
    /// This function will return a Result with the actual timeout object or an
    /// error. The timeout object itself is then a future which will be
    /// set to fire at the specified point in the future.
    pub fn new_at(at: Instant, handle: &reactor::Handle) -> io::Result<Timeout> {
        Ok(Timeout {
            token: try!(TimeoutToken::new(at, handle.as_sys())),
            when: at,
            handle: handle.remote().clone().into_sys(),
        })
    }

    /// Resets this timeout to an new timeout which will fire at the time
    /// specified by `at`.
    ///
    /// This method is usable even of this instance of `Timeout` has "already
    /// fired". That is, if this future has resolved, calling this method means
    /// that the future will still re-resolve at the specified instant.
    ///
    /// If `at` is in the past then this future will immediately be resolved
    /// (when `poll` is called).
    ///
    /// Note that if any task is currently blocked on this future then that task
    /// will be dropped. It is required to call `poll` again after this method
    /// has been called to ensure that a task is blocked on this future.
    pub fn reset(&mut self, at: Instant) {
        self.when = at;
        self.token.reset_timeout(self.when, &self.handle);
    }

    /// Polls this `Timeout` instance to see if it's elapsed, assuming the
    /// current time is specified by `now`.
    ///
    /// The `Future::poll` implementation for `Timeout` will call `Instant::now`
    /// each time it's invoked, but in some contexts this can be a costly
    /// operation. This method is provided to amortize the cost by avoiding
    /// usage of `Instant::now`, assuming that it's been called elsewhere.
    ///
    /// This function takes the assumed current time as the first parameter and
    /// otherwise functions as this future's `poll` function. This will block a
    /// task if one isn't already blocked or update a previous one if already
    /// blocked.
    pub fn poll_at(&mut self, now: Instant) -> Poll<(), io::Error> {
        if self.when <= now {
            Ok(Async::Ready(()))
        } else {
            self.token.update_timeout(&self.handle);
            Ok(Async::NotReady)
        }
    }
}

impl Drop for Timeout {
    fn drop(&mut self) {
        self.token.cancel_timeout(&self.handle);
    }
}
