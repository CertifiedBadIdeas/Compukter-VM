/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::TrySendError;
use std::sync::Arc;

use super::memory::{observation_bytes, ByteBudget, OutputHeadroom, Reservation};
use super::pending::PendingOutput;
use super::{
    CloseRequest, EventSender, FleetDetachCompletion, FleetDetachRejection, FleetDetachResult,
    FleetEvent, FleetQuiescent, FleetStopCompletion, LifecycleCompletion,
};

/// Machines with undelivered output pause independently under shared byte and
/// entry reservations. Only an operation exceeding those reservations pauses
/// the whole worker. Lifecycle completions retain their separate ownership.
pub(super) struct WorkerOutput<'a> {
    transport: EventSender,
    pending: RefCell<PendingOutput>,
    closing: RefCell<Option<ClosingOutput>>,
    headroom: Arc<AtomicBool>,
    waiters: &'a AtomicUsize,
}

impl<'a> WorkerOutput<'a> {
    pub(super) fn new(transport: EventSender, waiters: &'a AtomicUsize) -> Self {
        Self {
            transport,
            pending: RefCell::new(PendingOutput::default()),
            closing: RefCell::new(None),
            headroom: Arc::new(AtomicBool::new(true)),
            waiters,
        }
    }

    #[cfg(test)]
    pub(super) fn has_pending(&self) -> bool {
        !self.pending.borrow().is_empty() || !self.headroom.load(Ordering::Acquire)
    }

    pub(super) fn closing_machine(&self) -> Option<u64> {
        self.closing
            .borrow()
            .as_ref()
            .map(|closing| closing.machine)
    }

    pub(super) fn blocks_worker(&self) -> bool {
        self.pending.borrow().blocks_worker() || !self.headroom.load(Ordering::Acquire)
    }

    pub(super) fn blocks_machine(&self, machine: u64) -> bool {
        self.pending.borrow().contains(machine)
    }

    pub(super) fn flush(&self) -> bool {
        self.pending
            .borrow_mut()
            .flush(&self.transport, self.waiters)
    }

    pub(super) fn begin_closing(&self, machine: u64, request: CloseRequest) -> bool {
        let mut closing = self.closing.borrow_mut();
        if let Some(previous) = closing.as_ref() {
            assert_eq!((previous.machine, previous.request), (machine, request));
            return true;
        }
        if matches!(request, CloseRequest::Detach { .. }) && !self.headroom.load(Ordering::Acquire)
        {
            return false;
        }
        let mut output = ClosingOutput {
            machine,
            request,
            observations: VecDeque::new(),
            reports: VecDeque::new(),
            reservations: Vec::new(),
            memory: Arc::clone(&self.transport.detach_memory),
            local_memory: ByteBudget::new(self.transport.detach_machine_limit),
            overflowed: false,
            overflow_bytes: 0,
        };
        self.pending
            .borrow_mut()
            .drain_machine(machine, self.waiters, |event| output.retain(event));
        *closing = Some(output);
        true
    }

    pub(super) fn overflowed_detach(&self) -> Option<u64> {
        self.closing
            .borrow()
            .as_ref()
            .filter(|closing| closing.overflowed)
            .map(|closing| closing.machine)
    }

    fn complete(&self, machine: u64, completion: LifecycleCompletion) {
        let previous = self
            .transport
            .completions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(machine, super::lifecycle::LifecycleSlot::new(completion));
        assert!(previous.is_none(), "duplicate machine lifecycle completion");
        self.transport.signal.notify();
    }

    pub(super) fn complete_stop(&self, mut completion: FleetStopCompletion) {
        let closing = self.closing.borrow_mut().take().expect("close in progress");
        assert_eq!(
            (closing.machine, closing.request),
            (completion.machine, CloseRequest::Stop)
        );
        completion.prior_reports = closing.reports;
        self.complete(completion.machine, LifecycleCompletion::Stop(completion));
    }

