/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

//! Experimental, opt-in native worker fleet. This is not the production scheduler.
//!
//! Each machine is owned by one worker. Commands and events are bounded; the host
//! must drain events and retry rejected submissions. Minecraft actions stay with
//! the host. Unsupported lifecycle outcomes are surfaced instead of interpreted.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::{
    ComputerAdvanceOutcome, ComputerHostRequest, ComputerMachine, ComputerValue, HostFailure,
    HostFailureKind, HostResponse, HostValueInput,
};

#[derive(Clone, Copy, Debug)]
struct RequestCost<'a> {
    pub units: u32,
    pub over_budget_detail: &'a str,
}

/// Host operations whose cost must be determined asynchronously by the host.
#[derive(Clone, Debug)]
pub struct RequestInspection {
    pub namespace: Box<str>,
    pub name: Box<str>,
    pub abi_major: u16,
    pub abi_minor: u16,
    pub operation: u32,
    pub over_budget_detail: Box<str>,
}

impl RequestInspection {
    fn matches(&self, request: &ComputerHostRequest) -> bool {
        self.namespace == request.namespace
            && self.name == request.name
            && self.abi_major == request.abi_major
            && self.abi_minor == request.abi_minor
            && self.operation == request.operation
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FleetConfig {
    pub workers: usize,
    pub mailbox_capacity: usize,
    pub event_capacity: usize,
    pub guest_budget: u32,
    pub maintenance_budget: u32,
    pub host_budget: u32,
    pub maximum_advances_per_tick: u32,
}

#[derive(Clone, Debug)]
pub enum ReplyValue {
    Unit,
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Bool(bool),
    Char(u16),
    String(Box<[u16]>),
    Bytes(Box<[u8]>),
    Record(crate::HostRecordValue),
    Failure(HostFailureKind, Box<str>),
}

impl ReplyValue {
    fn as_response(&self) -> HostResponse<'_> {
        if let Self::Failure(kind, detail) = self {
            return HostResponse::Failure(HostFailure::new(*kind, detail));
        }
        HostResponse::Success(match self {
            Self::Unit => HostValueInput::Unit,
            Self::I32(v) => HostValueInput::I32(*v),
            Self::I64(v) => HostValueInput::I64(*v),
            Self::F32(v) => HostValueInput::F32(*v),
            Self::F64(v) => HostValueInput::F64(*v),
            Self::Bool(v) => HostValueInput::Bool(*v),
            Self::Char(v) => HostValueInput::Char(*v),
            Self::String(v) => HostValueInput::String(v),
            Self::Bytes(v) => HostValueInput::ByteArray(v),
            Self::Record(v) => HostValueInput::Record(v),
            Self::Failure(..) => unreachable!(),
        })
    }
}

#[derive(Debug)]
pub enum FleetEvent {
    Inspect {
        machine: u64,
        tick: u64,
        request: ComputerHostRequest,
    },
    Requests {
        machine: u64,
        tick: u64,
        requests: Vec<ComputerHostRequest>,
    },
    Boundary {
        machine: u64,
        tick: u64,
        outcome: ComputerAdvanceOutcome,
    },
    /// The machine was removed. Pending identities identify host actions that
    /// the embedder must cancel or discard; late replies are ignored.
    Stopped {
        machine: u64,
        pending: Vec<(u32, u64)>,
    },
    /// Requests belonging to a finished or replaced process must be discarded by the host.
    ScopeStopped {
        machine: u64,
        scope: u64,
        pending: Vec<(u32, u64)>,
    },
    Error {
        machine: u64,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    UnknownMachine,
    Full,
    Closed,
    InvalidCost,
    InvalidMachine,
    Capacity,
    IdentityExhausted,
}

#[derive(Default)]
struct Metrics {
    advances: AtomicU64,
    retired: AtomicU64,
    requests: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
pub struct FleetMetrics {
    pub advances: u64,
    pub retired: u64,
    pub requests: u64,
}

enum Command {
    Add {
        machine: u64,
        computer: Box<ComputerMachine>,
    },
    Tick {
        machine: u64,
        tick: u64,
        retirements: u32,
        deadline: Option<Instant>,
    },
    Reply {
        machine: u64,
        task: u32,
        request: u64,
        value: ReplyValue,
    },
    Stop {
        machine: u64,
    },
    TerminalKey {
        machine: u64,
        event: crate::TerminalKeyEvent,
    },
    Cost {
        machine: u64,
        task: u32,
        request: u64,
        units: u32,
    },
}

struct Actor {
    machine: ComputerMachine,
    tick: u64,
    tick_initialized: bool,
    remaining: u32,
    requests_left: u32,
    advances_left: u32,
    deadline: Option<Instant>,
    queued: bool,
    parked: bool,
    stopped: bool,
    pending: HashSet<(u32, u64)>,
    inspecting: HashSet<(u32, u64)>,
    scopes: Vec<u64>,
    request_scopes: HashMap<(u32, u64), u64>,
    inspected: HashMap<(u32, u64), u32>,
    timers: HashMap<(u32, u64), u64>,
}

impl Actor {
    fn new(machine: ComputerMachine, host_budget: u32) -> Self {
        Self {
            machine,
            tick: 0,
            tick_initialized: false,
            remaining: 0,
            requests_left: host_budget,
            advances_left: 0,
            deadline: None,
            queued: false,
            parked: false,
            stopped: false,
            pending: HashSet::new(),
            inspecting: HashSet::new(),
            scopes: vec![0],
            request_scopes: HashMap::new(),
            inspected: HashMap::new(),
            timers: HashMap::new(),
        }
    }
}

struct Owners {
    lanes: HashMap<u64, usize>,
    next_id: Option<u64>,
    next_lane: usize,
}

pub struct FleetScheduler {
    lanes: Vec<SyncSender<Command>>,
    owners: Mutex<Owners>,
    events: Receiver<FleetEvent>,
    stopped: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
    metrics: Arc<Metrics>,
    waiting_publishers: Arc<AtomicUsize>,
}

impl FleetScheduler {
    pub fn new(
        machines: Vec<(u64, ComputerMachine)>,
        config: FleetConfig,
    ) -> Result<Self, &'static str> {
        Self::new_with_request_inspections(machines, config, Vec::new())
    }

