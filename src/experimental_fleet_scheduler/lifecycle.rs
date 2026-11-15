/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::memory::{OutputHeadroom, Reservation};
use super::{Command, ComputerMachine, FleetEvent, FleetTimer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseRequest {
    Stop,
    Detach { preserve_timers_at: Option<u64> },
}

impl CloseRequest {
    pub(super) fn command(self, machine: u64) -> Command {
        match self {
            Self::Stop => Command::Stop { machine },
            Self::Detach { preserve_timers_at } => Command::Detach {
                machine,
                preserve_timers_at,
            },
        }
    }
}

/// The last accounting report is retained alongside the stop result so that
/// stopping does not depend on space in the ordinary event queue.
pub struct FleetStopCompletion {
    pub machine: u64,
    pub pending: Vec<(u32, u64)>,
    /// Final filesystem generation after all accepted commands, before the VM is destroyed.
    pub filesystem_generation: u64,
    /// Final cumulative accounting for the last tick, even if it was already
    /// emitted as an ordinary event. Consumers must reconcile by tick.
    pub quiescent: Option<FleetQuiescent>,
    /// Reports cancelled from ordinary delivery while draining accepted commands.
    pub prior_reports: VecDeque<FleetQuiescent>,
    pub(super) report_pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetQuiescent {
    pub tick: u64,
    pub retired: u64,
    pub timers_pending: bool,
}

/// Output produced before detachment remains owned by this completion, including
/// on refusal. Already published ordinary events remain in the ordinary queue.
pub struct FleetDetachCompletion {
    pub machine: u64,
    pub observations: VecDeque<FleetEvent>,
    pub result: FleetDetachResult,
    resume: Option<Arc<AtomicBool>>,
    reservations: Vec<(Reservation, Reservation)>,
    headroom: Option<OutputHeadroom>,
}

#[derive(Debug)]
pub enum FleetDetachResult {
    Detached {
        computer: Box<ComputerMachine>,
        timers: Vec<FleetTimer>,
    },
    Rejected(FleetDetachRejection),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FleetDetachRejection {
    Busy,
    OutputCapacity,
}

pub(super) enum LifecycleCompletion {
    Stop(FleetStopCompletion),
    Detach(FleetDetachCompletion),
}

pub(super) type LifecycleCompletions = Arc<Mutex<BTreeMap<u64, LifecycleSlot>>>;

/// A claimed stream cannot also be drained by ordinary polling. The final
/// delivery retains the ownership fence and memory reservations until ack.
pub(super) struct LifecycleSlot {
    completion: Option<LifecycleCompletion>,
    delivery: Delivery,
}

enum Delivery {
    Unclaimed,
    Ready,
    Observation,
    Terminal(CompletionLease),
}

struct CompletionLease {
    remove_owner: bool,
    resume: Option<Arc<AtomicBool>>,
    reservations: Vec<(Reservation, Reservation)>,
    headroom: Option<OutputHeadroom>,
}

impl CompletionLease {
    fn release(self) -> bool {
        drop(self.reservations);
        drop(self.headroom);
        if let Some(resume) = self.resume {
            resume.store(true, Ordering::Release);
        }
        self.remove_owner
    }
}

impl LifecycleSlot {
    pub(super) fn new(completion: LifecycleCompletion) -> Self {
        Self {
            completion: Some(completion),
            delivery: Delivery::Unclaimed,
        }
    }

    pub(super) fn unclaimed(&self) -> Option<&LifecycleCompletion> {
        matches!(self.delivery, Delivery::Unclaimed)
            .then_some(self.completion.as_ref())
            .flatten()
    }

    pub(super) fn into_completion(self) -> LifecycleCompletion {
        assert!(matches!(self.delivery, Delivery::Unclaimed));
        self.completion.expect("unclaimed completion exists")
    }

    pub(super) fn ordinary_observation(&mut self) -> Option<FleetEvent> {
        assert!(matches!(self.delivery, Delivery::Unclaimed));
        self.completion
            .as_mut()
            .expect("unclaimed completion exists")
            .next_observation()
    }

    pub(super) fn poll(&mut self, machine: u64) -> Option<FleetEvent> {
        if matches!(self.delivery, Delivery::Observation | Delivery::Terminal(_)) {
            return None;
        }
        self.delivery = Delivery::Ready;
        if let Some(event) = self.completion.as_mut()?.next_observation() {
            self.delivery = Delivery::Observation;
            return Some(event);
        }
        let (event, lease) = self.completion.take()?.into_delivery(machine);
        self.delivery = Delivery::Terminal(lease);
        Some(event)
    }

    /// None means no delivery, false an observation, true the terminal event.
    pub(super) fn acknowledge(&mut self) -> Option<bool> {
        match self.delivery {
            Delivery::Observation => {
                self.delivery = Delivery::Ready;
                Some(false)
            }
            Delivery::Terminal(_) => Some(true),
            Delivery::Unclaimed | Delivery::Ready => None,
        }
    }

    pub(super) fn finish(self) -> bool {
        let Delivery::Terminal(lease) = self.delivery else {
            unreachable!("terminal delivery required")
        };
        lease.release()
    }
}

impl FleetDetachCompletion {
    pub(super) fn new(
        machine: u64,
        observations: VecDeque<FleetEvent>,
        result: FleetDetachResult,
        resume: Option<Arc<AtomicBool>>,
        reservations: Vec<(Reservation, Reservation)>,
        headroom: Option<OutputHeadroom>,
    ) -> Self {
        Self {
            machine,
            observations,
            result,
            resume,
            reservations,
            headroom,
        }
    }

    pub(super) fn release(&mut self) {
        self.reservations.clear();
        drop(self.headroom.take());
        if let Some(resume) = self.resume.take() {
            resume.store(true, Ordering::Release);
        }
    }
}

impl LifecycleCompletion {
    pub(super) fn next_observation(&mut self) -> Option<FleetEvent> {
        match self {
            Self::Stop(completion) => {
                let report = completion.prior_reports.pop_front().or_else(|| {
                    if !completion.report_pending {
                        return None;
                    }
                    completion.report_pending = false;
                    completion.quiescent
                })?;
                Some(FleetEvent::Quiescent {
                    machine: completion.machine,
                    tick: report.tick,
                    retired: report.retired,
                    timers_pending: report.timers_pending,
                })
            }
            Self::Detach(completion) => completion.observations.pop_front(),
        }
    }

    pub(super) fn into_event(self, machine: u64) -> FleetEvent {
        let (event, lease) = self.into_delivery(machine);
        lease.release();
        event
    }

    fn into_delivery(self, machine: u64) -> (FleetEvent, CompletionLease) {
        match self {
            Self::Stop(completion) => (
                FleetEvent::Stopped {
                    machine,
                    pending: completion.pending,
                    filesystem_generation: completion.filesystem_generation,
                },
                CompletionLease {
                    remove_owner: true,
                    resume: None,
                    reservations: Vec::new(),
                    headroom: None,
                },
            ),
            Self::Detach(completion) => {
                let remove_owner = matches!(completion.result, FleetDetachResult::Detached { .. });
                let event = match completion.result {
                    FleetDetachResult::Detached { computer, timers } => FleetEvent::Detached {
                        machine,
                        computer,
                        timers,
                    },
                    FleetDetachResult::Rejected(_) => FleetEvent::DetachRejected { machine },
                };
                (
                    event,
                    CompletionLease {
                        remove_owner,
                        resume: completion.resume,
                        reservations: completion.reservations,
                        headroom: completion.headroom,
                    },
                )
            }
        }
    }
}