    pub(super) fn complete_detach(
        &self,
        machine: u64,
        result: FleetDetachResult,
        resume: Option<Arc<AtomicBool>>,
    ) {
        let closing = self.closing.borrow_mut().take().expect("close in progress");
        assert_eq!(closing.machine, machine);
        assert!(matches!(closing.request, CloseRequest::Detach { .. }));
        self.complete(
            machine,
            LifecycleCompletion::Detach(FleetDetachCompletion::new(
                machine,
                closing.observations,
                result,
                resume,
                closing.reservations,
                if closing.overflowed {
                    Some(OutputHeadroom::retain(
                        Arc::clone(&self.headroom),
                        Arc::clone(&self.transport.detach_headroom),
                        closing.overflow_bytes,
                    ))
                } else {
                    None
                },
            )),
        );
    }

    pub(super) fn reject_detach(&self, machine: u64, resume: Option<Arc<AtomicBool>>) {
        self.complete(
            machine,
            LifecycleCompletion::Detach(FleetDetachCompletion::new(
                machine,
                VecDeque::new(),
                FleetDetachResult::Rejected(FleetDetachRejection::OutputCapacity),
                resume,
                Vec::new(),
                None,
            )),
        );
    }

    pub(super) fn emit(&self, event: FleetEvent) -> bool {
        if let Some(closing) = self.closing.borrow_mut().as_mut() {
            if closing.machine == event.machine() {
                closing.retain(event);
                return true;
            }
        }
        let mut pending = self.pending.borrow_mut();
        let event = if !pending.contains(event.machine()) {
            match self.transport.sender.try_send(event) {
                Ok(()) => {
                    self.transport.signal.notify();
                    return true;
                }
                Err(TrySendError::Disconnected(_)) => return false,
                Err(TrySendError::Full(event)) => event,
            }
        } else {
            event
        };
        pending.push(event, &self.transport, self.waiters);
        true
    }
}

struct ClosingOutput {
    machine: u64,
    request: CloseRequest,
    observations: VecDeque<FleetEvent>,
    reports: VecDeque<FleetQuiescent>,
    reservations: Vec<(Reservation, Reservation)>,
    memory: Arc<ByteBudget>,
    local_memory: Arc<ByteBudget>,
    overflowed: bool,
    overflow_bytes: usize,
}

impl ClosingOutput {
    fn retain(&mut self, event: FleetEvent) {
        match self.request {
            CloseRequest::Detach { .. } => {
                // A command/Guest operation is already bounded to three events.
                // On exhaustion keep only that in-flight batch as worker headroom;
                // the worker rejects detach before processing another command.
                if !self.overflowed {
                    let bytes = observation_bytes(&event)
                        + std::mem::size_of::<(Reservation, Reservation)>();
                    let reservation = self.memory.reserve(bytes).and_then(|global| {
                        self.local_memory
                            .reserve(bytes)
                            .map(|local| (global, local))
                    });
                    if let Some(reservation) = reservation {
                        self.reservations.push(reservation);
                    } else {
                        self.overflowed = true;
                    }
                }
                if self.overflowed {
                    self.overflow_bytes += observation_bytes(&event);
                }
                self.observations.push_back(event);
            }
            CloseRequest::Stop => {
                if let FleetEvent::Quiescent {
                    tick,
                    retired,
                    timers_pending,
                    ..
                } = event
                {
                    self.reports.push_back(FleetQuiescent {
                        tick,
                        retired,
                        timers_pending,
                    });
                }
            }
        }
    }
}

