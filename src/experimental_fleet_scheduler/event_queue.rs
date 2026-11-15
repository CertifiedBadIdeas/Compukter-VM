/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

//! Entry- and byte-bounded ordinary publication. The queue owns admitted bytes;
//! workers retain refused events without copying or consuming their payloads.

use std::collections::{HashMap, VecDeque};
#[cfg(test)]
use std::sync::mpsc;
use std::sync::mpsc::{TryRecvError, TrySendError};
#[cfg(test)]
use std::sync::Condvar;
use std::sync::{Arc, Mutex};

use super::memory::observation_bytes;
use super::{FleetEvent, FleetMemoryUsage};

struct Envelope {
    event: FleetEvent,
    bytes: usize,
    oversized: bool,
}

struct State {
    limit: usize,
    machine_limit: usize,
    used: usize,
    peak: usize,
    capacity: usize,
    queue: VecDeque<Envelope>,
    senders: usize,
    headroom: usize,
    machines: HashMap<u64, usize>,
    tracking_machines: bool,
    receiver_alive: bool,
}

struct Shared {
    state: Mutex<State>,
    #[cfg(test)]
    changed: Condvar,
}

pub(super) struct EventSender {
    shared: Arc<Shared>,
}

pub(super) struct EventReceiver {
    shared: Arc<Shared>,
}

pub(super) fn channel(
    capacity: usize,
    limit: usize,
    machine_limit: usize,
) -> (EventSender, EventReceiver) {
    assert!(capacity > 0 && limit > 0 && machine_limit > 0);
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            limit,
            machine_limit,
            used: 0,
            peak: 0,
            capacity,
            queue: VecDeque::new(),
            senders: 1,
            headroom: 0,
            machines: HashMap::new(),
            tracking_machines: false,
            receiver_alive: true,
        }),
        #[cfg(test)]
        changed: Condvar::new(),
    });
    (
        EventSender {
            shared: Arc::clone(&shared),
        },
        EventReceiver { shared },
    )
}

impl EventSender {
    // Return refused ownership inline: boxing the error would allocate on
    // every backpressure retry. FleetEvent intentionally carries its payload.
    #[allow(clippy::result_large_err)]
    pub(super) fn try_send(&self, event: FleetEvent) -> Result<(), TrySendError<FleetEvent>> {
        let bytes = observation_bytes(&event) + std::mem::size_of::<Envelope>()
            - std::mem::size_of::<FleetEvent>();
        let machine = event.machine();
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.receiver_alive {
            return Err(TrySendError::Disconnected(event));
        }
        let oversized = bytes > state.limit || bytes > state.machine_limit;
        // Small queues cannot exceed any individual owner's byte limit. Build
        // detailed ownership accounting only when aggregate bytes could do so;
        // keep it through this busy period to avoid threshold oscillation.
        if state.headroom == 0
            && !oversized
            && !state.tracking_machines
            && bytes <= state.limit - state.used
            && state.used + bytes > state.machine_limit
        {
            let mut machines = std::mem::take(&mut state.machines);
            for envelope in &state.queue {
                *machines.entry(envelope.event.machine()).or_default() += envelope.bytes;
            }
            state.machines = machines;
            state.tracking_machines = true;
        }
        let local = if state.tracking_machines {
            state.machines.get(&machine).copied().unwrap_or(0)
        } else {
            0
        };
        // Size alone must not make an observation permanently inadmissible.
        // It borrows the empty queue exclusively, rather than making all future
        // operations permanently inadmissible or accumulating oversized backlog.
        if state.queue.len() == state.capacity
            || state.headroom != 0
            || if oversized {
                !state.queue.is_empty()
            } else {
                bytes > state.limit - state.used || bytes > state.machine_limit - local
            }
        {
            return Err(TrySendError::Full(event));
        }
        state.queue.push_back(Envelope {
            event,
            bytes,
            oversized,
        });
        if oversized {
            state.headroom = bytes;
        } else {
            state.used += bytes;
            state.peak = state.peak.max(state.used);
            if state.tracking_machines {
                *state.machines.entry(machine).or_default() += bytes;
            }
        }
        #[cfg(test)]
        self.shared.changed.notify_one();
        Ok(())
    }
}