    /// Inspect configured operations through events; no host code runs on VM workers.
    pub fn new_with_request_inspections(
        machines: Vec<(u64, ComputerMachine)>,
        config: FleetConfig,
        inspections: Vec<RequestInspection>,
    ) -> Result<Self, &'static str> {
        if machines.len() > 4096 {
            return Err("fleet size exceeds maximum");
        }
        if config.workers == 0
            || config.mailbox_capacity == 0
            || config.event_capacity == 0
            || config.guest_budget == 0
            || config.host_budget == 0
            || config.maximum_advances_per_tick == 0
        {
            return Err("invalid fleet configuration");
        }
        let mut partitions: Vec<HashMap<u64, Actor>> =
            (0..config.workers).map(|_| HashMap::new()).collect();
        let mut owners = HashMap::new();
        for (index, (id, machine)) in machines.into_iter().enumerate() {
            let lane = index % config.workers;
            if owners.insert(id, lane).is_some() {
                return Err("duplicate machine identity");
            }
            partitions[lane].insert(id, Actor::new(machine, config.host_budget));
        }
        let (event_tx, events) = mpsc::sync_channel(config.event_capacity);
        let stopped = Arc::new(AtomicBool::new(false));
        let metrics = Arc::new(Metrics::default());
        let waiting_publishers = Arc::new(AtomicUsize::new(0));
        let inspections = Arc::new(inspections);
        let mut lanes = Vec::new();
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        for (lane, actors) in partitions.into_iter().enumerate() {
            let (tx, rx) = mpsc::sync_channel(config.mailbox_capacity);
            lanes.push(tx);
            let events = event_tx.clone();
            let stop = Arc::clone(&stopped);
            let worker_metrics = Arc::clone(&metrics);
            let waiting = Arc::clone(&waiting_publishers);
            let inspections = Arc::clone(&inspections);
            let worker = match thread::Builder::new()
                .name(format!("experimental-vm-{lane}"))
                .spawn(move || {
                    run_worker(
                        actors,
                        rx,
                        events,
                        stop,
                        worker_metrics,
                        waiting,
                        config,
                        inspections,
                    )
                }) {
                Ok(worker) => worker,
                Err(_) => {
                    stopped.store(true, Ordering::Release);
                    for worker in &workers {
                        worker.thread().unpark();
                    }
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err("cannot start fleet worker");
                }
            };
            workers.push(worker);
        }
        let next_id = match owners.keys().max() {
            Some(id) => id.checked_add(1),
            None => Some(0),
        };
        let next_lane = owners.len() % config.workers;
        Ok(Self {
            lanes,
            owners: Mutex::new(Owners {
                lanes: owners,
                next_id,
                next_lane,
            }),
            events,
            stopped,
            workers,
            metrics,
            waiting_publishers,
        })
    }

    /// Takes ownership only after the bounded mailbox accepts the machine. A refusal returns it.
    /// Machines must not have executed Guest instructions or have pending host operations.
    pub fn attach(
        &self,
        computer: Box<ComputerMachine>,
    ) -> Result<u64, (SubmitError, Box<ComputerMachine>)> {
        if !computer.fleet_attachable() {
            return Err((SubmitError::InvalidMachine, computer));
        }
        if self.stopped.load(Ordering::Acquire) {
            return Err((SubmitError::Closed, computer));
        }
        let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        if owners.lanes.len() >= 4096 {
            return Err((SubmitError::Capacity, computer));
        }
        let Some(id) = owners.next_id else {
            return Err((SubmitError::IdentityExhausted, computer));
        };
        let lane = owners.next_lane;
        match self.lanes[lane].try_send(Command::Add {
            machine: id,
            computer,
        }) {
            Ok(()) => {
                owners.next_id = id.checked_add(1);
                owners.next_lane = (lane + 1) % self.lanes.len();
                owners.lanes.insert(id, lane);
                self.workers[lane].thread().unpark();
                Ok(id)
            }
            Err(TrySendError::Full(Command::Add { computer, .. })) => {
                Err((SubmitError::Full, computer))
            }
            Err(TrySendError::Disconnected(Command::Add { computer, .. })) => {
                Err((SubmitError::Closed, computer))
            }
            Err(_) => unreachable!("only Add was submitted"),
        }
    }

    pub fn grant(&self, machine: u64, tick: u64, retirements: u32) -> Result<(), SubmitError> {
        self.grant_until(machine, tick, retirements, None)
    }

    pub fn grant_until(
        &self,
        machine: u64,
        tick: u64,
        retirements: u32,
        deadline: Option<Instant>,
    ) -> Result<(), SubmitError> {
        self.send(
            machine,
            Command::Tick {
                machine,
                tick,
                retirements,
                deadline,
            },
        )
    }

    pub fn terminal_key(
        &self,
        machine: u64,
        event: crate::TerminalKeyEvent,
    ) -> Result<(), SubmitError> {
        self.send(machine, Command::TerminalKey { machine, event })
    }

    pub fn reply(
        &self,
        machine: u64,
        task: u32,
        request: u64,
        value: ReplyValue,
    ) -> Result<(), SubmitError> {
        self.send(
            machine,
            Command::Reply {
                machine,
                task,
                request,
                value,
            },
        )
    }

    /// Requests asynchronous removal. Completion is reported by `Stopped`.
    pub fn stop(&self, machine: u64) -> Result<(), SubmitError> {
        self.send(machine, Command::Stop { machine })
    }

    pub fn resolve_cost(
        &self,
        machine: u64,
        task: u32,
        request: u64,
        units: u32,
    ) -> Result<(), SubmitError> {
        if units == 0 {
            return Err(SubmitError::InvalidCost);
        }
        self.send(
            machine,
            Command::Cost {
                machine,
                task,
                request,
                units,
            },
        )
    }

    fn send(&self, machine: u64, command: Command) -> Result<(), SubmitError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(SubmitError::Closed);
        }
        let lane = *self
            .owners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lanes
            .get(&machine)
            .ok_or(SubmitError::UnknownMachine)?;
        self.lanes[lane]
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => SubmitError::Full,
                TrySendError::Disconnected(_) => SubmitError::Closed,
            })?;
        self.workers[lane].thread().unpark();
        Ok(())
    }

    pub fn try_event(&self) -> Option<FleetEvent> {
        let result = self.events.try_recv().ok();
        if let Some(FleetEvent::Stopped { machine, .. }) = &result {
            self.owners
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .lanes
                .remove(machine);
        }
        if result.is_some() && self.waiting_publishers.load(Ordering::SeqCst) != 0 {
            for worker in &self.workers {
                worker.thread().unpark();
            }
        }
        result
    }

    pub fn metrics(&self) -> FleetMetrics {
        FleetMetrics {
            advances: self.metrics.advances.load(Ordering::Relaxed),
            retired: self.metrics.retired.load(Ordering::Relaxed),
            requests: self.metrics.requests.load(Ordering::Relaxed),
        }
    }
}

