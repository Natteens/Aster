//! Experimental shared hard budget for retained arena page capacity.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Point-in-time accounting for one experimental shared memory governor.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryGovernorTelemetry {
    pub hard_limit_bytes: u64,
    pub current_capacity_bytes: u64,
    pub peak_capacity_bytes: u64,
    pub grant_events: u64,
    pub denial_events: u64,
    pub release_events: u64,
    pub granted_bytes_cumulative: u64,
    pub released_bytes_cumulative: u64,
}

/// Experimental shared hard budget for current `PagedArena` page capacity.
#[doc(hidden)]
pub struct MemoryGovernor {
    hard_limit_bytes: u64,
    current_capacity_bytes: AtomicU64,
    peak_capacity_bytes: AtomicU64,
    grant_events: AtomicU64,
    denial_events: AtomicU64,
    release_events: AtomicU64,
    granted_bytes_cumulative: AtomicU64,
    released_bytes_cumulative: AtomicU64,
}

impl MemoryGovernor {
    /// # Panics
    ///
    /// Panics only on a target where `usize` is wider than 64 bits and the
    /// requested limit cannot be represented by governor telemetry.
    #[must_use]
    pub fn new(hard_limit_bytes: usize) -> Self {
        Self {
            hard_limit_bytes: u64::try_from(hard_limit_bytes)
                .expect("memory governor limits fit in u64"),
            current_capacity_bytes: AtomicU64::new(0),
            peak_capacity_bytes: AtomicU64::new(0),
            grant_events: AtomicU64::new(0),
            denial_events: AtomicU64::new(0),
            release_events: AtomicU64::new(0),
            granted_bytes_cumulative: AtomicU64::new(0),
            released_bytes_cumulative: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub fn telemetry(&self) -> MemoryGovernorTelemetry {
        MemoryGovernorTelemetry {
            hard_limit_bytes: self.hard_limit_bytes,
            current_capacity_bytes: self.current_capacity_bytes.load(Ordering::Relaxed),
            peak_capacity_bytes: self.peak_capacity_bytes.load(Ordering::Relaxed),
            grant_events: self.grant_events.load(Ordering::Relaxed),
            denial_events: self.denial_events.load(Ordering::Relaxed),
            release_events: self.release_events.load(Ordering::Relaxed),
            granted_bytes_cumulative: self.granted_bytes_cumulative.load(Ordering::Relaxed),
            released_bytes_cumulative: self.released_bytes_cumulative.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn hard_limit_bytes(&self) -> u64 {
        self.hard_limit_bytes
    }

    pub(crate) fn try_acquire(self: &Arc<Self>, bytes: usize) -> Option<GovernorReservation> {
        assert!(bytes > 0, "zero-byte governor reservation");
        let Ok(bytes) = u64::try_from(bytes) else {
            self.denial_events.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let mut current = self.current_capacity_bytes.load(Ordering::Relaxed);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                self.denial_events.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            if next > self.hard_limit_bytes {
                self.denial_events.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            // These atomics protect scalar accounting only. Arena ownership
            // publishes page memory, so no cross-thread memory synchronization
            // depends on the governor counters and Relaxed ordering is sufficient.
            match self.current_capacity_bytes.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.peak_capacity_bytes.fetch_max(next, Ordering::Relaxed);
                    self.grant_events.fetch_add(1, Ordering::Relaxed);
                    self.granted_bytes_cumulative
                        .fetch_add(bytes, Ordering::Relaxed);
                    return Some(GovernorReservation {
                        governor: Arc::clone(self),
                        bytes,
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    fn release(&self, bytes: u64) {
        self.current_capacity_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_sub(bytes)
            })
            .expect("memory governor capacity underflow");
        self.release_events.fetch_add(1, Ordering::Relaxed);
        self.released_bytes_cumulative
            .fetch_add(bytes, Ordering::Relaxed);
    }
}

/// One successful grant. Dropping it returns the exact granted capacity.
#[must_use]
pub(crate) struct GovernorReservation {
    governor: Arc<MemoryGovernor>,
    bytes: u64,
}

impl Drop for GovernorReservation {
    fn drop(&mut self) {
        self.governor.release(self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    const PAGE: usize = 4 * 1024;

    #[test]
    fn admission_is_exact_at_page_boundaries() {
        let below = Arc::new(MemoryGovernor::new(PAGE - 1));
        assert!(below.try_acquire(PAGE).is_none());
        assert_eq!(below.telemetry().current_capacity_bytes, 0);

        let exact = Arc::new(MemoryGovernor::new(PAGE));
        let reservation = exact.try_acquire(PAGE).expect("exact limit is admitted");
        assert!(exact.try_acquire(PAGE).is_none());
        assert_eq!(exact.telemetry().current_capacity_bytes, PAGE as u64);
        drop(reservation);

        let above = Arc::new(MemoryGovernor::new(PAGE + PAGE));
        let first = above.try_acquire(PAGE).expect("first page is admitted");
        let second = above.try_acquire(PAGE).expect("second page is admitted");
        assert_eq!(above.telemetry().current_capacity_bytes, (PAGE * 2) as u64);
        drop((first, second));
        assert_eq!(above.telemetry().current_capacity_bytes, 0);
    }

    #[test]
    fn admission_overflow_is_denied_without_changing_current_capacity() {
        let governor = Arc::new(MemoryGovernor::new(usize::MAX));
        let reservation = governor
            .try_acquire(usize::MAX)
            .expect("the exact maximum fits");
        let before_denial = governor.telemetry();
        assert!(governor.try_acquire(1).is_none());
        let after_denial = governor.telemetry();
        assert_eq!(
            after_denial.current_capacity_bytes,
            before_denial.current_capacity_bytes
        );
        assert_eq!(after_denial.denial_events, 1);
        drop(reservation);
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    }

    #[test]
    fn concurrent_reservations_never_overshoot_and_fully_release() {
        const THREADS: usize = 32;
        const ATTEMPTS: usize = 200;
        const CAPACITY_PAGES: usize = 8;

        let governor = Arc::new(MemoryGovernor::new(PAGE * CAPACITY_PAGES));
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let governor = Arc::clone(&governor);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                for _ in 0..ATTEMPTS {
                    if let Some(reservation) = governor.try_acquire(PAGE) {
                        assert!(
                            governor.telemetry().current_capacity_bytes
                                <= (PAGE * CAPACITY_PAGES) as u64
                        );
                        thread::yield_now();
                        drop(reservation);
                    }
                }
            }));
        }
        for handle in handles {
            handle.join().expect("reservation stress thread succeeds");
        }

        let telemetry = governor.telemetry();
        assert_eq!(telemetry.current_capacity_bytes, 0);
        assert!(telemetry.peak_capacity_bytes >= telemetry.current_capacity_bytes);
        assert!(telemetry.peak_capacity_bytes <= telemetry.hard_limit_bytes);
        assert_eq!(telemetry.grant_events, telemetry.release_events);
        assert_eq!(
            telemetry.granted_bytes_cumulative,
            telemetry.released_bytes_cumulative
        );
        assert_eq!(
            telemetry.grant_events + telemetry.denial_events,
            (THREADS * ATTEMPTS) as u64
        );
    }
}
