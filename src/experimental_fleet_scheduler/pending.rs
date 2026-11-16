/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

//! Lazy, bounded output batches for backpressured machines. A worker can run
//! other machines while all retained batches fit the shared reservations. If an
//! already started operation exhausts capacity, only that operation may borrow
//! worker headroom; ordinary execution then pauses until it leaves this owner.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::TrySendError;
use std::sync::Arc;

use super::memory::{observation_bytes, ByteBudget, Reservation};
use super::{EventSender, FleetEvent, FleetMemoryLimits};

/// Only a stalled flush may park; successful bounded publication can leave more
/// events ready even if the receiver has not had a chance to signal again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FlushResult {
    Published,
    Blocked,
    Disconnected,
}

pub(super) struct PendingCapacity {
    pub(super) memory: Arc<ByteBudget>,
    pub(super) entries: Arc<ByteBudget>,
    machine_limit: usize,
    pub(super) headroom_bytes: AtomicUsize,
    pub(super) inflight_bytes: AtomicUsize,
}

impl PendingCapacity {
    pub(super) fn new(limits: FleetMemoryLimits, entries: usize) -> Arc<Self> {
        Arc::new(Self {
            memory: ByteBudget::new(limits.pending_output_bytes),
            entries: ByteBudget::new(entries),
            machine_limit: limits.pending_output_bytes_per_machine,
            headroom_bytes: AtomicUsize::new(0),
            inflight_bytes: AtomicUsize::new(0),
        })
    }
}

struct PendingEvent {
    event: FleetEvent,
    lease: PendingLease,
}

/// The event carries its reservation and gauges through retries. Every exit,
/// including cancellation and worker failure, releases them through this owner.
struct PendingLease {
    capacity: Arc<PendingCapacity>,
    payload_bytes: usize,
    reservation: Option<(Reservation, Reservation)>,
}

impl Drop for PendingLease {
    fn drop(&mut self) {
        self.capacity
            .inflight_bytes
            .fetch_sub(self.payload_bytes, Ordering::AcqRel);
        if self.reservation.is_none() {
            self.capacity
                .headroom_bytes
                .fetch_sub(self.payload_bytes, Ordering::AcqRel);
        }
    }
}

#[derive(Default)]
struct Batch {
    events: VecDeque<PendingEvent>,
    bytes: usize,
    overflowed: bool,
}

#[derive(Default)]
pub(super) struct PendingOutput {
    batches: HashMap<u64, Batch>,
    ready: VecDeque<u64>,
    overflow: Option<u64>,
}

impl PendingOutput {
    pub(super) fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    pub(super) fn contains(&self, machine: u64) -> bool {
        self.batches.contains_key(&machine)
    }

    pub(super) fn blocks_worker(&self) -> bool {
        self.overflow.is_some()
    }

    pub(super) fn push(
        &mut self,
        event: FleetEvent,
        transport: &EventSender,
        waiters: &AtomicUsize,
    ) {
        let machine = event.machine();
        let payload_bytes = observation_bytes(&event);
        let bytes =
            payload_bytes + std::mem::size_of::<PendingEvent>() - std::mem::size_of::<FleetEvent>();
        let was_empty = self.is_empty();
        let batch = self.batches.entry(machine).or_insert_with(|| {
            self.ready.push_back(machine);
            Batch::default()
        });
        assert!(
            batch.events.len() < 3,
            "worker operation exceeded output batch bound"
        );
        batch.bytes += bytes;
        let reservation = if !batch.overflowed && batch.bytes <= transport.pending.machine_limit {
            transport.pending.memory.reserve(bytes).and_then(|memory| {
                transport
                    .pending
                    .entries
                    .reserve(1)
                    .map(|entry| (memory, entry))
            })
        } else {
            None
        };
        if reservation.is_none() {
            assert!(
                self.overflow.is_none_or(|owner| owner == machine),
                "multiple operations borrowed worker headroom"
            );
            transport
                .pending
                .headroom_bytes
                .fetch_add(payload_bytes, Ordering::AcqRel);
            batch.overflowed = true;
            self.overflow = Some(machine);
        }
        transport
            .pending
            .inflight_bytes
            .fetch_add(payload_bytes, Ordering::AcqRel);
        batch.events.push_back(PendingEvent {
            event,
            lease: PendingLease {
                capacity: Arc::clone(&transport.pending),
                payload_bytes,
                reservation,
            },
        });
        if was_empty {
            waiters.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// One event from each of at most four machines per worker turn. A local
    /// admission refusal must not prevent publication by another machine.
    pub(super) fn flush(&mut self, transport: &EventSender, waiters: &AtomicUsize) -> FlushResult {
        let was_empty = self.is_empty();
        let mut published = false;
        for _ in 0..self.ready.len().min(4) {
            let machine = self.ready.pop_front().expect("pending owner");
            let batch = self.batches.get_mut(&machine).expect("pending batch");
            let PendingEvent { event, lease } = batch.events.pop_front().expect("pending event");
            match transport.sender.try_send(event) {
                Ok(()) => {
                    published = true;
                    drop(lease);
                    transport.signal.notify();
                }
                Err(error) => {
                    let disconnected = matches!(error, TrySendError::Disconnected(_));
                    let event = match error {
                        TrySendError::Full(event) | TrySendError::Disconnected(event) => event,
                    };
                    batch.events.push_front(PendingEvent { event, lease });
                    self.ready.push_back(machine);
                    if disconnected {
                        return FlushResult::Disconnected;
                    }
                    continue;
                }
            }
            if batch.events.is_empty() {
                self.batches.remove(&machine);
                if self.overflow == Some(machine) {
                    self.overflow = None;
                }
            } else {
                self.ready.push_back(machine);
            }
        }
        if !was_empty && self.is_empty() {
            waiters.fetch_sub(1, Ordering::SeqCst);
        }
        if published {
            FlushResult::Published
        } else {
            FlushResult::Blocked
        }
    }

    pub(super) fn drain_machine(
        &mut self,
        machine: u64,
        waiters: &AtomicUsize,
        mut retain: impl FnMut(FleetEvent),
    ) {
        let Some(batch) = self.batches.remove(&machine) else {
            return;
        };
        self.ready.retain(|owner| *owner != machine);
        if self.overflow == Some(machine) {
            self.overflow = None;
        }
        for PendingEvent { event, lease } in batch.events {
            drop(lease);
            retain(event);
        }
        if self.is_empty() {
            waiters.fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub(super) fn clear(&mut self, waiters: &AtomicUsize) {
        if self.is_empty() {
            return;
        }
        self.batches.clear();
        self.ready.clear();
        self.overflow = None;
        waiters.fetch_sub(1, Ordering::SeqCst);
    }
}
