/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */
//! Reserve a bounded queue position before constructing an owned compilation plan.
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

pub(super) const CAPACITY: usize = 8;

#[derive(Debug)]
pub(super) struct Capacity {
    used: AtomicUsize,
    pub epoch: AtomicU32,
}

impl Capacity {
    pub const fn new() -> Self {
        Self {
            used: AtomicUsize::new(0),
            epoch: AtomicU32::new(0),
        }
    }

    pub fn reserve(&self) -> Result<Permit<'_>, u32> {
        // A release between this snapshot, rejection and wait publication is visible.
        let epoch = self.epoch.load(Ordering::Acquire);
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < CAPACITY).then_some(used + 1)
            })
            .map(|_| Permit { capacity: self })
            .map_err(|_| epoch)
    }

    #[cfg(any(test, feature = "guest-benchmark"))]
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub(super) struct Permit<'a> {
    capacity: &'a Capacity,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let before = self.capacity.used.fetch_sub(1, Ordering::Release);
        debug_assert!(before > 0);
        self.capacity.epoch.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_and_queued_jobs_share_one_capacity_until_receive_or_rejection() {
        let capacity = Capacity::new();
        let mut permits: Vec<_> = (0..CAPACITY).map(|_| capacity.reserve().unwrap()).collect();
        let denied = capacity.reserve().unwrap_err();
        assert_eq!(capacity.used(), CAPACITY);
        drop(permits.pop()); // Receive a queued job, or abandon an unsupported plan.
        assert_ne!(capacity.epoch.load(Ordering::Acquire), denied);
        let next = capacity.reserve().unwrap();
        assert_eq!(capacity.used(), CAPACITY);
        drop(next);
        drop(permits);
        assert_eq!(capacity.used(), 0);
    }

    #[test]
    fn canceled_builders_release_positions_even_before_wait_publication_and_wrap() {
        let capacity = Capacity::new();
        capacity.epoch.store(u32::MAX, Ordering::Relaxed);
        let mut permits: Vec<_> = (0..CAPACITY).map(|_| capacity.reserve().unwrap()).collect();
        let denied = capacity.reserve().unwrap_err();
        drop(permits.pop());
        assert_eq!(capacity.epoch.load(Ordering::Acquire), 0);
        assert_ne!(capacity.epoch.load(Ordering::Acquire), denied);
        assert!(capacity.reserve().is_ok());
        drop(permits);
        assert_eq!(capacity.used(), 0);
    }

    #[test]
    fn concurrent_producers_never_construct_more_plans_than_queue_positions() {
        let capacity = Capacity::new();
        let live = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    for _ in 0..128 {
                        if let Ok(permit) = capacity.reserve() {
                            let count = live.fetch_add(1, Ordering::AcqRel) + 1;
                            maximum.fetch_max(count, Ordering::Relaxed);
                            assert!(count <= CAPACITY);
                            std::thread::yield_now();
                            live.fetch_sub(1, Ordering::AcqRel);
                            drop(permit);
                        }
                    }
                });
            }
        });
        assert!(maximum.load(Ordering::Relaxed) > 0);
        assert_eq!(capacity.used(), 0);
        assert_eq!(live.load(Ordering::Relaxed), 0);
    }
}
