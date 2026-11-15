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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{
    ComputerAdvanceOutcome, ComputerHostRequest, ComputerMachine, ComputerValue, HostResponse,
    HostValueInput,
};

#[derive(Clone, Copy, Debug)]
pub struct FleetConfig {
    pub workers: usize,
    pub mailbox_capacity: usize,
    pub event_capacity: usize,
    pub guest_budget: u32,
    pub maintenance_budget: u32,
    pub host_budget: u32,
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
}

impl ReplyValue {
    fn as_input(&self) -> HostValueInput<'_> {
        match self {
            Self::Unit => HostValueInput::Unit,
            Self::I32(v) => HostValueInput::I32(*v),
            Self::I64(v) => HostValueInput::I64(*v),
            Self::F32(v) => HostValueInput::F32(*v),
            Self::F64(v) => HostValueInput::F64(*v),
            Self::Bool(v) => HostValueInput::Bool(*v),
            Self::Char(v) => HostValueInput::Char(*v),
            Self::String(v) => HostValueInput::String(v),
            Self::Bytes(v) => HostValueInput::ByteArray(v),
        }
    }
}

#[derive(Debug)]
pub enum FleetEvent {
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
    Tick {
        machine: u64,
        tick: u64,
        retirements: u32,
    },
    Reply {
        machine: u64,
        task: u32,
        request: u64,
        value: ReplyValue,
    },
}

struct Actor {
    machine: ComputerMachine,
    tick: u64,
    tick_initialized: bool,
    remaining: u32,
    requests_left: u32,
    queued: bool,
    parked: bool,
    stopped: bool,
    pending: HashSet<(u32, u64)>,
    timers: HashMap<(u32, u64), u64>,
}

pub struct FleetScheduler {
    lanes: Vec<SyncSender<Command>>,
    owners: HashMap<u64, usize>,
    events: Receiver<FleetEvent>,
    stopped: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
    metrics: Arc<Metrics>,
}

impl FleetScheduler {
    pub fn new(
        machines: Vec<(u64, ComputerMachine)>,
        config: FleetConfig,
    ) -> Result<Self, &'static str> {
        if config.workers == 0
            || config.mailbox_capacity == 0
            || config.event_capacity == 0
            || config.guest_budget == 0
            || config.host_budget == 0
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
            partitions[lane].insert(
                id,
                Actor {
                    machine,
                    tick: 0,
                    tick_initialized: false,
                    remaining: 0,
                    requests_left: config.host_budget,
                    queued: false,
                    parked: false,
                    stopped: false,
                    pending: HashSet::new(),
                    timers: HashMap::new(),
                },
            );
        }
        let (event_tx, events) = mpsc::sync_channel(config.event_capacity);
        let stopped = Arc::new(AtomicBool::new(false));
        let metrics = Arc::new(Metrics::default());
        let mut lanes = Vec::new();
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        for (lane, actors) in partitions.into_iter().enumerate() {
            let (tx, rx) = mpsc::sync_channel(config.mailbox_capacity);
            lanes.push(tx);
            let events = event_tx.clone();
            let stop = Arc::clone(&stopped);
            let worker_metrics = Arc::clone(&metrics);
            let worker = match thread::Builder::new()
                .name(format!("experimental-vm-{lane}"))
                .spawn(move || run_worker(actors, rx, events, stop, worker_metrics, config))
            {
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
        Ok(Self {
            lanes,
            owners,
            events,
            stopped,
            workers,
            metrics,
        })
    }

    pub fn grant(&self, machine: u64, tick: u64, retirements: u32) -> Result<(), SubmitError> {
        self.send(
            machine,
            Command::Tick {
                machine,
                tick,
                retirements,
            },
        )
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

    fn send(&self, machine: u64, command: Command) -> Result<(), SubmitError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(SubmitError::Closed);
        }
        let lane = *self
            .owners
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
        if result.is_some() {
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

fn publish(sender: &SyncSender<FleetEvent>, stopped: &AtomicBool, mut event: FleetEvent) -> bool {
    loop {
        if stopped.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                event = returned;
                thread::park_timeout(Duration::from_millis(10));
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn ready(id: u64, actor: &mut Actor, queue: &mut VecDeque<u64>) {
    if !actor.queued && !actor.stopped && actor.remaining > 0 {
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
    config: FleetConfig,
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
                Command::Tick {
                    machine,
                    tick,
                    retirements,
                } => {
                    let actor = actors.get_mut(&machine).expect("owned machine");
                    if actor.tick_initialized && tick < actor.tick {
                        continue;
                    }
                    if !actor.tick_initialized || tick > actor.tick {
                        actor.tick_initialized = true;
                        actor.tick = tick;
                        actor.remaining = retirements;
                        actor.requests_left = config.host_budget;
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
                        if let Err(error) = actor.machine.resume_host_request_for(
                            task,
                            request,
                            HostResponse::Success(HostValueInput::Unit),
                        ) {
                            actor.stopped = true;
                            if !publish(
                                &events,
                                &stopped,
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
                    let actor = actors.get_mut(&machine).expect("owned machine");
                    if actor.timers.contains_key(&(task, request))
                        || !actor.pending.remove(&(task, request))
                    {
                        if !publish(
                            &events,
                            &stopped,
                            FleetEvent::Error {
                                machine,
                                message: "reply does not match a pending request".into(),
                            },
                        ) {
                            return;
                        }
                        continue;
                    }
                    if let Err(error) = actor.machine.resume_host_request_for(
                        task,
                        request,
                        HostResponse::Success(value.as_input()),
                    ) {
                        actor.stopped = true;
                        if !publish(
                            &events,
                            &stopped,
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
            }
        }
        let Some(id) = queue.pop_front() else {
            thread::park_timeout(Duration::from_millis(1));
            continue;
        };
        let actor = actors.get_mut(&id).expect("queued machine");
        actor.queued = false;
        if actor.stopped || actor.remaining == 0 {
            continue;
        }
        let (outcome, retired) = match actor.machine.advance_with_retirement_count(
            config.guest_budget,
            config.maintenance_budget,
            config.host_budget,
            actor.remaining,
        ) {
            Ok(value) => value,
            Err(error) => {
                actor.stopped = true;
                if !publish(
                    &events,
                    &stopped,
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
                    actor.requests_left -= 1;
                    actor.pending.insert(key);
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
            other => {
                actor.stopped = true;
                if !publish(
                    &events,
                    &stopped,
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
