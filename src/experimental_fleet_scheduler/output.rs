/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::TrySendError;

use super::{EventSender, FleetEvent, FleetQuiescent, FleetStopCompletion};

/// One worker retains at most one unfinished operation's output. Ordinary
/// execution cannot resume until it is delivered; closing commands cancel their
/// observations and retain accounting in the independent completion slot.
pub(super) struct WorkerOutput<'a> {
    transport: EventSender,
    pending: RefCell<VecDeque<FleetEvent>>,
    closing: Cell<Option<u64>>,
    reports: RefCell<VecDeque<FleetQuiescent>>,
    waiters: &'a AtomicUsize,
}

impl<'a> WorkerOutput<'a> {
    pub(super) fn new(transport: EventSender, waiters: &'a AtomicUsize) -> Self {
        Self {
            transport,
            pending: RefCell::new(VecDeque::new()),
            closing: Cell::new(None),
            reports: RefCell::new(VecDeque::new()),
            waiters,
        }
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.pending.borrow().is_empty()
    }

    pub(super) fn flush(&self) -> bool {
        let mut pending = self.pending.borrow_mut();
        let had_pending = !pending.is_empty();
        while let Some(event) = pending.pop_front() {
            match self.transport.sender.try_send(event) {
                Ok(()) => self.transport.signal.notify(),
                Err(TrySendError::Full(event)) => {
                    pending.push_front(event);
                    break;
                }
                Err(TrySendError::Disconnected(event)) => {
                    pending.push_front(event);
                    return false;
                }
            }
        }
        if had_pending && pending.is_empty() {
            self.waiters.fetch_sub(1, Ordering::SeqCst);
        }
        true
    }

    fn retain_accounting(&self, event: &FleetEvent) {
        if let FleetEvent::Quiescent {
            tick,
            retired,
            timers_pending,
            ..
        } = event
        {
            self.reports.borrow_mut().push_back(FleetQuiescent {
                tick: *tick,
                retired: *retired,
                timers_pending: *timers_pending,
            });
        }
    }

    pub(super) fn begin_closing(&self, machine: u64) {
        if self.closing.replace(Some(machine)) == Some(machine) {
            return;
        }
        assert!(
            self.reports.borrow().is_empty(),
            "previous close accounting not collected"
        );
        let mut pending = self.pending.borrow_mut();
        let had_pending = !pending.is_empty();
        pending.retain(|event| {
            if event.machine() != machine {
                return true;
            }
            self.retain_accounting(event);
            false
        });
        if had_pending && pending.is_empty() {
            self.waiters.fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub(super) fn complete_stop(&self, mut completion: FleetStopCompletion) {
        assert_eq!(self.closing.get(), Some(completion.machine));
        self.closing.set(None);
        completion.prior_reports = std::mem::take(&mut *self.reports.borrow_mut());
        let previous = self
            .transport
            .stops
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(completion.machine, completion);
        assert!(previous.is_none(), "duplicate machine stop completion");
        self.transport.signal.notify();
    }

    pub(super) fn emit(&self, event: FleetEvent) -> bool {
        if self.closing.get() == Some(event.machine()) {
            self.retain_accounting(&event);
            return true;
        }
        let mut pending = self.pending.borrow_mut();
        if pending.is_empty() {
            match self.transport.sender.try_send(event) {
                Ok(()) => {
                    self.transport.signal.notify();
                    return true;
                }
                Err(TrySendError::Disconnected(_)) => return false,
                Err(TrySendError::Full(event)) => {
                    self.waiters.fetch_add(1, Ordering::SeqCst);
                    pending.push_back(event);
                }
            }
        } else {
            // Tick processing and one Guest step each produce at most three
            // events. No further ordinary operation may start with pending output.
            assert!(
                pending.len() < 3,
                "worker operation exceeded output batch bound"
            );
            pending.push_back(event);
        }
        true
    }
}

impl Drop for WorkerOutput<'_> {
    fn drop(&mut self) {
        if !self.pending.get_mut().is_empty() {
            self.waiters.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experimental_fleet_scheduler::FleetEventSignal;
    use crate::{ComputerAdvanceOutcome, ComputerValue};
    use std::collections::BTreeMap;
    use std::sync::{mpsc, Arc, Mutex};

    #[test]
    fn backpressure_preserves_large_payload_ownership_and_order() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let waiters = AtomicUsize::new(0);
        let output = WorkerOutput::new(
            EventSender {
                sender,
                signal: Arc::new(FleetEventSignal::default()),
                stops: Arc::new(Mutex::new(BTreeMap::new())),
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
        output.begin_closing(2);
        output.complete_stop(FleetStopCompletion {
            machine: 2,
            pending: Vec::new(),
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
    }
}