impl EventReceiver {
    fn release(state: &mut State, envelope: Envelope) -> FleetEvent {
        if envelope.oversized {
            state.headroom = 0;
        } else {
            state.used -= envelope.bytes;
            if state.tracking_machines {
                let machine = envelope.event.machine();
                let local = state
                    .machines
                    .get_mut(&machine)
                    .expect("admitted event owner");
                *local -= envelope.bytes;
                if *local == 0 {
                    state.machines.remove(&machine);
                }
                if state.queue.is_empty() {
                    state.tracking_machines = false;
                    debug_assert!(state.machines.is_empty());
                }
            }
        }
        envelope.event
    }

    pub(super) fn try_recv(&self) -> Result<FleetEvent, TryRecvError> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        match state.queue.pop_front() {
            Some(event) => Ok(Self::release(&mut state, event)),
            None if state.senders == 0 => Err(TryRecvError::Disconnected),
            None => Err(TryRecvError::Empty),
        }
    }

    #[cfg(test)]
    pub(super) fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<FleetEvent, mpsc::RecvTimeoutError> {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let (mut state, _) = self
            .shared
            .changed
            .wait_timeout_while(state, timeout, |state| {
                state.queue.is_empty() && state.senders != 0
            })
            .unwrap_or_else(|e| e.into_inner());
        match state.queue.pop_front() {
            Some(event) => Ok(Self::release(&mut state, event)),
            None if state.senders == 0 => Err(mpsc::RecvTimeoutError::Disconnected),
            None => Err(mpsc::RecvTimeoutError::Timeout),
        }
    }

    pub(super) fn memory(&self) -> FleetMemoryUsage {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        FleetMemoryUsage {
            retained_bytes: state.used,
            peak_bytes: state.peak,
            limit_bytes: state.limit,
        }
    }

    pub(super) fn headroom_bytes(&self) -> usize {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .headroom
    }
}

impl Drop for EventReceiver {
    fn drop(&mut self) {
        // Sender clones cannot retain accounting or accept new observations
        // after their receiving owner disappears. Queued payloads are released here.
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.receiver_alive = false;
        state.used = 0;
        state.queue.clear();
        state.headroom = 0;
        state.machines.clear();
    }
}