impl Drop for FleetScheduler {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        for worker in &self.workers {
            worker.thread().unpark();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn publish(
    sender: &SyncSender<FleetEvent>,
    stopped: &AtomicBool,
    waiters: &AtomicUsize,
    mut event: FleetEvent,
) -> bool {
    let mut registered = false;
    let success = loop {
        if stopped.load(Ordering::Acquire) {
            break false;
        }
        match sender.try_send(event) {
            Ok(()) => break true,
            Err(TrySendError::Full(returned)) => {
                event = returned;
                if !registered {
                    waiters.fetch_add(1, Ordering::SeqCst);
                    registered = true;
                    // Recheck after registering to avoid a missed wakeup.
                    continue;
                }
                thread::park_timeout(Duration::from_millis(10));
            }
            Err(TrySendError::Disconnected(_)) => break false,
        }
    };
    if registered {
        waiters.fetch_sub(1, Ordering::SeqCst);
    }
    success
}

fn ready(id: u64, actor: &mut Actor, queue: &mut VecDeque<u64>) {
    if !actor.queued && !actor.stopped && actor.remaining > 0 && actor.advances_left > 0 {
        actor.queued = true;
        queue.push_back(id);
    }
}

fn run_worker(
    mut actors: HashMap<u64, Actor>,
    commands: Receiver<Command>,
    events: SyncSender<FleetEvent>,
    stopped: Arc<AtomicBool>,
    metrics: Arc<Metrics>,
    waiters: Arc<AtomicUsize>,
    config: FleetConfig,
    inspections: Arc<Vec<RequestInspection>>,
) {
    let mut queue = VecDeque::new();
    while !stopped.load(Ordering::Acquire) {
        for _ in 0..4 {
            let command = match commands.try_recv() {
                Ok(value) => value,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            };
            match command {
                Command::Add { machine, computer } => {
                    assert!(actors
                        .insert(machine, Actor::new(*computer, config.host_budget))
                        .is_none());
                }
                Command::Tick {
                    machine,
                    tick,
                    retirements,
                    deadline,
                } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    if actor.tick_initialized && tick < actor.tick {
                        continue;
                    }
                    if !actor.tick_initialized || tick > actor.tick {
                        actor.tick_initialized = true;
                        actor.tick = tick;
                        actor.remaining = retirements;
                        actor.requests_left = config.host_budget;
                        actor.advances_left = config.maximum_advances_per_tick;
                        actor.deadline = deadline;
                    }
                    let mut due: Vec<_> = actor
                        .timers
                        .iter()
                        .filter_map(|(key, due)| (*due <= tick).then_some(*key))
                        .collect();
                    due.sort_unstable();
                    for (task, request) in due {
                        actor.timers.remove(&(task, request));
                        actor.pending.remove(&(task, request));
                        actor.request_scopes.remove(&(task, request));
                        if let Err(error) = actor.machine.resume_host_request_for(
                            task,
                            request,
                            HostResponse::Success(HostValueInput::Unit),
                        ) {
                            actor.stopped = true;
                            if !publish(
                                &events,
                                &stopped,
                                &waiters,
                                FleetEvent::Error {
                                    machine,
                                    message: format!("timer completion: {error:?}"),
                                },
                            ) {
                                return;
                            }
                            break;
                        }
                        actor.parked = false;
                    }
                    if !actor.parked {
                        ready(machine, actor, &mut queue);
                    }
                }
                Command::Reply {
                    machine,
                    task,
                    request,
                    value,
                } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    if actor.timers.contains_key(&(task, request))
                        || !actor.pending.remove(&(task, request))
                    {
                        // Completions can race with process exit and cancellation.
                        continue;
                    }
                    actor.request_scopes.remove(&(task, request));
                    if let Err(error) =
                        actor
                            .machine
                            .resume_host_request_for(task, request, value.as_response())
                    {
                        actor.stopped = true;
                        if !publish(
                            &events,
                            &stopped,
                            &waiters,
                            FleetEvent::Error {
                                machine,
                                message: format!("host completion: {error:?}"),
                            },
                        ) {
                            return;
                        }
                        continue;
                    }
                    actor.parked = false;
                    ready(machine, actor, &mut queue);
                }
                Command::Stop { machine } => {
                    if let Some(actor) = actors.remove(&machine) {
                        let mut pending: Vec<_> = actor
                            .pending
                            .iter()
                            .copied()
                            .filter(|key| !actor.timers.contains_key(key))
                            .collect();
                        pending.extend(actor.inspecting.iter().copied());
                        pending.extend(actor.inspected.keys().copied());
                        pending.sort_unstable();
                        pending.dedup();
                        drop(actor);
                        if !publish(
                            &events,
                            &stopped,
                            &waiters,
                            FleetEvent::Stopped { machine, pending },
                        ) {
                            return;
                        }
                    }
                }
                Command::TerminalKey { machine, event } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    if let Err(error) = actor.machine.submit_terminal_key(event) {
                        if !publish(
                            &events,
                            &stopped,
                            &waiters,
                            FleetEvent::Error {
                                machine,
                                message: format!("terminal key: {error:?}"),
                            },
                        ) {
                            return;
                        }
                    } else {
                        actor.parked = false;
                        ready(machine, actor, &mut queue);
                    }
                }
                Command::Cost {
                    machine,
                    task,
                    request,
                    units,
                } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    let key = (task, request);
                    if !actor.inspecting.remove(&key) {
                        // An inspection can finish after its process has exited.
                        continue;
                    }
                    actor.inspected.insert(key, units);
                    actor.parked = false;
                    ready(machine, actor, &mut queue);
                }
            }
        }
        let Some(id) = queue.pop_front() else {
            thread::park_timeout(Duration::from_millis(1));
            continue;
        };
        let Some(actor) = actors.get_mut(&id) else {
            continue;
        };
        actor.queued = false;
        if actor.stopped || actor.remaining == 0 || actor.advances_left == 0 {
            continue;
        }
        if actor
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            continue;
        }
        actor.advances_left -= 1;
        let (outcome, retired) = match actor.machine.advance_with_retirement_count(
            config.guest_budget,
            config.maintenance_budget,
            actor
                .requests_left
                .saturating_add(actor.pending.len() as u32)
                .saturating_add(actor.inspecting.len() as u32)
                .saturating_add(actor.inspected.len() as u32),
            actor.remaining,
        ) {
            Ok(value) => value,
            Err(error) => {
                actor.stopped = true;
                if !publish(
                    &events,
                    &stopped,
                    &waiters,
                    FleetEvent::Error {
                        machine: id,
                        message: format!("advance: {error:?}"),
                    },
                ) {
                    return;
                }
                continue;
            }
        };
        metrics.advances.fetch_add(1, Ordering::Relaxed);
        metrics.retired.fetch_add(retired, Ordering::Relaxed);
        actor.remaining = actor.remaining.saturating_sub(retired as u32);
        match outcome {
            ComputerAdvanceOutcome::SliceExhausted => {
                if retired == 0 {
                    // Maintenance can make progress without retiring instructions.
                    // Yield until a new tick instead of treating it as a host wait.
                    actor.remaining = 0;
                } else {
                    ready(id, actor, &mut queue);
                }
            }
            ComputerAdvanceOutcome::WaitingForHostQuota => {
                actor.remaining = 0;
                actor.parked = false;
            }
            ComputerAdvanceOutcome::HostRequestBatch(batch) => {
                let mut fresh = Vec::new();
                let mut discovered = false;
                let mut budget_exhausted = false;
                for request in batch.requests.into_vec() {
                    let key = (request.task_id, request.id);
                    if actor.pending.contains(&key) {
                        continue;
                    }
                    if actor.requests_left == 0 {
                        budget_exhausted = true;
                        break;
                    }
                    let cost = if let Some(rule) =
                        inspections.iter().find(|rule| rule.matches(&request))
                    {
                        if let Some(&units) = actor.inspected.get(&key) {
                            RequestCost {
                                units,
                                over_budget_detail: &rule.over_budget_detail,
                            }
                        } else {
                            if actor.inspecting.insert(key) {
                                actor
                                    .request_scopes
                                    .insert(key, *actor.scopes.last().expect("root scope"));
                                discovered = true;
                                if !publish(
                                    &events,
                                    &stopped,
                                    &waiters,
                                    FleetEvent::Inspect {
                                        machine: id,
                                        tick: actor.tick,
                                        request,
                                    },
                                ) {
                                    return;
                                }
                            }
                            // Preserve the order of this native request batch until
                            // the host has classified the earlier request.
                            break;
                        }
                    } else {
                        RequestCost {
                            units: 1,
                            over_budget_detail: "Host request exceeds per-tick budget",
                        }
                    };
                    if cost.units == 0 {
                        actor.stopped = true;
                        if !publish(
                            &events,
                            &stopped,
                            &waiters,
                            FleetEvent::Error {
                                machine: id,
                                message: "host request cost must be positive".into(),
                            },
                        ) {
                            return;
                        }
                        break;
                    }
                    if cost.units > config.host_budget {
                        actor.inspected.remove(&key);
                        actor.request_scopes.remove(&key);
                        actor.requests_left -= 1;
                        discovered = true;
                        if let Err(error) = actor.machine.resume_host_request_for(
                            request.task_id,
                            request.id,
                            HostResponse::Failure(HostFailure::new(
                                HostFailureKind::Other,
                                cost.over_budget_detail,
                            )),
                        ) {
                            actor.stopped = true;
                            if !publish(
                                &events,
                                &stopped,
                                &waiters,
                                FleetEvent::Error {
                                    machine: id,
                                    message: format!("host quota failure: {error:?}"),
                                },
                            ) {
                                return;
                            }
                        }
                        continue;
                    }
                    if cost.units > actor.requests_left {
                        budget_exhausted = true;
                        continue;
                    }
                    actor.requests_left -= cost.units;
                    actor.inspected.remove(&key);
                    actor.pending.insert(key);
                    actor
                        .request_scopes
                        .insert(key, *actor.scopes.last().expect("root scope"));
                    discovered = true;
                    if request.namespace.as_ref() == "compukter"
                        && request.name.as_ref() == "timer"
                        && request.operation == 0
                    {
                        match request.arguments.as_ref() {
                            [ComputerValue::I32(delay)] if *delay >= 0 => {
                                actor
                                    .timers
                                    .insert(key, actor.tick.saturating_add((*delay as u64).max(1)));
                            }
                            _ => {
                                actor.stopped = true;
                                if !publish(
                                    &events,
                                    &stopped,
                                    &waiters,
                                    FleetEvent::Error {
                                        machine: id,
                                        message: "unsupported timer request".into(),
                                    },
                                ) {
                                    return;
                                }
                            }
                        }
                    } else {
                        fresh.push(request);
                    }
                }
                let made_request = !fresh.is_empty();
                metrics
                    .requests
                    .fetch_add(fresh.len() as u64, Ordering::Relaxed);
                if made_request
                    && !publish(
                        &events,
                        &stopped,
                        &waiters,
                        FleetEvent::Requests {
                            machine: id,
                            tick: actor.tick,
                            requests: fresh,
                        },
                    )
                {
                    return;
                }
                // Probe once after a newly observed request: another cooperative task may be runnable.
                if budget_exhausted {
                    actor.remaining = 0;
                    actor.parked = false;
                } else if retired == 0 && !discovered {
                    actor.parked = true;
                } else {
                    ready(id, actor, &mut queue);
                }
            }
            lifecycle @ (ComputerAdvanceOutcome::ProcessEntered(_)
            | ComputerAdvanceOutcome::ProcessExited(_)
            | ComputerAdvanceOutcome::ProcessReplaced { .. }) => {
                let finished = match &lifecycle {
                    ComputerAdvanceOutcome::ProcessEntered(scope) => {
                        if actor.scopes.contains(scope) {
                            Some(Err("duplicate process scope"))
                        } else {
                            actor.scopes.push(*scope);
                            None
                        }
                    }
                    ComputerAdvanceOutcome::ProcessExited(scope) => {
                        if actor.scopes.len() <= 1 || actor.scopes.last() != Some(scope) {
                            Some(Err("process exit scope mismatch"))
                        } else {
                            actor.scopes.pop();
                            Some(Ok(*scope))
                        }
                    }
                    ComputerAdvanceOutcome::ProcessReplaced { previous, current } => {
                        if actor.scopes.last() != Some(previous)
                            || current <= previous
                            || actor.scopes.contains(current)
                        {
                            Some(Err("process replacement scope mismatch"))
                        } else {
                            *actor.scopes.last_mut().expect("root scope") = *current;
                            Some(Ok(*previous))
                        }
                    }
                    _ => unreachable!(),
                };
                if let Some(finished) = finished {
                    let scope = match finished {
                        Ok(scope) => scope,
                        Err(message) => {
                            actor.stopped = true;
                            if !publish(
                                &events,
                                &stopped,
                                &waiters,
                                FleetEvent::Error {
                                    machine: id,
                                    message: message.into(),
                                },
                            ) {
                                return;
                            }
                            continue;
                        }
                    };
                    let keys: Vec<_> = actor
                        .request_scopes
                        .iter()
                        .filter_map(|(key, owner)| (*owner == scope).then_some(*key))
                        .collect();
                    let mut pending = Vec::new();
                    for key in keys {
                        actor.request_scopes.remove(&key);
                        actor.inspecting.remove(&key);
                        actor.inspected.remove(&key);
                        actor.pending.remove(&key);
                        if actor.timers.remove(&key).is_none() {
                            pending.push(key);
                        }
                    }
                    pending.sort_unstable();
                    if !pending.is_empty()
                        && !publish(
                            &events,
                            &stopped,
                            &waiters,
                            FleetEvent::ScopeStopped {
                                machine: id,
                                scope,
                                pending,
                            },
                        )
                    {
                        return;
                    }
                }
                if !publish(
                    &events,
                    &stopped,
                    &waiters,
                    FleetEvent::Boundary {
                        machine: id,
                        tick: actor.tick,
                        outcome: lifecycle,
                    },
                ) {
                    return;
                }
                actor.parked = false;
                ready(id, actor, &mut queue);
            }
            other => {
                actor.stopped = true;
                if !publish(
                    &events,
                    &stopped,
                    &waiters,
                    FleetEvent::Boundary {
                        machine: id,
                        tick: actor.tick,
                        outcome: other,
                    },
                ) {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CapabilityBinding, EntryArgumentLimits, ExecutionProfile, HostValueType, OperationSchema,
    };
    use std::time::Instant;

    fn profile() -> ExecutionProfile {
        ExecutionProfile {
            heap_bytes: 1024 * 1024,
            frame_storage_bytes: 1024 * 1024,
            maximum_call_depth: 64,
            maximum_coroutines: 64,
            maximum_channels: 64,
            maximum_channel_values: 4096,
            maximum_host_requests: 64,
            maximum_events: 64,
            maximum_slice_budget: u32::MAX,
            compiler_abi: [0; 32],
            platform_abi: [0; 32],
            maximum_host_arguments: 16,
            maximum_outbound_utf16_code_units: 4096,
            maximum_inbound_utf16_code_units: 4096,
            maximum_accepted_responses: 64,
            entry_argument_limits: EntryArgumentLimits {
                maximum_count: 64,
                maximum_code_units_per_argument: 4096,
                maximum_total_code_units: 16384,
            },
        }
    }
    fn config() -> FleetConfig {
        FleetConfig {
            workers: 2,
            mailbox_capacity: 64,
            event_capacity: 64,
            guest_budget: 64,
            maintenance_budget: 64,
            host_budget: 64,
            maximum_advances_per_tick: 32,
        }
    }
    fn next(fleet: &FleetScheduler) -> FleetEvent {
        let limit = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(event) = fleet.try_event() {
                return event;
            }
            assert!(
                Instant::now() < limit,
                "timed out waiting for scheduler event"
            );
            thread::yield_now();
        }
    }
    fn wait_advances(fleet: &FleetScheduler, minimum: u64) {
        let limit = Instant::now() + Duration::from_secs(5);
        while fleet.metrics().advances < minimum {
            assert!(Instant::now() < limit, "timed out waiting for VM advance");
            thread::yield_now();
        }
    }

    #[test]
    fn process_lifecycle_events_do_not_stop_the_fleet() {
        use crate::{
            ComputerFileSystem, FileCapability, FileRights, FileSystemLimits, VirtualPath,
        };
        let limits = FileSystemLimits::testing();
        let home = VirtualPath::parse_utf8("/home", &limits).unwrap();
        let owner = FileCapability::new(home, FileRights::OWNER);
        let mut filesystem = ComputerFileSystem::with_limits(limits.clone());
        let child = crate::execution::fixtures::two_block_artifact(1, 1);
        let bytes = crate::test_encode::encode_artifact(child.decoded()).unwrap();
        let path = VirtualPath::parse_utf8("/home/child", &limits).unwrap();
        filesystem.write_file(&owner, &path, &bytes, true).unwrap();
        let parent = crate::execution::fixtures::process_v2_run_artifact(
            &"/home/child".encode_utf16().collect::<Vec<_>>(),
            &[0, 0],
        );
        let machine =
            ComputerMachine::start_in_filesystem(parent, profile(), &[], &[], filesystem, owner)
                .unwrap();
        let fleet = FleetScheduler::new(vec![(7, machine)], config()).unwrap();
        fleet.grant(7, 1, 512).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::ProcessEntered(1),
                ..
            }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::ProcessExited(1),
                ..
            }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(0))),
                ..
            }
        ));
    }

    #[test]
    fn replacement_preserves_process_lifecycle_and_resumes_parent() {
        use crate::{
            ComputerFileSystem, FileCapability, FileRights, FileSystemLimits, VirtualPath,
        };
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(
            VirtualPath::parse_utf8("/home", &limits).unwrap(),
            FileRights::OWNER,
        );
        let mut filesystem = ComputerFileSystem::with_limits(limits.clone());
        let launcher = crate::execution::fixtures::process_v3_exec_artifact(
            &"/home/replacement".encode_utf16().collect::<Vec<_>>(),
            &[0, 0],
        );
        let replacement = crate::execution::fixtures::process_v2_exit_artifact(37);
        for (name, artifact) in [
            ("/home/child", launcher),
            ("/home/replacement", replacement),
        ] {
            let bytes = crate::test_encode::encode_artifact(artifact.decoded()).unwrap();
            filesystem
                .write_file(
                    &owner,
                    &VirtualPath::parse_utf8(name, &limits).unwrap(),
                    &bytes,
                    true,
                )
                .unwrap();
        }
        let parent = crate::execution::fixtures::process_v2_run_artifact(
            &"/home/child".encode_utf16().collect::<Vec<_>>(),
            &[0, 0],
        );
        let machine =
            ComputerMachine::start_in_filesystem(parent, profile(), &[], &[], filesystem, owner)
                .unwrap();
        let fleet = FleetScheduler::new(vec![(7, machine)], config()).unwrap();
        fleet.grant(7, 1, 512).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::ProcessEntered(1),
                ..
            }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::ProcessReplaced {
                    previous: 1,
                    current: 2
                },
                ..
            }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::ProcessExited(2),
                ..
            }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(37))),
                ..
            }
        ));
    }

    #[test]
    fn terminating_child_cancels_its_pending_request_and_discards_late_reply() {
        use crate::{
            ComputerFileSystem, FileCapability, FileRights, FileSystemLimits, TerminalKey,
            TerminalKeyAction, TerminalKeyEvent, TerminalModifiers, VirtualPath,
        };
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(
            VirtualPath::parse_utf8("/home", &limits).unwrap(),
            FileRights::OWNER,
        );
        let mut filesystem = ComputerFileSystem::with_limits(limits.clone());
        let child = crate::execution::fixtures::capability_artifact(true, true, 1, 0);
        let bytes = crate::test_encode::encode_artifact(child.decoded()).unwrap();
        filesystem
            .write_file(
                &owner,
                &VirtualPath::parse_utf8("/home/child", &limits).unwrap(),
                &bytes,
                true,
            )
            .unwrap();
        let parent = crate::execution::fixtures::process_v2_run_artifact(
            &"/home/child".encode_utf16().collect::<Vec<_>>(),
            &[0, 0],
        );
        let ops = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let binding = CapabilityBinding::new("app", "entry", 1, 2, &ops);
        let machine = ComputerMachine::start_in_filesystem(
            parent,
            profile(),
            &[binding],
            &[],
            filesystem,
            owner,
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(7, machine)], config()).unwrap();
        fleet.grant(7, 1, 512).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::ProcessEntered(1),
                ..
            }
        ));
        let request = match next(&fleet) {
            FleetEvent::Requests { requests, .. } => requests[0].clone(),
            event => panic!("{event:?}"),
        };
        fleet
            .terminal_key(
                7,
                TerminalKeyEvent::new(
                    TerminalKey::C,
                    TerminalKeyAction::Press,
                    TerminalModifiers::new(TerminalModifiers::CONTROL).unwrap(),
                ),
            )
            .unwrap();
        match next(&fleet) {
            FleetEvent::ScopeStopped {
                machine: 7,
                scope: 1,
                pending,
            } => assert_eq!(pending, vec![(request.task_id, request.id)]),
            event => panic!("{event:?}"),
        }
        fleet
            .reply(7, request.task_id, request.id, ReplyValue::Unit)
            .unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::ProcessExited(1),
                ..
            }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(130))),
                ..
            }
        ));
    }

    #[test]
    fn machines_can_join_a_running_fleet_without_reusing_identities() {
        let fleet = FleetScheduler::new(vec![], config()).unwrap();
        let make = || {
            ComputerMachine::start(
                crate::execution::fixtures::two_block_artifact(1, 1),
                profile(),
                &[],
                &[],
            )
            .unwrap()
        };
        let first = fleet.attach(Box::new(make())).unwrap();
        fleet.grant(first, 1, 64).unwrap();
        assert!(
            matches!(next(&fleet), FleetEvent::Boundary { machine, outcome: ComputerAdvanceOutcome::Halted(_), .. } if machine == first)
        );
        fleet.stop(first).unwrap();
        assert!(matches!(next(&fleet), FleetEvent::Stopped { machine, .. } if machine == first));
        let second = fleet.attach(Box::new(make())).unwrap();
        assert_ne!(first, second);
        assert_eq!(fleet.grant(first, 2, 64), Err(SubmitError::UnknownMachine));
        fleet.grant(second, 2, 64).unwrap();
        assert!(
            matches!(next(&fleet), FleetEvent::Boundary { machine, outcome: ComputerAdvanceOutcome::Halted(_), .. } if machine == second)
        );
    }

    #[test]
    fn attach_refusal_returns_the_original_machine() {
        let fleet = FleetScheduler::new(vec![], config()).unwrap();
        let mut machine = ComputerMachine::start(
            crate::execution::fixtures::two_block_artifact(1, 1),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        machine.advance(1, 1, 1).unwrap();
        let (reason, mut returned) = fleet.attach(Box::new(machine)).unwrap_err();
        assert_eq!(reason, SubmitError::InvalidMachine);
        loop {
            match returned.advance(64, 64, 64).unwrap() {
                ComputerAdvanceOutcome::Halted(_) => break,
                ComputerAdvanceOutcome::SliceExhausted => {}
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn halted_machines_run_on_workers_with_bounded_credit() {
        let machines = (0..16)
            .map(|id| {
                (
                    id,
                    ComputerMachine::start(
                        crate::execution::fixtures::two_block_artifact(1, 1),
                        profile(),
                        &[],
                        &[],
                    )
                    .unwrap(),
                )
            })
            .collect();
        let fleet = FleetScheduler::new(machines, config()).unwrap();
        for id in 0..16 {
            fleet.grant(id, 1, 64).unwrap();
        }
        let mut seen = HashSet::new();
        for _ in 0..16 {
            match next(&fleet) {
                FleetEvent::Boundary {
                    machine,
                    outcome: ComputerAdvanceOutcome::Halted(_),
                    ..
                } => assert!(seen.insert(machine)),
                event => panic!("unexpected event {event:?}"),
            }
        }
        assert_eq!(seen.len(), 16);
        assert!(fleet.metrics().retired > 0);
        assert_eq!(fleet.grant(999, 1, 64), Err(SubmitError::UnknownMachine));
    }

    #[test]
    fn waiting_request_is_not_polled_on_each_tick_and_reply_resumes_it() {
        let ops = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let binding = CapabilityBinding::new("app", "entry", 1, 0, &ops);
        let machine = ComputerMachine::start(
            crate::execution::fixtures::two_unit_capability_calls_artifact(),
            profile(),
            &[binding],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(7, machine)], config()).unwrap();
        fleet.grant(7, 1, 64).unwrap();
        let first = match next(&fleet) {
            FleetEvent::Requests { requests, .. } => requests[0].clone(),
            event => panic!("{event:?}"),
        };
        wait_advances(&fleet, 2);
        for tick in 2..=5 {
            fleet.grant(7, tick, 64).unwrap();
        }
        thread::sleep(Duration::from_millis(10));
        assert_eq!(fleet.metrics().advances, 2);
        fleet
            .reply(7, first.task_id, first.id, ReplyValue::Unit)
            .unwrap();
        let second = match next(&fleet) {
            FleetEvent::Requests { requests, .. } => requests[0].clone(),
            event => panic!("{event:?}"),
        };
        assert_ne!(first.id, second.id);
        fleet
            .reply(7, second.task_id, second.id, ReplyValue::Unit)
            .unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
    }

    #[test]
    fn inspection_does_not_block_other_machines_and_stop_discards_late_cost() {
        let ops = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let binding = CapabilityBinding::new("app", "entry", 1, 0, &ops);
        let waiting = ComputerMachine::start(
            crate::execution::fixtures::two_unit_capability_calls_artifact(),
            profile(),
            &[binding],
            &[],
        )
        .unwrap();
        let independent = ComputerMachine::start(
            crate::execution::fixtures::two_block_artifact(1, 1),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new_with_request_inspections(
            vec![(7, waiting), (8, independent)],
            FleetConfig {
                workers: 1,
                event_capacity: 1,
                ..config()
            },
            vec![RequestInspection {
                namespace: "app".into(),
                name: "entry".into(),
                abi_major: 1,
                abi_minor: 0,
                operation: 0,
                over_budget_detail: "group exceeds budget".into(),
            }],
        )
        .unwrap();
        fleet.grant(7, 1, 64).unwrap();
        let request = match next(&fleet) {
            FleetEvent::Inspect { request, .. } => request,
            event => panic!("{event:?}"),
        };
        fleet.grant(8, 1, 64).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                machine: 8,
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
        fleet.stop(7).unwrap();
        match next(&fleet) {
            FleetEvent::Stopped { machine, pending } => {
                assert_eq!(machine, 7);
                assert_eq!(pending, vec![(request.task_id, request.id)]);
            }
            event => panic!("{event:?}"),
        }
        assert_eq!(
            fleet.resolve_cost(7, request.task_id, request.id, 1),
            Err(SubmitError::UnknownMachine)
        );
        thread::sleep(Duration::from_millis(10));
        assert!(fleet.try_event().is_none());
    }

    #[test]
    fn host_inspection_defers_admission_and_preserves_tick_budget() {
        let ops = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let binding = CapabilityBinding::new("app", "entry", 1, 0, &ops);
        let machine = ComputerMachine::start(
            crate::execution::fixtures::two_unit_capability_calls_artifact(),
            profile(),
            &[binding],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new_with_request_inspections(
            vec![(7, machine)],
            FleetConfig {
                host_budget: 3,
                ..config()
            },
            vec![RequestInspection {
                namespace: "app".into(),
                name: "entry".into(),
                abi_major: 1,
                abi_minor: 0,
                operation: 0,
                over_budget_detail: "group exceeds budget".into(),
            }],
        )
        .unwrap();
        fleet.grant(7, 1, 64).unwrap();
        let first = match next(&fleet) {
            FleetEvent::Inspect { request, .. } => request,
            event => panic!("{event:?}"),
        };
        assert_eq!(
            fleet.resolve_cost(7, first.task_id, first.id, 0),
            Err(SubmitError::InvalidCost)
        );
        wait_advances(&fleet, 2);
        assert!(fleet.try_event().is_none());
        fleet.resolve_cost(7, first.task_id, first.id, 2).unwrap();
        match next(&fleet) {
            FleetEvent::Requests { requests, .. } => assert_eq!(requests[0].id, first.id),
            event => panic!("{event:?}"),
        }
        fleet
            .reply(7, first.task_id, first.id, ReplyValue::Unit)
            .unwrap();
        let second = match next(&fleet) {
            FleetEvent::Inspect { request, .. } => request,
            event => panic!("{event:?}"),
        };
        fleet.resolve_cost(7, second.task_id, second.id, 2).unwrap();
        thread::sleep(Duration::from_millis(10));
        assert!(fleet.try_event().is_none());
        fleet.grant(7, 2, 64).unwrap();
        match next(&fleet) {
            FleetEvent::Requests { tick, requests, .. } => {
                assert_eq!(tick, 2);
                assert_eq!(requests[0].id, second.id);
            }
            event => panic!("{event:?}"),
        }
        fleet
            .reply(7, second.task_id, second.id, ReplyValue::Unit)
            .unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
    }

    #[test]
    fn request_larger_than_tick_budget_fails_without_dispatch() {
        let ops = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let binding = CapabilityBinding::new("app", "entry", 1, 0, &ops);
        let machine = ComputerMachine::start(
            crate::execution::fixtures::two_unit_capability_calls_artifact(),
            profile(),
            &[binding],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new_with_request_inspections(
            vec![(7, machine)],
            FleetConfig {
                host_budget: 3,
                ..config()
            },
            vec![RequestInspection {
                namespace: "app".into(),
                name: "entry".into(),
                abi_major: 1,
                abi_minor: 0,
                operation: 0,
                over_budget_detail: "group exceeds budget".into(),
            }],
        )
        .unwrap();
        fleet.grant(7, 1, 64).unwrap();
        let request = match next(&fleet) {
            FleetEvent::Inspect { request, .. } => request,
            event => panic!("{event:?}"),
        };
        fleet
            .resolve_cost(7, request.task_id, request.id, 4)
            .unwrap();
        match next(&fleet) {
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::UncaughtException(detail),
                ..
            } => assert!(detail.contains("group exceeds budget")),
            event => panic!("unexpected event {event:?}"),
        }
        assert_eq!(fleet.metrics().requests, 0);
    }

    #[test]
    fn deadline_prevents_execution_until_the_next_tick() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::two_block_artifact(1, 1),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(1, machine)], config()).unwrap();
        fleet.grant_until(1, 1, 64, Some(Instant::now())).unwrap();
        fleet.grant(1, 1, 64).unwrap();
        thread::sleep(Duration::from_millis(10));
        assert_eq!(fleet.metrics().advances, 0);
        assert!(fleet.try_event().is_none());
        fleet.grant(1, 2, 64).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
    }

    #[test]
    fn stopping_a_waiting_machine_reports_pending_and_discards_late_replies() {
        let ops = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let binding = CapabilityBinding::new("app", "entry", 1, 0, &ops);
        let machine = ComputerMachine::start(
            crate::execution::fixtures::two_unit_capability_calls_artifact(),
            profile(),
            &[binding],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(7, machine)], config()).unwrap();
        fleet.grant(7, 1, 64).unwrap();
        let request = match next(&fleet) {
            FleetEvent::Requests { requests, .. } => requests[0].clone(),
            event => panic!("{event:?}"),
        };
        fleet.stop(7).unwrap();
        match next(&fleet) {
            FleetEvent::Stopped { machine, pending } => {
                assert_eq!(machine, 7);
                assert_eq!(pending, vec![(request.task_id, request.id)]);
            }
            event => panic!("unexpected event {event:?}"),
        }
        let advances = fleet.metrics().advances;
        assert_eq!(
            fleet.reply(7, request.task_id, request.id, ReplyValue::Unit),
            Err(SubmitError::UnknownMachine)
        );
        assert_eq!(fleet.grant(7, 2, 64), Err(SubmitError::UnknownMachine));
        thread::sleep(Duration::from_millis(10));
        assert_eq!(fleet.metrics().advances, advances);
        assert!(fleet.try_event().is_none());
        assert_eq!(fleet.stop(99), Err(SubmitError::UnknownMachine));
    }

    #[test]
    fn timer_wakes_only_when_its_tick_arrives() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::timer_unit_artifact(2),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(1, machine)], config()).unwrap();
        fleet.grant(1, 1, 64).unwrap();
        wait_advances(&fleet, 2);
        fleet.grant(1, 2, 64).unwrap();
        thread::sleep(Duration::from_millis(10));
        assert_eq!(fleet.metrics().advances, 2);
        fleet.grant(1, 3, 64).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
    }

    #[test]
    fn independent_guest_tasks_can_wait_and_complete_out_of_order() {
        let ops = [
            OperationSchema::asynchronous(&[], HostValueType::I32),
            OperationSchema::asynchronous(&[], HostValueType::Unit),
        ];
        let binding = CapabilityBinding::new("app", "entry", 1, 0, &ops);
        let machine = ComputerMachine::start(
            crate::execution::fixtures::two_task_host_artifact(),
            profile(),
            &[binding],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(5, machine)], config()).unwrap();
        fleet.grant(5, 1, 64).unwrap();
        let mut pending = Vec::new();
        while pending.len() < 2 {
            match next(&fleet) {
                FleetEvent::Requests { requests, .. } => pending.extend(requests),
                event => panic!("unexpected event {event:?}"),
            }
        }
        assert_ne!(pending[0].task_id, pending[1].task_id);
        for request in pending.iter().rev() {
            let value = if request.operation == 0 {
                ReplyValue::I32(123)
            } else {
                ReplyValue::Unit
            };
            fleet.reply(5, request.task_id, request.id, value).unwrap();
        }
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
    }

    #[test]
    fn draining_a_full_event_queue_releases_waiting_workers() {
        let machines = (0..4)
            .map(|id| {
                (
                    id,
                    ComputerMachine::start(
                        crate::execution::fixtures::two_block_artifact(1, 1),
                        profile(),
                        &[],
                        &[],
                    )
                    .unwrap(),
                )
            })
            .collect();
        let fleet = FleetScheduler::new(
            machines,
            FleetConfig {
                workers: 4,
                event_capacity: 1,
                ..config()
            },
        )
        .unwrap();
        for id in 0..4 {
            fleet.grant(id, 1, 64).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while fleet.waiting_publishers.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "no publisher waited for the full queue"
            );
            thread::yield_now();
        }
        let mut completed = HashSet::new();
        for _ in 0..4 {
            match next(&fleet) {
                FleetEvent::Boundary {
                    machine,
                    outcome: ComputerAdvanceOutcome::Halted(_),
                    ..
                } => assert!(completed.insert(machine)),
                event => panic!("unexpected event: {event:?}"),
            }
        }
        assert_eq!(completed.len(), 4);
    }

    #[test]
    fn full_event_queue_does_not_block_shutdown() {
        let machines = (0..4)
            .map(|id| {
                (
                    id,
                    ComputerMachine::start(
                        crate::execution::fixtures::two_block_artifact(1, 1),
                        profile(),
                        &[],
                        &[],
                    )
                    .unwrap(),
                )
            })
            .collect();
        let fleet = FleetScheduler::new(
            machines,
            FleetConfig {
                event_capacity: 1,
                ..config()
            },
        )
        .unwrap();
        for id in 0..4 {
            fleet.grant(id, 1, 64).unwrap();
        }
        wait_advances(&fleet, 2);
        let start = Instant::now();
        drop(fleet);
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn advance_limit_is_reset_only_by_a_new_tick() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::empty_loop_artifact(1),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(
            vec![(1, machine)],
            FleetConfig {
                guest_budget: 1,
                maximum_advances_per_tick: 2,
                ..config()
            },
        )
        .unwrap();
        fleet.grant(1, 0, 100).unwrap();
        wait_advances(&fleet, 2);
        fleet.grant(1, 0, 100).unwrap();
        thread::sleep(Duration::from_millis(10));
        assert_eq!(fleet.metrics().advances, 2);
        fleet.grant(1, 1, 100).unwrap();
        wait_advances(&fleet, 4);
        assert_eq!(fleet.metrics().advances, 4);
    }

    #[test]
    fn repeated_tick_does_not_refill_instruction_credit() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::empty_loop_artifact(1),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(1, machine)], config()).unwrap();
        fleet.grant(1, 0, 1).unwrap();
        wait_advances(&fleet, 1);
        fleet.grant(1, 0, 1).unwrap();
        thread::sleep(Duration::from_millis(10));
        assert_eq!(fleet.metrics().retired, 1);
        fleet.grant(1, 1, 1).unwrap();
        wait_advances(&fleet, 2);
        assert_eq!(fleet.metrics().retired, 2);
    }
}