impl Drop for WorkerOutput<'_> {
    fn drop(&mut self) {
        self.pending.get_mut().clear(self.waiters);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experimental_fleet_scheduler::{FleetEventSignal, FleetMemoryLimits};
    use crate::{ComputerAdvanceOutcome, ComputerValue};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    #[test]
    fn detach_charges_large_payloads_without_copying_and_releases_all_reservations() {
        for (global_limit, local_limit, overflow) in [
            (64 * 1024, 512 * 1024, true),
            (512 * 1024, 64 * 1024, true),
            (512 * 1024, 512 * 1024, false),
        ] {
            let (sender, _receiver) = super::super::event_queue::channel(
                1,
                FleetMemoryLimits::default().event_bytes,
                FleetMemoryLimits::default().event_bytes_per_machine,
            );
            let waiters = AtomicUsize::new(0);
            let memory = ByteBudget::new(global_limit);
            let headroom = Arc::new(AtomicUsize::new(0));
            let completions = Arc::new(Mutex::new(BTreeMap::new()));
            let output = WorkerOutput::new(
                EventSender {
                    sender,
                    signal: Arc::new(FleetEventSignal::default()),
                    completions: Arc::clone(&completions),
                    pending: super::super::pending::PendingCapacity::new(
                        FleetMemoryLimits::default(),
                        64,
                    ),
                    detach_memory: Arc::clone(&memory),
                    detach_machine_limit: local_limit,
                    detach_headroom: Arc::clone(&headroom),
                },
                &waiters,
            );
            assert!(output.emit(FleetEvent::Error {
                machine: 0,
                message: "occupied".into()
            }));
            let bytes = vec![42; 256 * 1024].into_boxed_slice();
            let backing = bytes.as_ptr();
            assert!(output.emit(FleetEvent::Boundary {
                machine: 1,
                tick: 1,
                outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::ByteArray(bytes))),
            }));
            assert!(output.begin_closing(
                1,
                CloseRequest::Detach {
                    mode: super::super::DetachMode::Quiescent
                }
            ));
            assert_eq!(output.overflowed_detach().is_some(), overflow);
            output.complete_detach(
                1,
                FleetDetachResult::Rejected(FleetDetachRejection::OutputCapacity),
                None,
            );
            if overflow {
                assert_eq!(memory.snapshot().retained_bytes, 0);
                assert!(headroom.load(Ordering::Acquire) >= 256 * 1024);
                assert!(
                    output.has_pending(),
                    "worker resumed before headroom was claimed"
                );
            } else {
                assert!(memory.snapshot().retained_bytes >= 256 * 1024);
                assert_eq!(headroom.load(Ordering::Acquire), 0);
            }
            let LifecycleCompletion::Detach(mut completion) = completions
                .lock()
                .unwrap()
                .remove(&1)
                .unwrap()
                .into_completion()
            else {
                panic!("detach completion missing");
            };
            let Some(FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::ByteArray(bytes))),
                ..
            }) = completion.observations.front()
            else {
                panic!("large payload was lost");
            };
            assert_eq!(bytes.as_ptr(), backing, "detach copied its payload");
            completion.release();
            assert_eq!(memory.snapshot().retained_bytes, 0);
            assert_eq!(headroom.load(Ordering::Acquire), 0);
            assert!(!output.has_pending());
        }
    }

    #[test]
    fn backpressure_preserves_large_payload_ownership_and_order() {
        let (sender, receiver) = super::super::event_queue::channel(
            1,
            FleetMemoryLimits::default().event_bytes,
            FleetMemoryLimits::default().event_bytes_per_machine,
        );
        let waiters = AtomicUsize::new(0);
        let output = WorkerOutput::new(
            EventSender {
                sender,
                signal: Arc::new(FleetEventSignal::default()),
                completions: Arc::new(Mutex::new(BTreeMap::new())),
                pending: super::super::pending::PendingCapacity::new(
                    FleetMemoryLimits::default(),
                    64,
                ),
                detach_memory: ByteBudget::new(FleetMemoryLimits::default().detach_output_bytes),
                detach_machine_limit: FleetMemoryLimits::default().detach_output_bytes_per_machine,
                detach_headroom: Arc::new(AtomicUsize::new(0)),
            },
            &waiters,
        );
        assert!(output.emit(FleetEvent::Error {
            machine: 0,
            message: "occupied".into()
        }));
        let bytes = vec![42; 256 * 1024].into_boxed_slice();
        let backing = bytes.as_ptr();
        assert!(output.emit(FleetEvent::Boundary {
            machine: 1,
            tick: 7,
            outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::ByteArray(bytes))),
        }));
        assert!(output.emit(FleetEvent::Quiescent {
            machine: 1,
            tick: 7,
            retired: 9,
            timers_pending: false
        }));
        assert_eq!(waiters.load(Ordering::SeqCst), 1);
        assert!(
            output
                .transport
                .pending
                .inflight_bytes
                .load(Ordering::Acquire)
                >= 256 * 1024
        );
        output.begin_closing(2, CloseRequest::Stop);
        output.complete_stop(FleetStopCompletion {
            machine: 2,
            pending: Vec::new(),
            filesystem_generation: 0,
            quiescent: None,
            prior_reports: VecDeque::new(),
            report_pending: false,
        });
        assert!(output.flush());
        assert!(matches!(
            receiver.try_recv(),
            Ok(FleetEvent::Error { machine: 0, .. })
        ));
        assert!(output.flush());
        let FleetEvent::Boundary {
            machine: 1,
            outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::ByteArray(bytes))),
            ..
        } = receiver.try_recv().unwrap()
        else {
            panic!("payload was lost or reordered");
        };
        assert_eq!(bytes.as_ptr(), backing, "delivery copied the payload");
        assert!(output.flush());
        assert!(matches!(
            receiver.try_recv(),
            Ok(FleetEvent::Quiescent {
                machine: 1,
                tick: 7,
                retired: 9,
                ..
            })
        ));
        assert_eq!(waiters.load(Ordering::SeqCst), 0);
        assert_eq!(
            output
                .transport
                .pending
                .inflight_bytes
                .load(Ordering::Acquire),
            0
        );
    }
    fn bounded_transport(
        capacity: usize,
        machine_bytes: usize,
        limits: FleetMemoryLimits,
        pending_entries: usize,
    ) -> (EventSender, super::super::event_queue::EventReceiver) {
        let (sender, receiver) = super::super::event_queue::channel(capacity, 8192, machine_bytes);
        (
            EventSender {
                sender,
                signal: Arc::new(FleetEventSignal::default()),
                completions: Arc::new(Mutex::new(BTreeMap::new())),
                pending: super::super::pending::PendingCapacity::new(limits, pending_entries),
                detach_memory: ByteBudget::new(limits.detach_output_bytes),
                detach_machine_limit: limits.detach_output_bytes_per_machine,
                detach_headroom: Arc::new(AtomicUsize::new(0)),
            },
            receiver,
        )
    }

    fn observation(machine: u64, message: &str) -> FleetEvent {
        FleetEvent::Error {
            machine,
            message: message.into(),
        }
    }

    #[test]
    fn another_machine_can_publish_while_one_machine_is_backpressured() {
        let (transport, receiver) = bounded_transport(
            8,
            std::mem::size_of::<FleetEvent>() + 32,
            FleetMemoryLimits::default(),
            8,
        );
        let waiters = AtomicUsize::new(0);
        let output = WorkerOutput::new(transport, &waiters);
        assert!(output.emit(observation(1, "a")));
        assert!(output.emit(observation(1, "b")));
        assert!(output.blocks_machine(1));
        assert!(!output.blocks_worker());
        assert!(output.emit(observation(2, "c")));
        assert!(!output.blocks_machine(2));
        assert_eq!(receiver.try_recv().unwrap().machine(), 1);
        assert_eq!(receiver.try_recv().unwrap().machine(), 2);
        assert!(output.flush());
        let FleetEvent::Error {
            machine: 1,
            message,
        } = receiver.try_recv().unwrap()
        else {
            panic!("pending result missing")
        };
        assert_eq!(message, "b");
        assert_eq!(waiters.load(Ordering::SeqCst), 0);
        assert_eq!(
            output
                .transport
                .pending
                .inflight_bytes
                .load(Ordering::Acquire),
            0
        );
    }

    #[test]
    fn flush_skips_a_locally_full_machine_and_preserves_each_machines_order() {
        let (transport, receiver) = bounded_transport(
            2,
            std::mem::size_of::<FleetEvent>() + 32,
            FleetMemoryLimits::default(),
            8,
        );
        let waiters = AtomicUsize::new(0);
        let output = WorkerOutput::new(transport, &waiters);
        assert!(output.emit(observation(0, "filler")));
        assert!(output.emit(observation(1, "first")));
        assert!(output.emit(observation(1, "second")));
        assert!(output.emit(observation(1, "third")));
        assert!(output.emit(observation(2, "other")));
        assert_eq!(receiver.try_recv().unwrap().machine(), 0);
        assert!(output.flush());
        assert!(output.blocks_machine(1));
        assert!(
            !output.blocks_machine(2),
            "local pressure prevented another owner's delivery"
        );
        assert_eq!(receiver.try_recv().unwrap().machine(), 1);
        assert_eq!(receiver.try_recv().unwrap().machine(), 2);
        for expected in ["second", "third"] {
            assert!(output.flush());
            let FleetEvent::Error {
                machine: 1,
                message,
            } = receiver.try_recv().unwrap()
            else {
                panic!("ordered result missing")
            };
            assert_eq!(message, expected);
        }
        assert!(!output.blocks_machine(1));
        assert_eq!(waiters.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn pending_capacity_is_shared_and_overflow_is_released_on_cancel_and_drop() {
        for (global, local, entries) in [(1, 8192, 8), (8192, 1, 8), (8192, 8192, 1)] {
            let limits = FleetMemoryLimits {
                pending_output_bytes: global,
                pending_output_bytes_per_machine: local,
                ..FleetMemoryLimits::default()
            };
            let (transport, _receiver) = bounded_transport(1, 8192, limits, entries);
            let capacity = Arc::clone(&transport.pending);
            let waiters = AtomicUsize::new(0);
            let first = WorkerOutput::new(transport.clone(), &waiters);
            let second = WorkerOutput::new(transport, &waiters);
            assert!(first.emit(observation(0, "occupied")));
            assert!(first.emit(observation(1, "pending")));
            assert_eq!(first.blocks_worker(), entries != 1);
            assert!(second.emit(observation(2, "other worker")));
            assert!(second.blocks_worker());
            assert!(capacity.memory.snapshot().retained_bytes <= global);
            assert!(capacity.entries.snapshot().retained_bytes <= entries);
            assert!(capacity.headroom_bytes.load(Ordering::Acquire) > 0);
            assert_eq!(waiters.load(Ordering::SeqCst), 2);
            assert!(first.begin_closing(1, CloseRequest::Stop));
            assert!(!first.blocks_worker());
            assert!(!first.blocks_machine(1));
            assert!(second.blocks_machine(2));
            drop(first);
            drop(second);
            assert_eq!(capacity.inflight_bytes.load(Ordering::Acquire), 0);
            assert_eq!(waiters.load(Ordering::SeqCst), 0);
            assert_eq!(capacity.memory.snapshot().retained_bytes, 0);
            assert_eq!(capacity.entries.snapshot().retained_bytes, 0);
            assert_eq!(capacity.headroom_bytes.load(Ordering::Acquire), 0);
        }
    }
    #[test]
    fn rotating_pending_owners_all_progress_with_one_delivery_slot() {
        let (transport, receiver) = bounded_transport(1, 8192, FleetMemoryLimits::default(), 16);
        let capacity = Arc::clone(&transport.pending);
        let waiters = AtomicUsize::new(0);
        let output = WorkerOutput::new(transport, &waiters);
        assert!(output.emit(observation(0, "occupied")));
        for machine in 1..=9 {
            assert!(output.emit(observation(machine, "pending")));
        }
        assert!(!output.blocks_worker());
        assert_eq!(receiver.try_recv().unwrap().machine(), 0);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..9 {
            assert!(output.flush());
            assert!(
                seen.insert(receiver.try_recv().unwrap().machine()),
                "duplicate owner delivery"
            );
        }
        assert_eq!(seen, (1..=9).collect());
        assert!(!output.has_pending());
        assert_eq!(capacity.memory.snapshot().retained_bytes, 0);
        assert_eq!(capacity.entries.snapshot().retained_bytes, 0);
        assert_eq!(
            output
                .transport
                .pending
                .inflight_bytes
                .load(Ordering::Acquire),
            0
        );
        assert_eq!(waiters.load(Ordering::SeqCst), 0);
    }
}