impl Clone for EventSender {
    fn clone(&self) -> Self {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .senders += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for EventSender {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.senders -= 1;
        #[cfg(test)]
        self.shared.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    fn event(machine: u64) -> FleetEvent {
        FleetEvent::Error {
            machine,
            message: String::new(),
        }
    }

    #[test]
    fn machine_byte_pressure_leaves_capacity_for_another_machine() {
        let bytes = std::mem::size_of::<Envelope>();
        let (sender, receiver) = channel(8, 3 * bytes, bytes);
        sender.try_send(event(1)).unwrap();
        assert!(matches!(
            sender.try_send(event(1)),
            Err(TrySendError::Full(_))
        ));
        sender.try_send(event(2)).unwrap();
        assert_eq!(receiver.memory().retained_bytes, 2 * bytes);
        assert_eq!(receiver.try_recv().unwrap().machine(), 1);
        sender.try_send(event(1)).unwrap();
        assert_eq!(receiver.try_recv().unwrap().machine(), 2);
        assert_eq!(receiver.try_recv().unwrap().machine(), 1);
        assert_eq!(receiver.memory().retained_bytes, 0);
        assert!(receiver.shared.state.lock().unwrap().machines.is_empty());
    }

    #[test]
    fn shared_byte_limit_rejects_even_with_free_entry_slots() {
        let bytes = std::mem::size_of::<Envelope>();
        let (sender, receiver) = channel(16, 2 * bytes, 2 * bytes);
        sender.try_send(event(1)).unwrap();
        sender.try_send(event(2)).unwrap();
        assert!(matches!(
            sender.try_send(event(3)),
            Err(TrySendError::Full(_))
        ));
        assert_eq!(receiver.memory().retained_bytes, 2 * bytes);
        assert_eq!(receiver.try_recv().unwrap().machine(), 1);
        sender.try_send(event(3)).unwrap();
        assert_eq!(receiver.try_recv().unwrap().machine(), 2);
        assert_eq!(receiver.try_recv().unwrap().machine(), 3);
        assert_eq!(receiver.memory().peak_bytes, 2 * bytes);
    }

    #[test]
    fn entry_limit_remains_independent_of_byte_capacity() {
        let (sender, receiver) = channel(1, usize::MAX, usize::MAX);
        sender.try_send(event(1)).unwrap();
        assert!(matches!(
            sender.try_send(event(2)),
            Err(TrySendError::Full(_))
        ));
        receiver.try_recv().unwrap();
        sender.try_send(event(2)).unwrap();
    }

    #[test]
    fn oversized_event_borrows_empty_queue_without_copying_or_accumulating() {
        use crate::{ComputerAdvanceOutcome, ComputerValue};
        // Exercise both a global limit and a smaller per-machine limit.
        for (global, local) in [(4096, 4096), (1024 * 1024, 4096)] {
            let (sender, receiver) = channel(8, global, local);
            sender.try_send(event(1)).unwrap();
            let payload = vec![42; 256 * 1024].into_boxed_slice();
            let backing = payload.as_ptr();
            let large = FleetEvent::Boundary {
                machine: 2,
                tick: 1,
                outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::ByteArray(payload))),
            };
            let Err(TrySendError::Full(large)) = sender.try_send(large) else {
                panic!("oversized event bypassed occupied queue");
            };
            assert_eq!(receiver.try_recv().unwrap().machine(), 1);
            sender.try_send(large).unwrap();
            assert_eq!(receiver.memory().retained_bytes, 0);
            assert!(receiver.headroom_bytes() >= 256 * 1024);
            assert!(matches!(
                sender.try_send(event(3)),
                Err(TrySendError::Full(_))
            ));
            let FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::ByteArray(payload))),
                ..
            } = receiver.try_recv().unwrap()
            else {
                panic!("payload missing")
            };
            assert_eq!(payload.as_ptr(), backing);
            assert_eq!(receiver.headroom_bytes(), 0);
            sender.try_send(event(3)).unwrap();
        }
    }

    #[test]
    fn concurrent_producers_share_one_atomic_admission_limit() {
        let bytes = std::mem::size_of::<Envelope>();
        let (sender, receiver) = channel(64, 3 * bytes, bytes);
        let barrier = Barrier::new(9);
        std::thread::scope(|scope| {
            for machine in 0..8 {
                let sender = sender.clone();
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    let _ = sender.try_send(event(machine));
                });
            }
            barrier.wait();
        });
        assert_eq!(receiver.memory().retained_bytes, 3 * bytes);
        assert_eq!(receiver.memory().peak_bytes, 3 * bytes);
        let mut delivered = 0;
        while receiver.try_recv().is_ok() {
            delivered += 1;
        }
        assert_eq!(delivered, 3);
        assert_eq!(receiver.memory().retained_bytes, 0);
    }

    #[test]
    fn disconnected_receiver_releases_queued_storage_and_refuses_retries() {
        let (sender, receiver) = channel(8, 4096, 4096);
        sender.try_send(event(1)).unwrap();
        let shared = Arc::clone(&sender.shared);
        drop(receiver);
        assert_eq!(shared.state.lock().unwrap().used, 0);
        assert!(shared.state.lock().unwrap().queue.is_empty());
        assert!(matches!(
            sender.try_send(event(2)),
            Err(TrySendError::Disconnected(_))
        ));
    }
}
