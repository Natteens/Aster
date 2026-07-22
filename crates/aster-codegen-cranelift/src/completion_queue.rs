//! Host-side completion signalling for awaited inner tasks.
//!
//! An async task suspended at its `await` is woken not by joining the inner
//! `Task.Run` (which would block a host thread on a worker), but by a worker
//! pushing that task's opaque completion token here. The host pump blocks on
//! [`CompletionQueue::pop`] until a token arrives, then joins the now-ready
//! inner task without ever waiting on a worker.
//!
//! Only jobs that back an `await` carry a token (see `worker_pool::Job`);
//! ordinary `Task.Run(...).Wait()` and `Parallel` chunks never touch this
//! queue, so no orphan token is ever produced for them.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

/// Opaque per-await completion token, unique within one [`super::task_runtime::TaskRuntime`].
pub(super) type CompletionToken = u64;

struct QueueState {
    tokens: VecDeque<CompletionToken>,
    closed: bool,
}

/// A `std`-only blocking queue of completion tokens: a mutex-guarded deque and
/// a condition variable, with an explicit closed flag for shutdown. Shared
/// across threads only as `Arc<CompletionQueue>`; workers get a clone solely
/// to push, never any access to the `TaskRuntime` itself.
pub(super) struct CompletionQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl CompletionQueue {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                tokens: VecDeque::new(),
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }

    /// Signal `token`'s completion and wake the pump. Never lost: the token is
    /// enqueued under the lock before the notify, so a pump about to block
    /// still observes it. Safe to call after `close`; the token is simply
    /// dropped once no pump can consume it.
    pub(super) fn push(&self, token: CompletionToken) {
        let mut state = self
            .state
            .lock()
            .expect("completion queue mutex is healthy");
        if state.closed {
            return;
        }
        state.tokens.push_back(token);
        drop(state);
        self.ready.notify_one();
    }

    /// Block until a token is available or the queue is closed. Tolerant of
    /// spurious wakeups (re-checks the deque under the lock). Returns `None`
    /// only once the queue is closed and drained.
    pub(super) fn pop(&self) -> Option<CompletionToken> {
        let mut state = self
            .state
            .lock()
            .expect("completion queue mutex is healthy");
        loop {
            if let Some(token) = state.tokens.pop_front() {
                return Some(token);
            }
            if state.closed {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .expect("completion queue mutex is healthy");
        }
    }

    /// Stop the queue and wake every blocked pump so it can observe the
    /// closure instead of blocking forever.
    pub(super) fn close(&self) {
        let mut state = self
            .state
            .lock()
            .expect("completion queue mutex is healthy");
        state.closed = true;
        drop(state);
        self.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn push_then_pop_returns_the_token() {
        let queue = CompletionQueue::new();
        queue.push(7);
        assert_eq!(queue.pop(), Some(7));
    }

    #[test]
    fn pop_blocks_until_a_push_from_another_thread() {
        let queue = Arc::new(CompletionQueue::new());
        let worker = Arc::clone(&queue);
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            worker.push(42);
        });
        // No busy-wait here: this blocks until the worker pushes.
        assert_eq!(queue.pop(), Some(42));
        handle.join().expect("worker thread joins");
    }

    #[test]
    fn close_unblocks_a_waiting_pop_with_none() {
        let queue = Arc::new(CompletionQueue::new());
        let closer = Arc::clone(&queue);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            closer.close();
        });
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn tokens_are_delivered_in_fifo_order_without_loss() {
        let queue = CompletionQueue::new();
        for token in 0..100 {
            queue.push(token);
        }
        for token in 0..100 {
            assert_eq!(queue.pop(), Some(token));
        }
    }
}
