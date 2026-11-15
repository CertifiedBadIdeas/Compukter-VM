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

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

mod event_queue;
use event_queue::{EventReceiver, EventSender as QueueSender};
mod output;
mod pending;
use output::WorkerOutput;
mod memory;
use memory::{ByteBudget, Reservation};
mod lifecycle;
use lifecycle::{CloseRequest, LifecycleCompletion, LifecycleCompletions};
pub use lifecycle::{
    FleetDetachCompletion, FleetDetachRejection, FleetDetachResult, FleetQuiescent,
    FleetStopCompletion,
};
pub use memory::{FleetMemoryLimits, FleetMemoryUsage};

static NEXT_FLEET_DEPLOYMENT_CANDIDATE: AtomicU64 = AtomicU64::new(1);
const MAXIMUM_PENDING_TIMERS: usize = 256;

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

/// Timer state transferred with an exclusively owned machine at detachment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FleetTimer {
    pub task: u32,
    pub request: u64,
    pub scope: u64,
    pub duration: u32,
    pub remaining: u64,
}

#[derive(Clone, Copy)]
struct ScheduledTimer {
    due: u64,
    duration: u32,
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

pub enum CompilationResult {
    Artifact(Box<[u8]>),
    Failure(Box<str>),
}

#[derive(Debug)]
pub enum FilesystemResult {
    Stat(crate::ComputerFileStat),
    List(crate::ComputerDirectoryListing),
    Read(crate::ComputerFileChunk),
}

/// Result of a deployment operation performed by the machine's owning worker.
#[derive(Debug)]
pub enum DeploymentResult {
    Prepared(u64),
    Revision(crate::ExecutableRevision),
    Installed(crate::ExecutableRevision),
    Discarded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeploymentError {
    Verification,
    Admission,
    InvalidCandidate,
    Capacity,
    WrongMachine,
    ProfileChanged,
    FileSystem(crate::FileSystemError),
}

enum DeploymentCommand {
    Prepare(Arc<[u8]>),
    Revision(Box<str>),
    Install {
        path: Box<str>,
        expected: crate::ExecutableRevision,
        candidate: u64,
    },
    Discard(u64),
}

#[derive(Debug)]
pub enum InputFailure {
    Terminal(crate::TerminalInputError),
    Canonical(crate::CanonicalLineSubmissionError),
}

enum FilesystemQuery {
    Stat(Box<str>),
    List {
        path: Box<str>,
        cursor: Option<Box<str>>,
        maximum: u32,
    },
    Read {
        path: Box<str>,
        offset: u64,
        maximum: u32,
        generation: u64,
    },
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
    Deployment {
        machine: u64,
        request: u64,
        result: Result<DeploymentResult, DeploymentError>,
    },
    InputResult {
        machine: u64,
        request: u64,
        result: Result<(), InputFailure>,
    },
    Filesystem {
        machine: u64,
        request: u64,
        result: Result<FilesystemResult, crate::ComputerFileReadError>,
    },
    ResourceSnapshot {
        machine: u64,
        request: u64,
        snapshot: crate::ComputerResourceSnapshot,
    },
    FilesystemGeneration {
        machine: u64,
        request: u64,
        generation: u64,
    },
    TerminalUpdate {
        machine: u64,
        request: u64,
        update: crate::TerminalUpdate,
    },
    /// Cumulative retirement count when this tick has no immediately runnable work.
    /// A host completion can make the same tick runnable again and produce another report.
    Quiescent {
        machine: u64,
        tick: u64,
        retired: u64,
        timers_pending: bool,
    },
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
    /// Ownership has returned to the receiver, with the VM state intact.
    Detached {
        machine: u64,
        computer: Box<ComputerMachine>,
        timers: Vec<FleetTimer>,
    },
    /// Detachment is only allowed after host requests have been settled.
    DetachRejected {
        machine: u64,
    },
    /// The machine was removed. Pending identities identify host actions that
    /// the embedder must cancel or discard; late replies are ignored.
    Stopped {
        machine: u64,
        pending: Vec<(u32, u64)>,
        /// Captured after accepted commands drain, immediately before destruction.
        filesystem_generation: u64,
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
    InvalidCompilation,
    InvalidInput,
    InvalidMachine,
    Capacity,
    IdentityExhausted,
}

impl FleetEvent {
    fn machine(&self) -> u64 {
        match self {
            Self::Deployment { machine, .. }
            | Self::InputResult { machine, .. }
            | Self::Filesystem { machine, .. }
            | Self::ResourceSnapshot { machine, .. }
            | Self::FilesystemGeneration { machine, .. }
            | Self::TerminalUpdate { machine, .. }
            | Self::Quiescent { machine, .. }
            | Self::Inspect { machine, .. }
            | Self::Requests { machine, .. }
            | Self::Boundary { machine, .. }
            | Self::Detached { machine, .. }
            | Self::DetachRejected { machine, .. }
            | Self::Stopped { machine, .. }
            | Self::ScopeStopped { machine, .. }
            | Self::Error { machine, .. } => *machine,
        }
    }
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

/// The checkpoint path settles external waits instead of requiring a host-side request mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetachMode {
    Quiescent,
    TimersAt(u64),
    CheckpointAt(u64),
}

impl DetachMode {
    fn timer_tick(self) -> Option<u64> {
        match self {
            Self::Quiescent => None,
            Self::TimersAt(tick) | Self::CheckpointAt(tick) => Some(tick),
        }
    }
}

enum Command {
    Deployment {
        machine: u64,
        request: u64,
        operation: DeploymentCommand,
    },
    Filesystem {
        machine: u64,
        request: u64,
        query: FilesystemQuery,
    },
    Add {
        machine: u64,
        computer: Box<ComputerMachine>,
        timers: Vec<FleetTimer>,
        base_tick: u64,
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
    Detach {
        machine: u64,
        mode: DetachMode,
    },
    TerminalKey {
        machine: u64,
        event: crate::TerminalKeyEvent,
        request: Option<u64>,
    },
    TerminalText {
        machine: u64,
        text: Box<str>,
        request: Option<u64>,
    },
    CanonicalLine {
        machine: u64,
        line: Box<[u16]>,
        request: Option<u64>,
    },
    TerminalUpdate {
        machine: u64,
        request: u64,
        base_revision: u64,
    },
    RedstoneInput {
        machine: u64,
        packet: u32,
    },
    RedstoneOutput {
        machine: u64,
        packed: u32,
    },
    ResourceSnapshot {
        machine: u64,
        request: u64,
    },
    FilesystemGeneration {
        machine: u64,
        request: u64,
    },
    Cost {
        machine: u64,
        task: u32,
        request: u64,
        units: u32,
    },
    CompilationComplete {
        machine: u64,
        token: u64,
        result: CompilationResult,
    },
}

impl Command {
    fn machine(&self) -> u64 {
        match self {
            Self::Deployment { machine, .. }
            | Self::Filesystem { machine, .. }
            | Self::Add { machine, .. }
            | Self::Tick { machine, .. }
            | Self::Reply { machine, .. }
            | Self::Stop { machine }
            | Self::Detach { machine, .. }
            | Self::TerminalKey { machine, .. }
            | Self::TerminalText { machine, .. }
            | Self::CanonicalLine { machine, .. }
            | Self::TerminalUpdate { machine, .. }
            | Self::RedstoneInput { machine, .. }
            | Self::RedstoneOutput { machine, .. }
            | Self::ResourceSnapshot { machine, .. }
            | Self::FilesystemGeneration { machine, .. }
            | Self::Cost { machine, .. }
            | Self::CompilationComplete { machine, .. } => *machine,
        }
    }
}

struct LaneMailbox {
    commands: VecDeque<QueuedCommand>,
    capacity: usize,
    memory: Arc<ByteBudget>,
    machine_limit: usize,
    machine_memory: HashMap<u64, Arc<ByteBudget>>,
    paused: HashMap<u64, Arc<AtomicBool>>,
    /// At most one entry per registered machine; registration is capped at 4096.
    closes: BTreeMap<u64, CloseRequest>,
}

struct QueuedCommand {
    command: Command,
    // Keep both reservations until execution finishes, including early returns.
    _memory: Option<(Reservation, Reservation)>,
}

impl LaneMailbox {
    #[cfg(test)]
    fn new(capacity: usize) -> Self {
        let limits = FleetMemoryLimits::default();
        Self::with_memory(
            capacity,
            ByteBudget::new(limits.command_bytes),
            limits.command_bytes_per_machine,
        )
    }

    fn with_memory(capacity: usize, memory: Arc<ByteBudget>, machine_limit: usize) -> Self {
        Self {
            commands: VecDeque::new(),
            capacity,
            memory,
            machine_limit,
            machine_memory: HashMap::new(),
            paused: HashMap::new(),
            closes: Default::default(),
        }
    }

    fn submit(&mut self, command: Command) -> Result<(), Command> {
        if self.commands.len() == self.capacity {
            return Err(command);
        }
        let Some(bytes) = memory::command_payload_bytes(&command)
            .and_then(|payload| payload.checked_add(std::mem::size_of::<QueuedCommand>()))
        else {
            return Err(command);
        };
        let Some(global) = self.memory.reserve(bytes) else {
            return Err(command);
        };
        let machine = command.machine();
        let local = match self.machine_memory.entry(machine) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.get().reserve(bytes),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let budget = ByteBudget::new(self.machine_limit);
                let reservation = budget.reserve(bytes);
                if reservation.is_some() {
                    entry.insert(budget);
                }
                reservation
            }
        };
        let Some(local) = local else {
            return Err(command);
        };
        self.commands.push_back(QueuedCommand {
            command,
            _memory: Some((global, local)),
        });
        Ok(())
    }

    #[cfg(test)]
    fn next(&mut self) -> Option<QueuedCommand> {
        self.next_for(None)
    }

    #[cfg(test)]
    fn next_for(&mut self, active: Option<u64>) -> Option<QueuedCommand> {
        self.next_eligible(active, |_| false)
    }

    fn next_eligible(
        &mut self,
        active: Option<u64>,
        blocked: impl Fn(u64) -> bool,
    ) -> Option<QueuedCommand> {
        self.paused
            .retain(|_, resume| !resume.load(Ordering::Acquire));
        let close = active
            .and_then(|id| self.closes.get_key_value(&id))
            .or_else(|| self.closes.first_key_value());
        if let Some((&machine, &close)) = close {
            if let Some(index) = self
                .commands
                .iter()
                .position(|queued| queued.command.machine() == machine)
            {
                return self.commands.remove(index);
            }
            self.closes.remove(&machine);
            return Some(QueuedCommand {
                command: close.command(machine),
                _memory: None,
            });
        }
        let index = self.eligible_index(blocked)?;
        self.commands.remove(index)
    }

    fn eligible_index(&self, blocked: impl Fn(u64) -> bool) -> Option<usize> {
        self.commands.iter().position(|queued| {
            let id = queued.command.machine();
            // Membership is stable under the lane lock for the entire scan.
            // Reading resume for each candidate could skip Add, observe a new
            // acknowledgement, then select a later command of that same VM.
            !self.paused.contains_key(&id) && !blocked(id)
        })
    }

    fn has_eligible(&mut self, blocked: impl Fn(u64) -> bool) -> bool {
        self.paused
            .retain(|_, resume| !resume.load(Ordering::Acquire));
        !self.closes.is_empty() || self.eligible_index(blocked).is_some()
    }
}

struct Actor {
    machine: ComputerMachine,
    tick: u64,
    tick_initialized: bool,
    remaining: u32,
    tick_retired: u64,
    reported_retired: Option<u64>,
    awaiting_grant: bool,
    compilation_token: Option<u64>,
    requests_left: u32,
    advances_left: u32,
    deadline: Option<Instant>,
    queued: bool,
    parked: bool,
    stopped: bool,
    detach_fence: Option<Arc<AtomicBool>>,
    pending: HashSet<(u32, u64)>,
    inspecting: HashSet<(u32, u64)>,
    scopes: Vec<u64>,
    request_scopes: HashMap<(u32, u64), u64>,
    inspected: HashMap<(u32, u64), u32>,
    timers: HashMap<(u32, u64), ScheduledTimer>,
    deployment_candidates: HashMap<u64, crate::DeploymentCandidate>,
    deployment_bytes: usize,
}

impl Actor {
    fn new(machine: ComputerMachine, host_budget: u32) -> Self {
        let scopes = machine.fleet_process_scopes();
        Self {
            machine,
            tick: 0,
            tick_initialized: false,
            remaining: 0,
            tick_retired: 0,
            reported_retired: None,
            awaiting_grant: false,
            compilation_token: None,
            requests_left: host_budget,
            advances_left: 0,
            deadline: None,
            queued: false,
            parked: false,
            stopped: false,
            detach_fence: None,
            pending: HashSet::new(),
            inspecting: HashSet::new(),
            scopes,
            request_scopes: HashMap::new(),
            inspected: HashMap::new(),
            timers: HashMap::new(),
            deployment_candidates: HashMap::new(),
            deployment_bytes: 0,
        }
    }
}

struct Owners {
    lanes: HashMap<u64, usize>,
    closing: HashMap<u64, LifecycleReader>,
    next_id: Option<u64>,
    next_lane: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LifecycleReader {
    Ordinary,
    Independent,
}

/// A generation-based notification that can be waited on independently of the fleet lock.
/// Capture a generation before draining the event queue, then wait for it to change.
#[derive(Default)]
pub struct FleetEventSignal {
    state: Mutex<(u64, bool)>,
    changed: Condvar,
}

impl FleetEventSignal {
    pub fn generation(&self) -> u64 {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0
    }

    /// Returns true when an event was published, false on timeout or fleet closure.
    pub fn wait(&self, generation: u64, timeout: Duration) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| state.0 == generation && !state.1)
            .unwrap_or_else(|e| e.into_inner());
        state.0 != generation
    }

    fn notify(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.0 = state.0.wrapping_add(1);
        self.changed.notify_all();
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.1 = true;
        self.changed.notify_all();
    }
}

#[derive(Clone)]
struct EventSender {
    sender: QueueSender,
    signal: Arc<FleetEventSignal>,
    completions: LifecycleCompletions,
    pending: Arc<pending::PendingCapacity>,
    detach_memory: Arc<ByteBudget>,
    detach_machine_limit: usize,
    detach_headroom: Arc<AtomicUsize>,
}

pub struct FleetScheduler {
    lanes: Vec<Arc<Mutex<LaneMailbox>>>,
    owners: Mutex<Owners>,
    events: EventReceiver,
    completions: LifecycleCompletions,
    event_signal: Arc<FleetEventSignal>,
    stopped: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
    metrics: Arc<Metrics>,
    waiting_publishers: Arc<AtomicUsize>,
    command_memory: Arc<ByteBudget>,
    pending: Arc<pending::PendingCapacity>,
    detach_memory: Arc<ByteBudget>,
    detach_headroom: Arc<AtomicUsize>,
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
        Self::new_with_coordinated_turns(machines, config, inspections, false)
    }

    /// With reporting enabled, every quiescent report ends the current turn. The host
    /// must grant again before a reply or input may execute Guest instructions.
    pub fn new_with_coordinated_turns(
        machines: Vec<(u64, ComputerMachine)>,
        config: FleetConfig,
        inspections: Vec<RequestInspection>,
        coordinated_turns: bool,
    ) -> Result<Self, &'static str> {
        Self::new_with_memory_limits(
            machines,
            config,
            inspections,
            coordinated_turns,
            FleetMemoryLimits::default(),
        )
    }

    pub fn new_with_memory_limits(
        machines: Vec<(u64, ComputerMachine)>,
        config: FleetConfig,
        inspections: Vec<RequestInspection>,
        coordinated_turns: bool,
        memory_limits: FleetMemoryLimits,
    ) -> Result<Self, &'static str> {
        if memory_limits.command_bytes == 0
            || memory_limits.command_bytes_per_machine == 0
            || memory_limits.pending_output_bytes == 0
            || memory_limits.pending_output_bytes_per_machine == 0
            || memory_limits.event_bytes == 0
            || memory_limits.event_bytes_per_machine == 0
            || memory_limits.detach_output_bytes == 0
            || memory_limits.detach_output_bytes_per_machine == 0
        {
            return Err("invalid fleet memory limits");
        }
        if machines.len() > 4096 {
            return Err("fleet size exceeds maximum");
        }
        if machines
            .iter()
            .any(|(_, machine)| !machine.fleet_attachable_with_timers(&[]))
        {
            return Err("machine has untracked external requests");
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
        let (event_tx, events) = event_queue::channel(
            config.event_capacity,
            memory_limits.event_bytes,
            memory_limits.event_bytes_per_machine,
        );
        let event_signal = Arc::new(FleetEventSignal::default());
        // At most one completion exists per registered machine; registration is capped.
        let completions = Arc::new(Mutex::new(BTreeMap::new()));
        let detach_memory = ByteBudget::new(memory_limits.detach_output_bytes);
        let detach_headroom = Arc::new(AtomicUsize::new(0));
        let pending = pending::PendingCapacity::new(memory_limits, config.event_capacity);
        let event_tx = EventSender {
            sender: event_tx,
            signal: Arc::clone(&event_signal),
            completions: Arc::clone(&completions),
            pending: Arc::clone(&pending),
            detach_memory: Arc::clone(&detach_memory),
            detach_machine_limit: memory_limits.detach_output_bytes_per_machine,
            detach_headroom: Arc::clone(&detach_headroom),
        };
        let stopped = Arc::new(AtomicBool::new(false));
        let metrics = Arc::new(Metrics::default());
        let waiting_publishers = Arc::new(AtomicUsize::new(0));
        let inspections = Arc::new(inspections);
        let mut lanes = Vec::new();
        let command_memory = ByteBudget::new(memory_limits.command_bytes);
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        for (lane, actors) in partitions.into_iter().enumerate() {
            let mailbox = Arc::new(Mutex::new(LaneMailbox::with_memory(
                config.mailbox_capacity,
                Arc::clone(&command_memory),
                memory_limits.command_bytes_per_machine,
            )));
            lanes.push(Arc::clone(&mailbox));
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
                        mailbox,
                        events,
                        stop,
                        worker_metrics,
                        waiting,
                        config,
                        inspections,
                        coordinated_turns,
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
                closing: HashMap::new(),
                next_id,
                next_lane,
            }),
            events,
            completions,
            event_signal,
            stopped,
            workers,
            metrics,
            waiting_publishers,
            command_memory,
            pending,
            detach_memory,
            detach_headroom,
        })
    }

    /// Takes ownership only after the bounded mailbox accepts the machine. A refusal returns it.
    /// Machines must have no pending or in-flight external host operations.
    pub fn attach(
        &self,
        computer: Box<ComputerMachine>,
    ) -> Result<u64, (SubmitError, Box<ComputerMachine>)> {
        self.attach_with_timers(computer, 0, Vec::new())
    }

    /// Restores timers only when every identity and duration matches the machine's pending requests.
    pub fn attach_with_timers(
        &self,
        computer: Box<ComputerMachine>,
        base_tick: u64,
        timers: Vec<FleetTimer>,
    ) -> Result<u64, (SubmitError, Box<ComputerMachine>)> {
        if timers.len() > MAXIMUM_PENDING_TIMERS
            || timers
                .iter()
                .any(|timer| base_tick.checked_add(timer.remaining).is_none())
            || !computer.fleet_attachable_with_timers(&timers)
        {
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
        if self.workers[lane].is_finished() {
            return Err((SubmitError::Closed, computer));
        }
        let mut mailbox = self.lanes[lane].lock().unwrap_or_else(|e| e.into_inner());
        match mailbox.submit(Command::Add {
            machine: id,
            computer,
            timers,
            base_tick,
        }) {
            Ok(()) => {
                owners.next_id = id.checked_add(1);
                owners.next_lane = (lane + 1) % self.lanes.len();
                owners.lanes.insert(id, lane);
                self.workers[lane].thread().unpark();
                Ok(id)
            }
            Err(Command::Add { computer, .. }) => Err((SubmitError::Full, computer)),
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
        self.send(
            machine,
            Command::TerminalKey {
                machine,
                event,
                request: None,
            },
        )
    }

    pub fn terminal_key_checked(
        &self,
        machine: u64,
        request: u64,
        event: crate::TerminalKeyEvent,
    ) -> Result<(), SubmitError> {
        self.send(
            machine,
            Command::TerminalKey {
                machine,
                event,
                request: Some(request),
            },
        )
    }

    pub fn terminal_text(&self, machine: u64, text: Box<str>) -> Result<(), SubmitError> {
        if text.chars().count() > 4096 {
            return Err(SubmitError::InvalidInput);
        }
        self.send(
            machine,
            Command::TerminalText {
                machine,
                text,
                request: None,
            },
        )
    }

    pub fn terminal_text_checked(
        &self,
        machine: u64,
        request: u64,
        text: Box<str>,
    ) -> Result<(), SubmitError> {
        if text.chars().count() > 4096 {
            return Err(SubmitError::InvalidInput);
        }
        self.send(
            machine,
            Command::TerminalText {
                machine,
                text,
                request: Some(request),
            },
        )
    }

    pub fn canonical_line(&self, machine: u64, line: Box<[u16]>) -> Result<(), SubmitError> {
        if line.len() > 4096 {
            return Err(SubmitError::InvalidInput);
        }
        self.send(
            machine,
            Command::CanonicalLine {
                machine,
                line,
                request: None,
            },
        )
    }

    pub fn canonical_line_checked(
        &self,
        machine: u64,
        request: u64,
        line: Box<[u16]>,
    ) -> Result<(), SubmitError> {
        if line.len() > 4096 {
            return Err(SubmitError::InvalidInput);
        }
        self.send(
            machine,
            Command::CanonicalLine {
                machine,
                line,
                request: Some(request),
            },
        )
    }

    /// Commits pending terminal writes and returns a delta or full state asynchronously.
    pub fn terminal_update(
        &self,
        machine: u64,
        request: u64,
        base_revision: u64,
    ) -> Result<(), SubmitError> {
        self.send(
            machine,
            Command::TerminalUpdate {
                machine,
                request,
                base_revision,
            },
        )
    }

    pub fn redstone_input(&self, machine: u64, packet: u32) -> Result<(), SubmitError> {
        if packet & !0x3fff_ffff != 0 {
            return Err(SubmitError::InvalidInput);
        }
        self.send(machine, Command::RedstoneInput { machine, packet })
    }

    pub fn confirm_redstone_output(&self, machine: u64, packed: u32) -> Result<(), SubmitError> {
        if packed & !0x3fff_ffff != 0 {
            return Err(SubmitError::InvalidInput);
        }
        self.send(machine, Command::RedstoneOutput { machine, packed })
    }

    /// Verify and retain a machine-bound candidate; all outcomes arrive as Deployment events.
    pub fn prepare_deployment(
        &self,
        machine: u64,
        request: u64,
        artifact: Arc<[u8]>,
    ) -> Result<(), SubmitError> {
        if artifact.len() > crate::ArtifactLimits::default().artifact_bytes {
            return Err(SubmitError::InvalidInput);
        }
        self.send(
            machine,
            Command::Deployment {
                machine,
                request,
                operation: DeploymentCommand::Prepare(artifact),
            },
        )
    }

    pub fn deployment_revision(
        &self,
        machine: u64,
        request: u64,
        path: Box<str>,
    ) -> Result<(), SubmitError> {
        if path.len() > 4096 {
            return Err(SubmitError::InvalidInput);
        }
        self.send(
            machine,
            Command::Deployment {
                machine,
                request,
                operation: DeploymentCommand::Revision(path),
            },
        )
    }

    pub fn install_deployment(
        &self,
        machine: u64,
        request: u64,
        path: Box<str>,
        expected: crate::ExecutableRevision,
        candidate: u64,
    ) -> Result<(), SubmitError> {
        if path.len() > 4096 || matches!(expected, crate::ExecutableRevision::Present(0)) {
            return Err(SubmitError::InvalidInput);
        }
        self.send(
            machine,
            Command::Deployment {
                machine,
                request,
                operation: DeploymentCommand::Install {
                    path,
                    expected,
                    candidate,
                },
            },
        )
    }

    pub fn discard_deployment(
        &self,
        machine: u64,
        request: u64,
        candidate: u64,
    ) -> Result<(), SubmitError> {
        self.send(
            machine,
            Command::Deployment {
                machine,
                request,
                operation: DeploymentCommand::Discard(candidate),
            },
        )
    }

    pub fn resource_snapshot(&self, machine: u64, request: u64) -> Result<(), SubmitError> {
        self.send(machine, Command::ResourceSnapshot { machine, request })
    }

    pub fn filesystem_generation(&self, machine: u64, request: u64) -> Result<(), SubmitError> {
        self.send(machine, Command::FilesystemGeneration { machine, request })
    }

    pub fn file_stat(&self, machine: u64, request: u64, path: Box<str>) -> Result<(), SubmitError> {
        if path.len() > 4096 {
            return Err(SubmitError::InvalidInput);
        }
        self.send(
            machine,
            Command::Filesystem {
                machine,
                request,
                query: FilesystemQuery::Stat(path),
            },
        )
    }

    pub fn file_list(
        &self,
        machine: u64,
        request: u64,
        path: Box<str>,
        cursor: Option<Box<str>>,
        maximum: u32,
    ) -> Result<(), SubmitError> {
        if path.len() > 4096
            || cursor.as_ref().is_some_and(|name| name.len() > 4096)
            || !(1..=256).contains(&maximum)
        {
            return Err(SubmitError::InvalidInput);
        }
        self.send(
            machine,
            Command::Filesystem {
                machine,
                request,
                query: FilesystemQuery::List {
                    path,
                    cursor,
                    maximum,
                },
            },
        )
    }

    pub fn file_read(
        &self,
        machine: u64,
        request: u64,
        path: Box<str>,
        offset: u64,
        maximum: u32,
        generation: u64,
    ) -> Result<(), SubmitError> {
        if path.len() > 4096 || !(1..=32768).contains(&maximum) {
            return Err(SubmitError::InvalidInput);
        }
        self.send(
            machine,
            Command::Filesystem {
                machine,
                request,
                query: FilesystemQuery::Read {
                    path,
                    offset,
                    maximum,
                    generation,
                },
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

    /// Returns the intact machine asynchronously after all pending requests settle.
    pub fn detach(&self, machine: u64) -> Result<(), SubmitError> {
        self.send_action(
            machine,
            Command::Detach {
                machine,
                mode: DetachMode::Quiescent,
            },
            Some(LifecycleReader::Ordinary),
        )
    }

    /// Atomically returns a machine and its timer state at the given simulation tick.
    pub fn detach_with_timers(&self, machine: u64, tick: u64) -> Result<(), SubmitError> {
        self.send_action(
            machine,
            Command::Detach {
                machine,
                mode: DetachMode::TimersAt(tick),
            },
            Some(LifecycleReader::Ordinary),
        )
    }

    /// Requests asynchronous removal. Completion is reported by `Stopped`.
    pub fn stop(&self, machine: u64) -> Result<(), SubmitError> {
        self.send_action(
            machine,
            Command::Stop { machine },
            Some(LifecycleReader::Ordinary),
        )
    }

    /// Select independent delivery atomically with the close fence, so an
    /// ordinary event pump cannot consume the completion before the first poll.
    pub fn stop_with_lifecycle_poll(&self, machine: u64) -> Result<(), SubmitError> {
        self.send_action(
            machine,
            Command::Stop { machine },
            Some(LifecycleReader::Independent),
        )
    }

    pub fn detach_with_lifecycle_poll(
        &self,
        machine: u64,
        preserve_timers_at: Option<u64>,
    ) -> Result<(), SubmitError> {
        self.send_action(
            machine,
            Command::Detach {
                machine,
                mode: preserve_timers_at.map_or(DetachMode::Quiescent, DetachMode::TimersAt),
            },
            Some(LifecycleReader::Independent),
        )
    }

    /// Drain accepted commands and return execution for checkpointing without running Guest.
    /// Unacknowledged external waits receive Unavailable; timers and compiler input survive.
    /// Completion and any undelivered prefix retain the existing bounded lifecycle ownership.
    pub fn detach_for_checkpoint(&self, machine: u64, tick: u64) -> Result<(), SubmitError> {
        self.send_action(
            machine,
            Command::Detach {
                machine,
                mode: DetachMode::CheckpointAt(tick),
            },
            Some(LifecycleReader::Independent),
        )
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

    pub fn complete_compilation(
        &self,
        machine: u64,
        token: u64,
        result: CompilationResult,
    ) -> Result<(), SubmitError> {
        let size = match &result {
            CompilationResult::Artifact(bytes) => bytes.len(),
            CompilationResult::Failure(detail) => detail.len(),
        };
        if token == 0 || size > crate::ArtifactLimits::default().artifact_bytes {
            return Err(SubmitError::InvalidCompilation);
        }
        self.send(
            machine,
            Command::CompilationComplete {
                machine,
                token,
                result,
            },
        )
    }

    fn send(&self, machine: u64, command: Command) -> Result<(), SubmitError> {
        self.send_action(machine, command, None)
    }

    fn send_action(
        &self,
        machine: u64,
        command: Command,
        reader: Option<LifecycleReader>,
    ) -> Result<(), SubmitError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(SubmitError::Closed);
        }
        let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        if owners.closing.contains_key(&machine) {
            return Err(SubmitError::UnknownMachine);
        }
        let lane = *owners
            .lanes
            .get(&machine)
            .ok_or(SubmitError::UnknownMachine)?;
        if self.workers[lane].is_finished() {
            return Err(SubmitError::Closed);
        }
        let mut mailbox = self.lanes[lane].lock().unwrap_or_else(|e| e.into_inner());
        let close = match command {
            Command::Stop { .. } => Some(CloseRequest::Stop),
            Command::Detach { mode, .. } => Some(CloseRequest::Detach { mode }),
            _ => None,
        };
        if let Some(close) = close {
            assert!(
                mailbox.closes.insert(machine, close).is_none(),
                "duplicate close request"
            );
        } else {
            mailbox.submit(command).map_err(|_| SubmitError::Full)?;
        }
        if let Some(reader) = reader {
            owners.closing.insert(machine, reader);
        }
        drop(mailbox);
        drop(owners);
        self.workers[lane].thread().unpark();
        Ok(())
    }

    pub fn try_event(&self) -> Option<FleetEvent> {
        let result = self.events.try_recv().ok().or_else(|| {
            let owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
            let mut completions = self.completions.lock().unwrap_or_else(|e| e.into_inner());
            let machine = *completions
                .iter()
                .find(|(machine, slot)| {
                    slot.unclaimed().is_some()
                        && owners.closing.get(machine) != Some(&LifecycleReader::Independent)
                })?
                .0;
            let std::collections::btree_map::Entry::Occupied(mut entry) =
                completions.entry(machine)
            else {
                unreachable!()
            };
            if let Some(event) = entry.get_mut().ordinary_observation() {
                return Some(event);
            }
            Some(entry.remove().into_completion().into_event(machine))
        });
        match &result {
            Some(FleetEvent::Stopped { machine, .. } | FleetEvent::Detached { machine, .. }) => {
                let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
                owners.lanes.remove(machine);
                owners.closing.remove(machine);
            }
            Some(FleetEvent::DetachRejected { machine }) => {
                self.owners
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .closing
                    .remove(machine);
            }
            _ => {}
        }
        if result.is_some()
            && (self.waiting_publishers.load(Ordering::SeqCst) != 0
                || matches!(
                    result,
                    Some(FleetEvent::DetachRejected { .. } | FleetEvent::Detached { .. })
                ))
        {
            for worker in &self.workers {
                worker.thread().unpark();
            }
        }
        result
    }

    /// Consume one machine's stop completion without draining ordinary events.
    /// The caller must cancel or disregard older events for this machine.
    pub fn try_stopped(&self, machine: u64) -> Option<FleetStopCompletion> {
        let completion = {
            let mut completions = self.completions.lock().unwrap_or_else(|e| e.into_inner());
            if !matches!(
                completions.get(&machine).and_then(|slot| slot.unclaimed()),
                Some(LifecycleCompletion::Stop(_))
            ) {
                return None;
            }
            let LifecycleCompletion::Stop(completion) =
                completions.remove(&machine)?.into_completion()
            else {
                unreachable!()
            };
            completion
        };
        let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        owners.lanes.remove(&machine);
        owners.closing.remove(&machine);
        Some(completion)
    }

    /// Consume detachment without draining the ordinary queue. Process the
    /// returned observations before resuming a rejected machine; older published
    /// events remain in the ordinary queue. Taking a result releases its fence.
    pub fn try_detached(&self, machine: u64) -> Option<FleetDetachCompletion> {
        let mut completion = {
            let mut completions = self.completions.lock().unwrap_or_else(|e| e.into_inner());
            if !matches!(
                completions.get(&machine).and_then(|slot| slot.unclaimed()),
                Some(LifecycleCompletion::Detach(_))
            ) {
                return None;
            }
            let LifecycleCompletion::Detach(completion) =
                completions.remove(&machine)?.into_completion()
            else {
                unreachable!()
            };
            completion
        };
        completion.release();
        let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        let lane = owners.lanes.get(&machine).copied();
        owners.closing.remove(&machine);
        if matches!(completion.result, FleetDetachResult::Detached { .. }) {
            owners.lanes.remove(&machine);
        }
        if let Some(lane) = lane {
            self.workers[lane].thread().unpark();
        }
        Some(completion)
    }

    /// Claims one machine's lifecycle stream, bypassing the ordinary event queue.
    /// Each returned event requires `consume_lifecycle_event` after successful
    /// delivery. Until then the slot, memory budget and ownership fence remain.
    /// Older already-published ordinary observations stay in the ordinary queue.
    pub fn poll_lifecycle_event(&self, machine: u64) -> Option<FleetEvent> {
        let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        *owners.closing.get_mut(&machine)? = LifecycleReader::Independent;
        self.completions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&machine)?
            .poll(machine)
    }

    /// Acknowledges exactly one event returned by `poll_lifecycle_event`.
    pub fn consume_lifecycle_event(&self, machine: u64) -> bool {
        let mut completions = self.completions.lock().unwrap_or_else(|e| e.into_inner());
        let Some(terminal) = completions
            .get_mut(&machine)
            .and_then(|slot| slot.acknowledge())
        else {
            return false;
        };
        if !terminal {
            return true;
        }
        let slot = completions
            .remove(&machine)
            .expect("acknowledged completion exists");
        drop(completions);
        let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        let lane = owners.lanes.get(&machine).copied();
        if slot.finish() {
            owners.lanes.remove(&machine);
        }
        owners.closing.remove(&machine);
        if let Some(lane) = lane {
            self.workers[lane].thread().unpark();
        }
        true
    }

    pub fn event_signal(&self) -> Arc<FleetEventSignal> {
        Arc::clone(&self.event_signal)
    }

    pub fn metrics(&self) -> FleetMetrics {
        FleetMetrics {
            advances: self.metrics.advances.load(Ordering::Relaxed),
            retired: self.metrics.retired.load(Ordering::Relaxed),
            requests: self.metrics.requests.load(Ordering::Relaxed),
        }
    }

    /// Ordinary queued observations only; worker in-flight output and lifecycle
    /// escrow are separate. Oversized delivery uses separately reported headroom.
    pub fn event_memory(&self) -> FleetMemoryUsage {
        self.events.memory()
    }

    /// All produced output waiting on workers: reserved batches plus at most
    /// one operation of excess headroom per worker.
    /// Includes reserved pending observations; do not add this to
    /// pending_output_memory. Admitted events and lifecycle escrow are separate.
    pub fn event_inflight_bytes(&self) -> usize {
        self.pending.inflight_bytes.load(Ordering::Acquire)
    }

    /// At most one oversized event may occupy the ordinary queue.
    pub fn event_headroom_bytes(&self) -> usize {
        self.events.headroom_bytes()
    }

    /// Excess produced output: at most one operation per worker, counted outside
    /// pending reservations. Nonzero bytes pause that worker, never other lanes.
    pub fn pending_output_headroom_bytes(&self) -> usize {
        self.pending.headroom_bytes.load(Ordering::Acquire)
    }

    pub fn pending_output_memory(&self) -> FleetMemoryUsage {
        self.pending.memory.snapshot()
    }

    /// Reserved pending observations; the shared limit is event_capacity.
    /// One in-flight operation per worker may exceed it before execution pauses.
    pub fn pending_output_entries(&self) -> usize {
        self.pending.entries.snapshot().retained_bytes
    }

    pub fn command_memory(&self) -> FleetMemoryUsage {
        self.command_memory.snapshot()
    }

    /// Unbudgeted in-flight output, bounded to one operation per worker.
    pub fn detach_headroom_bytes(&self) -> usize {
        self.detach_headroom.load(Ordering::Acquire)
    }

    pub fn detach_output_memory(&self) -> FleetMemoryUsage {
        self.detach_memory.snapshot()
    }
}

impl Drop for FleetScheduler {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.event_signal.close();
        for worker in &self.workers {
            worker.thread().unpark();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn publish(sender: &WorkerOutput<'_>, stopped: &AtomicBool, event: FleetEvent) -> bool {
    !stopped.load(Ordering::Acquire) && sender.emit(event)
}

fn ready(id: u64, actor: &mut Actor, queue: &mut VecDeque<u64>) {
    if !actor.queued
        && !actor.stopped
        && !actor.awaiting_grant
        && actor.compilation_token.is_none()
        && actor.remaining > 0
        && actor.advances_left > 0
    {
        actor.queued = true;
        actor.reported_retired = None;
        queue.push_back(id);
    }
}

fn remove_actor(
    actors: &mut HashMap<u64, Actor>,
    queue: &mut VecDeque<u64>,
    mailbox: &Mutex<LaneMailbox>,
    id: u64,
) -> Option<Actor> {
    let actor = actors.remove(&id)?;
    // Closing can complete without draining ordinary output. Stale ready entries
    // must not accumulate across repeated close/attach cycles under backpressure.
    queue.retain(|queued| *queued != id);
    mailbox
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .machine_memory
        .remove(&id);
    Some(actor)
}

fn fence_detach(
    actor: Option<&mut Actor>,
    mailbox: &mut LaneMailbox,
    machine: u64,
) -> Option<Arc<AtomicBool>> {
    let resume = Arc::new(AtomicBool::new(false));
    if let Some(actor) = actor {
        actor.detach_fence = Some(Arc::clone(&resume));
    }
    mailbox.paused.insert(machine, Arc::clone(&resume));
    Some(resume)
}

fn report_input(
    machine: u64,
    request: Option<u64>,
    result: Result<(), InputFailure>,
    operation: &str,
    events: &WorkerOutput<'_>,
    stopped: &AtomicBool,
) -> bool {
    match (request, result) {
        (Some(request), result) => publish(
            events,
            stopped,
            FleetEvent::InputResult {
                machine,
                request,
                result,
            },
        ),
        (None, Err(error)) => {
            let detail = match error {
                InputFailure::Terminal(error) => format!("{error:?}"),
                InputFailure::Canonical(error) => format!("{error:?}"),
            };
            publish(
                events,
                stopped,
                FleetEvent::Error {
                    machine,
                    message: format!("{operation}: {detail}"),
                },
            )
        }
        (None, Ok(())) => true,
    }
}

fn report_quiescent(
    id: u64,
    actor: &mut Actor,
    events: &WorkerOutput<'_>,
    stopped: &AtomicBool,
    enabled: bool,
) -> bool {
    if !enabled || !actor.tick_initialized || actor.reported_retired == Some(actor.tick_retired) {
        return true;
    }
    actor.reported_retired = Some(actor.tick_retired);
    actor.awaiting_grant = true;
    publish(
        events,
        stopped,
        FleetEvent::Quiescent {
            machine: id,
            tick: actor.tick,
            retired: actor.tick_retired,
            timers_pending: !actor.timers.is_empty(),
        },
    )
}

fn run_worker(
    mut actors: HashMap<u64, Actor>,
    mailbox: Arc<Mutex<LaneMailbox>>,
    events: EventSender,
    stopped: Arc<AtomicBool>,
    metrics: Arc<Metrics>,
    waiters: Arc<AtomicUsize>,
    config: FleetConfig,
    inspections: Arc<Vec<RequestInspection>>,
    coordinated_turns: bool,
) {
    let events = WorkerOutput::new(events, &waiters);
    let mut queue = VecDeque::new();
    while !stopped.load(Ordering::Acquire) {
        if !events.flush() {
            return;
        }
        for _ in 0..4 {
            let queued = {
                let mut mailbox = mailbox.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(machine) = events.overflowed_detach() {
                    mailbox.closes.remove(&machine);
                    events.complete_detach(
                        machine,
                        FleetDetachResult::Rejected(FleetDetachRejection::OutputCapacity),
                        fence_detach(actors.get_mut(&machine), &mut mailbox, machine),
                    );
                }
                if events.blocks_worker() && mailbox.closes.is_empty() {
                    break;
                }
                let active = events.closing_machine();
                let close = active
                    .and_then(|id| mailbox.closes.get_key_value(&id))
                    .or_else(|| mailbox.closes.first_key_value())
                    .map(|(&machine, &close)| (machine, close));
                if let Some((machine, close)) = close {
                    if !events.begin_closing(machine, close) {
                        mailbox.closes.remove(&machine);
                        events.reject_detach(
                            machine,
                            fence_detach(actors.get_mut(&machine), &mut mailbox, machine),
                        );
                        continue;
                    }
                    if events.overflowed_detach().is_some() {
                        continue;
                    }
                }
                let Some(queued) = mailbox.next_eligible(active, |id| events.blocks_machine(id))
                else {
                    break;
                };
                queued
            };
            let QueuedCommand { command, _memory } = queued;
            match command {
                Command::Deployment {
                    machine,
                    request,
                    operation,
                } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    let result = match operation {
                        DeploymentCommand::Prepare(bytes) => {
                            if actor.deployment_candidates.len() >= 4
                                || actor.deployment_bytes.saturating_add(bytes.len())
                                    > crate::ArtifactLimits::default().artifact_bytes
                            {
                                Err(DeploymentError::Capacity)
                            } else {
                                match actor.machine.verify_for_deploy(bytes) {
                                    Ok(candidate) => {
                                        match NEXT_FLEET_DEPLOYMENT_CANDIDATE.fetch_update(
                                            Ordering::Relaxed,
                                            Ordering::Relaxed,
                                            |value| (value < i64::MAX as u64).then_some(value + 1),
                                        ) {
                                            Ok(token) => {
                                                actor.deployment_bytes += candidate.bytes.len();
                                                actor
                                                    .deployment_candidates
                                                    .insert(token, candidate);
                                                Ok(DeploymentResult::Prepared(token))
                                            }
                                            Err(_) => Err(DeploymentError::Capacity),
                                        }
                                    }
                                    Err(crate::HostVerifyError::Artifact(_)) => {
                                        Err(DeploymentError::Verification)
                                    }
                                    Err(crate::HostVerifyError::Admission(_)) => {
                                        Err(DeploymentError::Admission)
                                    }
                                }
                            }
                        }
                        DeploymentCommand::Revision(path) => actor
                            .machine
                            .parse_inspection_path(&path)
                            .and_then(|path| actor.machine.executable_revision(&path))
                            .map(DeploymentResult::Revision)
                            .map_err(DeploymentError::FileSystem),
                        DeploymentCommand::Install {
                            path,
                            expected,
                            candidate,
                        } => match actor.machine.parse_inspection_path(&path) {
                            Err(error) => Err(DeploymentError::FileSystem(error)),
                            Ok(path) => match actor.deployment_candidates.remove(&candidate) {
                                None => Err(DeploymentError::InvalidCandidate),
                                Some(value) => {
                                    let size = value.bytes.len();
                                    match actor.machine.deploy(&path, expected, value) {
                                        Ok(revision) => {
                                            actor.deployment_bytes -= size;
                                            Ok(DeploymentResult::Installed(revision))
                                        }
                                        Err(failure) => {
                                            let error = match failure.error() {
                                                crate::HostDeployError::WrongMachine => {
                                                    DeploymentError::WrongMachine
                                                }
                                                crate::HostDeployError::ProfileChanged => {
                                                    DeploymentError::ProfileChanged
                                                }
                                                crate::HostDeployError::FileSystem(error) => {
                                                    DeploymentError::FileSystem(error)
                                                }
                                            };
                                            actor
                                                .deployment_candidates
                                                .insert(candidate, failure.into_candidate());
                                            Err(error)
                                        }
                                    }
                                }
                            },
                        },
                        DeploymentCommand::Discard(candidate) => {
                            match actor.deployment_candidates.remove(&candidate) {
                                None => Err(DeploymentError::InvalidCandidate),
                                Some(value) => {
                                    actor.deployment_bytes -= value.bytes.len();
                                    Ok(DeploymentResult::Discarded)
                                }
                            }
                        }
                    };
                    if !publish(
                        &events,
                        &stopped,
                        FleetEvent::Deployment {
                            machine,
                            request,
                            result,
                        },
                    ) {
                        return;
                    }
                }
                Command::Filesystem {
                    machine,
                    request,
                    query,
                } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    let result = match query {
                        FilesystemQuery::Stat(path) => actor
                            .machine
                            .parse_inspection_path(&path)
                            .and_then(|path| actor.machine.filesystem_stat(&path))
                            .map(FilesystemResult::Stat)
                            .map_err(Into::into),
                        FilesystemQuery::List {
                            path,
                            cursor,
                            maximum,
                        } => actor
                            .machine
                            .parse_inspection_path(&path)
                            .map_err(crate::ComputerFileReadError::from)
                            .and_then(|path| {
                                actor
                                    .machine
                                    .filesystem_list(&path, cursor.as_deref(), maximum)
                            })
                            .map(FilesystemResult::List),
                        FilesystemQuery::Read {
                            path,
                            offset,
                            maximum,
                            generation,
                        } => actor
                            .machine
                            .parse_inspection_path(&path)
                            .map_err(crate::ComputerFileReadError::from)
                            .and_then(|path| {
                                actor
                                    .machine
                                    .filesystem_read(&path, offset, maximum, generation)
                            })
                            .map(FilesystemResult::Read),
                    };
                    if !publish(
                        &events,
                        &stopped,
                        FleetEvent::Filesystem {
                            machine,
                            request,
                            result,
                        },
                    ) {
                        return;
                    }
                }
                Command::Add {
                    machine,
                    computer,
                    timers,
                    base_tick,
                } => {
                    let mut actor = Actor::new(*computer, config.host_budget);
                    for timer in timers {
                        let key = (timer.task, timer.request);
                        actor.pending.insert(key);
                        actor.request_scopes.insert(key, timer.scope);
                        actor.timers.insert(
                            key,
                            ScheduledTimer {
                                due: base_tick.saturating_add(timer.remaining),
                                duration: timer.duration,
                            },
                        );
                    }
                    actor.tick = base_tick;
                    assert!(actors.insert(machine, actor).is_none());
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
                    if tick < actor.tick {
                        continue;
                    }
                    if !actor.tick_initialized || tick > actor.tick {
                        if !report_quiescent(machine, actor, &events, &stopped, coordinated_turns) {
                            return;
                        }
                        actor.tick_initialized = true;
                        actor.tick = tick;
                        actor.remaining = retirements;
                        actor.tick_retired = 0;
                        actor.reported_retired = None;
                        actor.requests_left = config.host_budget;
                        actor.advances_left = config.maximum_advances_per_tick;
                        actor.deadline = deadline;
                    }
                    actor.awaiting_grant = false;
                    actor.reported_retired = None;
                    let mut due: Vec<_> = actor
                        .timers
                        .iter()
                        .filter_map(|(key, timer)| (timer.due <= tick).then_some(*key))
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
                    if !actor.queued
                        && !report_quiescent(machine, actor, &events, &stopped, coordinated_turns)
                    {
                        return;
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
                Command::Detach { machine, mode } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    let preserve_timers_at = mode.timer_tick();
                    let checkpoint = matches!(mode, DetachMode::CheckpointAt(_));
                    // Validate before settlement: a rejected request must not change execution.
                    if preserve_timers_at.is_some_and(|tick| tick < actor.tick)
                        || actor.timers.len() > MAXIMUM_PENDING_TIMERS
                        || (!checkpoint
                            && (actor.pending.iter().any(|key| {
                                preserve_timers_at.is_none() || !actor.timers.contains_key(key)
                            }) || !actor.deployment_candidates.is_empty()
                                || !actor.inspecting.is_empty()
                                || !actor.inspected.is_empty()
                                || actor.compilation_token.is_some()))
                    {
                        events.complete_detach(
                            machine,
                            FleetDetachResult::Rejected(FleetDetachRejection::Busy),
                            fence_detach(
                                Some(actor),
                                &mut mailbox.lock().unwrap_or_else(|e| e.into_inner()),
                                machine,
                            ),
                        );
                        continue;
                    }
                    if checkpoint {
                        // Use the machine's full route table, including requests not yet published
                        // or waiting for cost inspection. No Guest instructions execute here.
                        let timers = &actor.timers;
                        if let Err(error) =
                            actor
                                .machine
                                .settle_fleet_checkpoint_requests(|task, request| {
                                    timers.contains_key(&(task, request))
                                })
                        {
                            actor.stopped = true;
                            if !publish(
                                &events,
                                &stopped,
                                FleetEvent::Error {
                                    machine,
                                    message: format!("checkpoint request settlement: {error:?}"),
                                },
                            ) {
                                return;
                            }
                            events.complete_detach(
                                machine,
                                FleetDetachResult::Rejected(FleetDetachRejection::Busy),
                                fence_detach(
                                    Some(actor),
                                    &mut mailbox.lock().unwrap_or_else(|e| e.into_inner()),
                                    machine,
                                ),
                            );
                            continue;
                        }
                        actor.pending.retain(|key| actor.timers.contains_key(key));
                        actor
                            .request_scopes
                            .retain(|key, _| actor.timers.contains_key(key));
                        actor.inspecting.clear();
                        actor.inspected.clear();
                        actor.deployment_candidates.clear();
                        actor.deployment_bytes = 0;
                        // The computer retains the exact compilation transaction for checkpoint replay.
                    }
                    if !report_quiescent(machine, actor, &events, &stopped, coordinated_turns) {
                        return;
                    }
                    let actor = remove_actor(&mut actors, &mut queue, &mailbox, machine)
                        .expect("checked actor");
                    let mut timers: Vec<_> = actor
                        .timers
                        .iter()
                        .map(|(&(task, request), timer)| FleetTimer {
                            task,
                            request,
                            scope: actor.request_scopes[&(task, request)],
                            duration: timer.duration,
                            remaining: timer
                                .due
                                .saturating_sub(preserve_timers_at.expect("timers require a tick")),
                        })
                        .collect();
                    timers.sort_unstable_by_key(|timer| (timer.task, timer.request));
                    events.complete_detach(
                        machine,
                        FleetDetachResult::Detached {
                            computer: Box::new(actor.machine),
                            timers,
                        },
                        None,
                    );
                }
                Command::Stop { machine } => {
                    if let Some(actor) = remove_actor(&mut actors, &mut queue, &mailbox, machine) {
                        let quiescent = if coordinated_turns && actor.tick_initialized {
                            Some(FleetQuiescent {
                                tick: actor.tick,
                                retired: actor.tick_retired,
                                timers_pending: !actor.timers.is_empty(),
                            })
                        } else {
                            None
                        };
                        let report_pending = quiescent.is_some()
                            && actor.reported_retired != Some(actor.tick_retired);
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
                        let filesystem_generation = actor.machine.filesystem_generation();
                        drop(actor);
                        events.complete_stop(FleetStopCompletion {
                            machine,
                            pending,
                            filesystem_generation,
                            quiescent,
                            prior_reports: VecDeque::new(),
                            report_pending,
                        });
                    }
                }
                Command::TerminalKey {
                    machine,
                    event,
                    request,
                } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    let result = actor
                        .machine
                        .submit_terminal_key(event)
                        .map_err(InputFailure::Terminal);
                    if result.is_ok() {
                        actor.parked = false;
                        ready(machine, actor, &mut queue);
                    }
                    if !report_input(machine, request, result, "terminal key", &events, &stopped) {
                        return;
                    }
                }
                Command::TerminalText {
                    machine,
                    text,
                    request,
                } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    let result = actor
                        .machine
                        .terminal_mut()
                        .push_text(&text)
                        .map_err(InputFailure::Terminal);
                    if result.is_ok() {
                        actor.parked = false;
                        ready(machine, actor, &mut queue);
                    }
                    if !report_input(machine, request, result, "terminal text", &events, &stopped) {
                        return;
                    }
                }
                Command::CanonicalLine {
                    machine,
                    line,
                    request,
                } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    let result = actor
                        .machine
                        .submit_canonical_line(&line)
                        .map_err(InputFailure::Canonical);
                    if result.is_ok() {
                        actor.parked = false;
                        ready(machine, actor, &mut queue);
                    }
                    if !report_input(
                        machine,
                        request,
                        result,
                        "canonical line",
                        &events,
                        &stopped,
                    ) {
                        return;
                    }
                }
                Command::TerminalUpdate {
                    machine,
                    request,
                    base_revision,
                } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    actor.machine.terminal_mut().commit();
                    let update = actor.machine.terminal().changes_since(base_revision);
                    if !publish(
                        &events,
                        &stopped,
                        FleetEvent::TerminalUpdate {
                            machine,
                            request,
                            update,
                        },
                    ) {
                        return;
                    }
                }
                Command::RedstoneInput { machine, packet } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    if let Err(error) = actor.machine.submit_redstone_input(packet) {
                        if !publish(
                            &events,
                            &stopped,
                            FleetEvent::Error {
                                machine,
                                message: format!("redstone input: {error:?}"),
                            },
                        ) {
                            return;
                        }
                    } else {
                        actor.parked = false;
                        ready(machine, actor, &mut queue);
                    }
                }
                Command::RedstoneOutput { machine, packed } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    if let Err(error) = actor.machine.confirm_redstone_output(packed) {
                        if !publish(
                            &events,
                            &stopped,
                            FleetEvent::Error {
                                machine,
                                message: format!("redstone output: {error:?}"),
                            },
                        ) {
                            return;
                        }
                    }
                }
                Command::ResourceSnapshot { machine, request } => {
                    let Some(actor) = actors.get(&machine) else {
                        continue;
                    };
                    if !publish(
                        &events,
                        &stopped,
                        FleetEvent::ResourceSnapshot {
                            machine,
                            request,
                            snapshot: actor.machine.resource_snapshot(),
                        },
                    ) {
                        return;
                    }
                }
                Command::FilesystemGeneration { machine, request } => {
                    let Some(actor) = actors.get(&machine) else {
                        continue;
                    };
                    if !publish(
                        &events,
                        &stopped,
                        FleetEvent::FilesystemGeneration {
                            machine,
                            request,
                            generation: actor.machine.filesystem_generation(),
                        },
                    ) {
                        return;
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
                Command::CompilationComplete {
                    machine,
                    token,
                    result,
                } => {
                    let Some(actor) = actors.get_mut(&machine) else {
                        continue;
                    };
                    if actor.compilation_token != Some(token) {
                        continue;
                    }
                    actor.compilation_token = None;
                    let outcome = match result {
                        CompilationResult::Artifact(bytes) => {
                            actor.machine.complete_compilation_success(token, &bytes)
                        }
                        CompilationResult::Failure(detail) => {
                            actor.machine.complete_compilation_failure(token, &detail)
                        }
                    };
                    if let Err(error) = outcome {
                        actor.stopped = true;
                        if !publish(
                            &events,
                            &stopped,
                            FleetEvent::Error {
                                machine,
                                message: format!("compilation completion: {error:?}"),
                            },
                        ) {
                            return;
                        }
                    } else {
                        actor.parked = false;
                        ready(machine, actor, &mut queue);
                    }
                }
            }
        }
        // A stop fences further Guest execution until its already-accepted
        // commands have been processed and the actor has been removed.
        if !mailbox
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .closes
            .is_empty()
        {
            continue;
        }
        if events.blocks_worker() {
            thread::park_timeout(Duration::from_millis(1));
            continue;
        }
        let ready_index = queue.iter().position(|id| !events.blocks_machine(*id));
        let Some(id) = ready_index.and_then(|index| queue.remove(index)) else {
            if !mailbox
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .has_eligible(|id| events.blocks_machine(id))
            {
                thread::park_timeout(Duration::from_millis(1));
            }
            continue;
        };
        let Some(actor) = actors.get_mut(&id) else {
            continue;
        };
        actor.queued = false;
        if let Some(fence) = &actor.detach_fence {
            if !fence.load(Ordering::Acquire) {
                actor.queued = true;
                queue.push_back(id);
                if queue.iter().all(|id| {
                    events.blocks_machine(*id)
                        || actors.get(id).is_none_or(|actor| {
                            actor
                                .detach_fence
                                .as_ref()
                                .is_some_and(|fence| !fence.load(Ordering::Acquire))
                        })
                }) && !mailbox
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .has_eligible(|id| events.blocks_machine(id))
                {
                    thread::park_timeout(Duration::from_millis(1));
                }
                continue;
            }
            actor.detach_fence = None;
        }
        if actor.stopped || actor.remaining == 0 || actor.advances_left == 0 {
            if !report_quiescent(id, actor, &events, &stopped, coordinated_turns) {
                return;
            }
            continue;
        }
        if actor
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            if !report_quiescent(id, actor, &events, &stopped, coordinated_turns) {
                return;
            }
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
        actor.tick_retired += retired;
        actor.remaining = actor.remaining.saturating_sub(retired as u32);
        match outcome {
            ComputerAdvanceOutcome::CompilationRequested(request) => {
                actor.compilation_token = Some(request.token);
                actor.parked = true;
                if !publish(
                    &events,
                    &stopped,
                    FleetEvent::Boundary {
                        machine: id,
                        tick: actor.tick,
                        outcome: ComputerAdvanceOutcome::CompilationRequested(request),
                    },
                ) {
                    return;
                }
            }
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
            ComputerAdvanceOutcome::WaitingForTerminalEvent => {
                actor.parked = true;
                if !publish(
                    &events,
                    &stopped,
                    FleetEvent::Boundary {
                        machine: id,
                        tick: actor.tick,
                        outcome: ComputerAdvanceOutcome::WaitingForTerminalEvent,
                    },
                ) {
                    return;
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
                                FleetEvent::Error {
                                    machine: id,
                                    message: format!("host quota failure: {error:?}"),
                                },
                            ) {
                                return;
                            }
                            break;
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
                            [ComputerValue::I32(_)]
                                if actor.timers.len() >= MAXIMUM_PENDING_TIMERS =>
                            {
                                actor.pending.remove(&key);
                                actor.request_scopes.remove(&key);
                                if let Err(error) = actor.machine.resume_host_request_for(
                                    request.task_id,
                                    request.id,
                                    HostResponse::Failure(HostFailure::new(
                                        HostFailureKind::Unavailable,
                                        "Timer pending request limit was reached",
                                    )),
                                ) {
                                    actor.stopped = true;
                                    if !publish(
                                        &events,
                                        &stopped,
                                        FleetEvent::Error {
                                            machine: id,
                                            message: format!("timer rejection: {error:?}"),
                                        },
                                    ) {
                                        return;
                                    }
                                }
                            }
                            [ComputerValue::I32(delay)] if *delay >= 0 => {
                                actor.timers.insert(
                                    key,
                                    ScheduledTimer {
                                        due: actor.tick.saturating_add((*delay as u64).max(1)),
                                        duration: *delay as u32,
                                    },
                                );
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
                    if actor.stopped {
                        break;
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
        if !actor.queued && !report_quiescent(id, actor, &events, &stopped, coordinated_turns) {
            return;
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
            if Instant::now() >= limit {
                let lanes: Vec<_> = fleet
                    .lanes
                    .iter()
                    .map(|lane| {
                        let lane = lane.lock().unwrap();
                        (
                            lane.commands
                                .iter()
                                .map(|q| q.command.machine())
                                .collect::<Vec<_>>(),
                            lane.paused
                                .iter()
                                .map(|(id, resume)| (*id, resume.load(Ordering::Acquire)))
                                .collect::<Vec<_>>(),
                            lane.closes.keys().copied().collect::<Vec<_>>(),
                        )
                    })
                    .collect();
                panic!("timed out waiting for scheduler event: {:?}; lanes={lanes:?}; pending={} headroom={} detach_headroom={}",
                    fleet.metrics(), fleet.event_inflight_bytes(), fleet.pending_output_headroom_bytes(), fleet.detach_headroom_bytes());
            }
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

    fn wait_lifecycle(fleet: &FleetScheduler, machine: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fleet.completions.lock().unwrap().contains_key(&machine) {
            assert!(Instant::now() < deadline, "lifecycle completion blocked");
            thread::yield_now();
        }
    }

    fn saturated_fleet(memory_limits: FleetMemoryLimits, host_request: bool) -> FleetScheduler {
        let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
        let machines = (0..2)
            .map(|id| {
                let (artifact, bindings) = if id == 1 && host_request {
                    (
                        crate::execution::fixtures::two_unit_capability_calls_artifact(),
                        vec![binding],
                    )
                } else {
                    (
                        crate::execution::fixtures::two_block_artifact(1, 1),
                        Vec::new(),
                    )
                };
                (
                    id,
                    ComputerMachine::start(artifact, profile(), &bindings, &[]).unwrap(),
                )
            })
            .collect();
        let fleet = FleetScheduler::new_with_memory_limits(
            machines,
            FleetConfig {
                workers: 1,
                event_capacity: 1,
                mailbox_capacity: 2,
                ..config()
            },
            Vec::new(),
            false,
            // These lifecycle tests require a fully stalled worker, rather than
            // merely one backpressured VM. Make that pressure explicit.
            FleetMemoryLimits {
                pending_output_bytes: 1,
                ..memory_limits
            },
        )
        .unwrap();
        fleet.grant(0, 1, 64).unwrap();
        fleet.grant(1, 1, 64).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fleet.waiting_publishers.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "output did not saturate");
            thread::yield_now();
        }
        fleet
    }

    fn two_machine_output_fleet(config: FleetConfig, limits: FleetMemoryLimits) -> FleetScheduler {
        let machines = (0..2)
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
        FleetScheduler::new_with_memory_limits(machines, config, Vec::new(), false, limits).unwrap()
    }

    fn wait_pending(fleet: &FleetScheduler, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while fleet.pending_output_entries() < count {
            assert!(
                Instant::now() < deadline,
                "pending observations did not arrive"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn output_pressure_preserves_order_while_other_commands_and_guest_execution_progress() {
        let fleet = two_machine_output_fleet(
            FleetConfig {
                workers: 1,
                event_capacity: 64,
                ..config()
            },
            FleetMemoryLimits {
                event_bytes_per_machine: std::mem::size_of::<FleetEvent>() + 32,
                ..FleetMemoryLimits::default()
            },
        );
        fleet.resource_snapshot(0, 100).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fleet.event_memory().retained_bytes == 0 {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        fleet.resource_snapshot(0, 101).unwrap();
        wait_pending(&fleet, 1);
        fleet.resource_snapshot(0, 102).unwrap();
        fleet.resource_snapshot(1, 200).unwrap();
        fleet.grant(1, 1, 64).unwrap();
        // No ordinary polling: VM 0 stays backpressured. The worker must skip
        // its accepted command and still execute VM 1's commands and Guest.
        wait_advances(&fleet, 1);
        wait_pending(&fleet, 2);
        assert_eq!(fleet.pending_output_headroom_bytes(), 0);
        assert!(
            fleet.pending_output_memory().retained_bytes
                <= fleet.pending_output_memory().limit_bytes
        );
        let mut first = Vec::new();
        let mut second_snapshot = false;
        let mut second_halted = false;
        for _ in 0..5 {
            match next(&fleet) {
                FleetEvent::ResourceSnapshot {
                    machine: 0,
                    request,
                    ..
                } => first.push(request),
                FleetEvent::ResourceSnapshot {
                    machine: 1,
                    request: 200,
                    ..
                } => second_snapshot = true,
                FleetEvent::Boundary {
                    machine: 1,
                    outcome: ComputerAdvanceOutcome::Halted(None),
                    ..
                } => {
                    assert!(
                        second_snapshot,
                        "Guest result overtook its earlier observation"
                    );
                    second_halted = true;
                }
                event => panic!("unexpected observation: {event:?}"),
            }
        }
        assert_eq!(first, vec![100, 101, 102]);
        assert!(second_snapshot && second_halted);
        assert!(fleet.try_event().is_none());
    }

    #[test]
    fn closing_one_of_multiple_backpressured_machines_retains_the_others_output() {
        for detach in [false, true] {
            let fleet = two_machine_output_fleet(
                FleetConfig {
                    workers: 1,
                    event_capacity: 1,
                    ..config()
                },
                FleetMemoryLimits::default(),
            );
            fleet.resource_snapshot(0, 100).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while fleet.event_memory().retained_bytes == 0 {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            fleet.resource_snapshot(0, 101).unwrap();
            wait_pending(&fleet, 1);
            fleet.resource_snapshot(1, 200).unwrap();
            // event_capacity=1 also bounds pending entry reservations. VM 1's
            // already started operation borrows the one worker headroom slot.
            while fleet.pending_output_headroom_bytes() == 0 {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            if detach {
                fleet.detach_with_lifecycle_poll(0, None).unwrap();
            } else {
                fleet.stop_with_lifecycle_poll(0).unwrap();
            }
            wait_lifecycle(&fleet, 0);
            if detach {
                assert!(matches!(
                    fleet.poll_lifecycle_event(0),
                    Some(FleetEvent::ResourceSnapshot {
                        machine: 0,
                        request: 101,
                        ..
                    })
                ));
                assert!(fleet.consume_lifecycle_event(0));
                assert!(fleet.detach_output_memory().retained_bytes > 0);
                assert!(matches!(
                    fleet.poll_lifecycle_event(0),
                    Some(FleetEvent::Detached { machine: 0, .. })
                ));
            } else {
                assert!(matches!(
                    fleet.poll_lifecycle_event(0),
                    Some(FleetEvent::Stopped { machine: 0, .. })
                ));
            }
            assert!(fleet.consume_lifecycle_event(0));
            assert_eq!(fleet.detach_output_memory().retained_bytes, 0);
            assert_eq!(fleet.pending_output_entries(), 0);
            assert!(
                fleet.pending_output_headroom_bytes() > 0,
                "unrelated output was canceled"
            );
            assert!(matches!(
                next(&fleet),
                FleetEvent::ResourceSnapshot {
                    machine: 0,
                    request: 100,
                    ..
                }
            ));
            assert!(matches!(
                next(&fleet),
                FleetEvent::ResourceSnapshot {
                    machine: 1,
                    request: 200,
                    ..
                }
            ));
            assert!(fleet.try_event().is_none());
            let deadline = Instant::now() + Duration::from_secs(5);
            while fleet.event_inflight_bytes() != 0 || fleet.pending_output_headroom_bytes() != 0 {
                assert!(
                    Instant::now() < deadline,
                    "pending ownership was not released"
                );
                thread::yield_now();
            }
            assert_eq!(fleet.pending_output_memory().retained_bytes, 0);
        }
    }

    #[test]
    fn lifecycle_bypasses_ordinary_byte_pressure_and_preserves_published_output() {
        // The first case fills bytes with ample entry capacity. The second uses
        // the oversized delivery path. Neither may require ordinary polling.
        for limit in [std::mem::size_of::<FleetEvent>() + 32, 1] {
            let machines = (0..2)
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
            let fleet = FleetScheduler::new_with_memory_limits(
                machines,
                FleetConfig {
                    workers: 1,
                    event_capacity: 64,
                    ..config()
                },
                Vec::new(),
                false,
                FleetMemoryLimits {
                    event_bytes: limit,
                    event_bytes_per_machine: limit,
                    ..FleetMemoryLimits::default()
                },
            )
            .unwrap();
            fleet.resource_snapshot(0, 100).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while fleet.event_memory().retained_bytes == 0 && fleet.event_headroom_bytes() == 0 {
                assert!(Instant::now() < deadline, "first observation not published");
                thread::yield_now();
            }
            if limit > 1 {
                assert!(fleet.event_memory().retained_bytes > 0);
                assert_eq!(fleet.event_headroom_bytes(), 0);
            } else {
                assert!(fleet.event_headroom_bytes() > 0);
            }
            fleet.resource_snapshot(1, 200).unwrap();
            while fleet.event_inflight_bytes() == 0 {
                assert!(
                    Instant::now() < deadline,
                    "second observation did not encounter byte pressure"
                );
                thread::yield_now();
            }
            assert!(fleet.event_memory().retained_bytes <= limit);
            fleet.detach_with_lifecycle_poll(1, None).unwrap();
            wait_lifecycle(&fleet, 1);
            assert!(matches!(
                fleet.poll_lifecycle_event(1),
                Some(FleetEvent::ResourceSnapshot {
                    machine: 1,
                    request: 200,
                    ..
                })
            ));
            assert!(fleet.consume_lifecycle_event(1));
            assert!(matches!(
                fleet.poll_lifecycle_event(1),
                Some(FleetEvent::Detached { machine: 1, .. })
            ));
            assert!(fleet.consume_lifecycle_event(1));
            assert_eq!(fleet.event_inflight_bytes(), 0);
            assert_eq!(fleet.detach_output_memory().retained_bytes, 0);
            fleet.stop_with_lifecycle_poll(0).unwrap();
            wait_lifecycle(&fleet, 0);
            assert!(matches!(
                fleet.poll_lifecycle_event(0),
                Some(FleetEvent::Stopped { machine: 0, .. })
            ));
            assert!(fleet.consume_lifecycle_event(0));
            assert!(matches!(
                next(&fleet),
                FleetEvent::ResourceSnapshot {
                    machine: 0,
                    request: 100,
                    ..
                }
            ));
            assert!(fleet.try_event().is_none());
            assert_eq!(fleet.event_memory().retained_bytes, 0);
            assert_eq!(fleet.event_headroom_bytes(), 0);
            assert_eq!(fleet.event_inflight_bytes(), 0);
        }
    }

    #[test]
    fn detach_completes_with_full_command_and_event_queues_without_polling() {
        let command_limit = 2 * std::mem::size_of::<QueuedCommand>();
        let fleet = saturated_fleet(
            FleetMemoryLimits {
                command_bytes: command_limit,
                command_bytes_per_machine: command_limit,
                ..FleetMemoryLimits::default()
            },
            false,
        );
        fleet.resource_snapshot(1, 41).unwrap();
        fleet.resource_snapshot(0, 42).unwrap();
        assert_eq!(fleet.command_memory().retained_bytes, command_limit);
        fleet.detach(1).unwrap();
        wait_lifecycle(&fleet, 1);
        assert!(fleet.try_stopped(1).is_none(), "wrong poll consumed detach");
        assert!(fleet.detach_output_memory().retained_bytes > 0);
        let completion = fleet.try_detached(1).unwrap();
        assert!(completion
            .observations
            .iter()
            .all(|event| event.machine() == 1));
        assert!(completion
            .observations
            .iter()
            .any(|event| matches!(event, FleetEvent::ResourceSnapshot { request: 41, .. })));
        let FleetDetachResult::Detached { computer, timers } = completion.result else {
            panic!("eligible machine was not detached");
        };
        assert!(timers.is_empty());
        assert!(computer.resource_snapshot().retired_instructions > 0);
        assert_eq!(fleet.detach_output_memory().retained_bytes, 0);
        assert_eq!(fleet.grant(1, 2, 64), Err(SubmitError::UnknownMachine));
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary { machine: 0, .. }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::ResourceSnapshot {
                machine: 0,
                request: 42,
                ..
            }
        ));
        assert!(fleet.attach(computer).is_ok());
    }

    #[test]
    fn rejected_detach_preserves_undelivered_host_requests_and_query_results() {
        let fleet = saturated_fleet(FleetMemoryLimits::default(), true);
        fleet.resource_snapshot(1, 51).unwrap();
        fleet.detach(1).unwrap();
        wait_lifecycle(&fleet, 1);
        assert_eq!(fleet.grant(1, 2, 64), Err(SubmitError::UnknownMachine));
        let completion = fleet.try_detached(1).unwrap();
        assert!(matches!(completion.result, FleetDetachResult::Rejected(_)));
        let mut observations = completion.observations.into_iter();
        let FleetEvent::Requests { requests, .. } = observations.next().unwrap() else {
            panic!("undelivered host request was lost");
        };
        assert!(matches!(
            observations.next(),
            Some(FleetEvent::ResourceSnapshot { request: 51, .. })
        ));
        assert!(observations.next().is_none());
        let request = &requests[0];
        fleet
            .reply(1, request.task_id, request.id, ReplyValue::Unit)
            .unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary { machine: 0, .. }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::Requests { machine: 1, .. }
        ));
    }

    #[test]
    fn lifecycle_stream_holds_the_fence_and_memory_until_terminal_ack() {
        let fleet = saturated_fleet(FleetMemoryLimits::default(), true);
        fleet.resource_snapshot(1, 101).unwrap();
        fleet.detach_with_lifecycle_poll(1, None).unwrap();
        wait_lifecycle(&fleet, 1);
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary { machine: 0, .. }
        ));
        assert!(
            fleet.try_event().is_none(),
            "ordinary reader stole independent output"
        );
        assert!(fleet.detach_output_memory().retained_bytes > 0);
        assert!(matches!(
            fleet.poll_lifecycle_event(1),
            Some(FleetEvent::Requests { .. })
        ));
        assert!(
            fleet.poll_lifecycle_event(1).is_none(),
            "unacknowledged observation was skipped"
        );
        assert!(fleet.try_detached(1).is_none(), "claimed stream was stolen");
        assert!(fleet.consume_lifecycle_event(1));
        assert!(!fleet.consume_lifecycle_event(1), "duplicate ack succeeded");
        assert!(matches!(
            fleet.poll_lifecycle_event(1),
            Some(FleetEvent::ResourceSnapshot { request: 101, .. })
        ));
        assert!(fleet.consume_lifecycle_event(1));
        assert!(matches!(
            fleet.poll_lifecycle_event(1),
            Some(FleetEvent::DetachRejected { machine: 1 })
        ));
        assert!(fleet.detach_output_memory().retained_bytes > 0);
        assert_eq!(fleet.grant(1, 2, 64), Err(SubmitError::UnknownMachine));
        assert!(fleet.try_event().is_none());
        assert!(fleet.consume_lifecycle_event(1));
        assert_eq!(fleet.detach_output_memory().retained_bytes, 0);
        assert!(fleet.grant(1, 2, 64).is_ok());
        assert!(!fleet.consume_lifecycle_event(1));
    }

    #[test]
    fn lifecycle_stream_retains_worker_headroom_through_small_buffer_retries() {
        let fleet = saturated_fleet(
            FleetMemoryLimits {
                detach_output_bytes: 1,
                detach_output_bytes_per_machine: 1,
                ..FleetMemoryLimits::default()
            },
            false,
        );
        fleet.detach_with_lifecycle_poll(1, None).unwrap();
        wait_lifecycle(&fleet, 1);
        assert!(matches!(
            fleet.poll_lifecycle_event(1),
            Some(FleetEvent::Boundary { machine: 1, .. })
        ));
        assert!(fleet.consume_lifecycle_event(1));
        assert!(matches!(
            fleet.poll_lifecycle_event(1),
            Some(FleetEvent::DetachRejected { machine: 1 })
        ));
        assert!(fleet.detach_headroom_bytes() > 0);
        fleet.stop_with_lifecycle_poll(0).unwrap();
        wait_lifecycle(&fleet, 0);
        assert!(matches!(
            fleet.poll_lifecycle_event(0),
            Some(FleetEvent::Stopped { machine: 0, .. })
        ));
        assert!(fleet.consume_lifecycle_event(0));
        assert!(fleet.detach_headroom_bytes() > 0);
        assert!(fleet.consume_lifecycle_event(1));
        assert_eq!(fleet.detach_headroom_bytes(), 0);
    }

    #[test]
    fn detach_memory_exhaustion_keeps_commands_and_does_not_block_stop() {
        let fleet = saturated_fleet(
            FleetMemoryLimits {
                detach_output_bytes: 1,
                detach_output_bytes_per_machine: 1,
                ..FleetMemoryLimits::default()
            },
            false,
        );
        fleet.resource_snapshot(1, 61).unwrap();
        fleet.detach(1).unwrap();
        wait_lifecycle(&fleet, 1);
        assert_eq!(
            fleet.lanes[0].lock().unwrap().commands.len(),
            1,
            "commands kept running after detach exhausted its output budget"
        );
        assert_eq!(fleet.detach_output_memory().retained_bytes, 0);
        assert!(fleet.detach_headroom_bytes() > 0);
        fleet.stop(0).unwrap();
        wait_lifecycle(&fleet, 0);
        assert!(fleet.try_stopped(0).is_some());
        let completion = fleet.try_detached(1).unwrap();
        assert!(matches!(
            completion.result,
            FleetDetachResult::Rejected(FleetDetachRejection::OutputCapacity)
        ));
        assert_eq!(fleet.detach_headroom_bytes(), 0);
        assert!(matches!(
            completion.observations.front(),
            Some(FleetEvent::Boundary { machine: 1, .. })
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary { machine: 0, .. }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::ResourceSnapshot {
                machine: 1,
                request: 61,
                ..
            }
        ));
        fleet.detach(1).unwrap();
        wait_lifecycle(&fleet, 1);
        assert!(matches!(
            fleet.try_detached(1).unwrap().result,
            FleetDetachResult::Detached { .. }
        ));
    }

    #[test]
    fn detach_pressure_preserves_order_through_the_ordinary_polling_api() {
        let limit =
            std::mem::size_of::<FleetEvent>() + std::mem::size_of::<(Reservation, Reservation)>();
        let fleet = saturated_fleet(
            FleetMemoryLimits {
                detach_output_bytes: limit,
                detach_output_bytes_per_machine: limit,
                ..FleetMemoryLimits::default()
            },
            false,
        );
        fleet.resource_snapshot(1, 71).unwrap();
        fleet.resource_snapshot(1, 72).unwrap();
        fleet.detach(1).unwrap();
        wait_lifecycle(&fleet, 1);
        assert_eq!(fleet.detach_output_memory().retained_bytes, limit);
        assert!(fleet.detach_headroom_bytes() > 0);
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary { machine: 0, .. }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary { machine: 1, .. }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::ResourceSnapshot {
                machine: 1,
                request: 71,
                ..
            }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::DetachRejected { machine: 1 }
        ));
        assert_eq!(fleet.detach_output_memory().retained_bytes, 0);
        assert_eq!(fleet.detach_headroom_bytes(), 0);
        assert!(matches!(
            next(&fleet),
            FleetEvent::ResourceSnapshot {
                machine: 1,
                request: 72,
                ..
            }
        ));
    }

    #[test]
    fn worker_headroom_cannot_accumulate_across_unclaimed_detach_refusals() {
        let fleet = saturated_fleet(
            FleetMemoryLimits {
                detach_output_bytes: 1,
                detach_output_bytes_per_machine: 1,
                ..FleetMemoryLimits::default()
            },
            false,
        );
        fleet.detach(1).unwrap();
        wait_lifecycle(&fleet, 1);
        let headroom = fleet.detach_headroom_bytes();
        assert!(headroom > 0);
        let machine = ComputerMachine::start(
            crate::execution::fixtures::two_block_artifact(1, 1),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let id = fleet.attach(Box::new(machine)).unwrap();
        fleet.resource_snapshot(id, 81).unwrap();
        fleet.detach(id).unwrap();
        wait_lifecycle(&fleet, id);
        assert_eq!(fleet.detach_headroom_bytes(), headroom);
        let completion = fleet.try_detached(1).unwrap();
        assert!(matches!(
            completion.result,
            FleetDetachResult::Rejected(FleetDetachRejection::OutputCapacity)
        ));
        fleet.stop(1).unwrap();
        wait_lifecycle(&fleet, 1);
        assert_eq!(
            fleet.lanes[0].lock().unwrap().commands.len(),
            2,
            "unclaimed refusal released the attach/command fence"
        );
        assert_eq!(fleet.detach_headroom_bytes(), 0);
        let completion = fleet.try_detached(id).unwrap();
        assert!(matches!(
            completion.result,
            FleetDetachResult::Rejected(FleetDetachRejection::OutputCapacity)
        ));
        assert!(completion.observations.is_empty());
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary { machine: 0, .. }
        ));
        // Stop completion can be observed before the query; it has its own slot.
        let mut observed = false;
        while !observed {
            observed = matches!(next(&fleet), FleetEvent::ResourceSnapshot { machine, request: 81, .. } if machine == id);
        }
    }

    #[test]
    fn checkpoint_detach_settles_undelivered_requests_without_draining_other_output() {
        let fleet = saturated_fleet(FleetMemoryLimits::default(), true);
        let advances = fleet.metrics().advances;
        fleet.detach_for_checkpoint(1, 1).unwrap();
        wait_lifecycle(&fleet, 1);
        let completion = fleet.try_detached(1).unwrap();
        assert!(completion
            .observations
            .iter()
            .any(|event| matches!(event, FleetEvent::Requests { machine: 1, .. })));
        let FleetDetachResult::Detached {
            mut computer,
            timers,
        } = completion.result
        else {
            panic!("checkpoint refused")
        };
        assert!(timers.is_empty());
        assert_eq!(advances, fleet.metrics().advances);
        assert_eq!(0, fleet.detach_output_memory().retained_bytes);
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary { machine: 0, .. }
        ));
        let outcome = computer.advance(64, 64, 64).unwrap();
        assert!(
            matches!(outcome, ComputerAdvanceOutcome::UncaughtException(ref detail) if detail.contains("hibernated")),
            "{outcome:?}"
        );
    }

    #[test]
    fn checkpoint_detach_cancels_cost_inspection_without_a_host_request_registry() {
        let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
        let computer = ComputerMachine::start(
            crate::execution::fixtures::two_unit_capability_calls_artifact(),
            profile(),
            &[binding],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new_with_request_inspections(
            vec![(1, computer)],
            config(),
            vec![RequestInspection {
                namespace: "app".into(),
                name: "entry".into(),
                abi_major: 1,
                abi_minor: 0,
                operation: 0,
                over_budget_detail: "over budget".into(),
            }],
        )
        .unwrap();
        fleet.grant(1, 1, 64).unwrap();
        assert!(matches!(next(&fleet), FleetEvent::Inspect { .. }));
        fleet.resource_snapshot(1, 77).unwrap();
        let retired = match next(&fleet) {
            FleetEvent::ResourceSnapshot {
                request: 77,
                snapshot,
                ..
            } => snapshot.retired_instructions,
            other => panic!("{other:?}"),
        };
        fleet.detach_for_checkpoint(1, 1).unwrap();
        wait_lifecycle(&fleet, 1);
        let completion = fleet.try_detached(1).unwrap();
        let FleetDetachResult::Detached { mut computer, .. } = completion.result else {
            panic!("inspection blocked checkpoint")
        };
        assert_eq!(retired, computer.resource_snapshot().retired_instructions);
        let outcome = computer.advance(64, 64, 64).unwrap();
        assert!(
            matches!(outcome, ComputerAdvanceOutcome::UncaughtException(ref detail) if detail.contains("hibernated")),
            "{outcome:?}"
        );
    }

    #[test]
    fn checkpoint_detach_drains_accepted_reply_without_executing_the_next_host_call() {
        let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
        let computer = ComputerMachine::start(
            crate::execution::fixtures::two_unit_capability_calls_artifact(),
            profile(),
            &[binding],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new_with_coordinated_turns(
            vec![(1, computer)],
            config(),
            Vec::new(),
            true,
        )
        .unwrap();
        fleet.grant(1, 1, 64).unwrap();
        let request = match next(&fleet) {
            FleetEvent::Requests { requests, .. } => requests.into_iter().next().unwrap(),
            other => panic!("{other:?}"),
        };
        let retired = match next(&fleet) {
            FleetEvent::Quiescent { retired, .. } => retired,
            other => panic!("{other:?}"),
        };
        fleet
            .reply(1, request.task_id, request.id, ReplyValue::Unit)
            .unwrap();
        fleet.detach_for_checkpoint(1, 1).unwrap();
        wait_lifecycle(&fleet, 1);
        let FleetDetachResult::Detached { mut computer, .. } =
            fleet.try_detached(1).unwrap().result
        else {
            panic!("checkpoint refused")
        };
        assert_eq!(retired, computer.resource_snapshot().retired_instructions);
        assert_eq!(
            Err(crate::ComputerError::InvalidRequestId),
            computer.resume_host_request_for(
                request.task_id,
                request.id,
                HostResponse::Success(HostValueInput::Unit)
            )
        );
        let second = match computer.advance(64, 64, 64).unwrap() {
            ComputerAdvanceOutcome::HostRequestBatch(batch) => batch.requests.into_vec().remove(0),
            other => panic!("{other:?}"),
        };
        assert_ne!(second.id, request.id);
    }

    #[test]
    fn checkpoint_detach_rejects_stale_tick_before_canceling_waits() {
        let fleet = saturated_fleet(FleetMemoryLimits::default(), true);
        fleet.detach_for_checkpoint(1, 0).unwrap();
        wait_lifecycle(&fleet, 1);
        let completion = fleet.try_detached(1).unwrap();
        assert!(matches!(
            completion.result,
            FleetDetachResult::Rejected(FleetDetachRejection::Busy)
        ));
        let request = completion
            .observations
            .into_iter()
            .find_map(|event| match event {
                FleetEvent::Requests { mut requests, .. } => Some(requests.remove(0)),
                _ => None,
            })
            .unwrap();
        fleet
            .reply(1, request.task_id, request.id, ReplyValue::Unit)
            .unwrap();
        fleet.detach_for_checkpoint(1, 1).unwrap();
        wait_lifecycle(&fleet, 1);
        assert!(matches!(
            fleet.try_detached(1).unwrap().result,
            FleetDetachResult::Detached { .. }
        ));
    }

    #[test]
    fn checkpoint_detach_preserves_timer_remaining_ticks() {
        let computer = ComputerMachine::start(
            crate::execution::fixtures::timer_unit_artifact(2),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new_with_coordinated_turns(
            vec![(1, computer)],
            config(),
            Vec::new(),
            true,
        )
        .unwrap();
        fleet.grant(1, 1, 64).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Quiescent {
                timers_pending: true,
                ..
            }
        ));
        fleet.detach_for_checkpoint(1, 2).unwrap();
        wait_lifecycle(&fleet, 1);
        let FleetDetachResult::Detached { computer, timers } =
            fleet.try_detached(1).unwrap().result
        else {
            panic!("timer blocked checkpoint")
        };
        assert_eq!(1, timers.len());
        assert_eq!((2, 1), (timers[0].duration, timers[0].remaining));
        let machine = fleet.attach_with_timers(computer, 100, timers).unwrap();
        fleet.grant(machine, 100, 64).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Quiescent {
                timers_pending: true,
                ..
            }
        ));
        fleet.grant(machine, 101, 64).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
    }

    #[test]
    fn saturated_detach_returns_timer_state_without_pumping_observations() {
        let timer = ComputerMachine::start(
            crate::execution::fixtures::timer_unit_artifact(2),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let idle = ComputerMachine::start(
            crate::execution::fixtures::two_block_artifact(1, 1),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new_with_coordinated_turns(
            vec![(0, idle), (1, timer)],
            FleetConfig {
                workers: 1,
                event_capacity: 1,
                ..config()
            },
            Vec::new(),
            true,
        )
        .unwrap();
        fleet.grant(1, 1, 64).unwrap();
        wait_advances(&fleet, 2);
        fleet.resource_snapshot(0, 91).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fleet.waiting_publishers.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "output did not saturate");
            thread::yield_now();
        }
        fleet.detach_with_timers(1, 2).unwrap();
        wait_lifecycle(&fleet, 1);
        let completion = fleet.try_detached(1).unwrap();
        let FleetDetachResult::Detached { computer, timers } = completion.result else {
            panic!("timer detach refused");
        };
        assert_eq!(timers.len(), 1);
        assert_eq!((timers[0].duration, timers[0].remaining), (2, 1));
        assert!(fleet.attach_with_timers(computer, 100, timers).is_ok());
    }

    #[test]
    fn a_new_close_cannot_interrupt_the_active_machines_fence() {
        let mut mailbox = LaneMailbox::new(4);
        for tick in 1..=2 {
            assert!(mailbox
                .submit(Command::Tick {
                    machine: 9,
                    tick,
                    retirements: 1,
                    deadline: None,
                })
                .is_ok());
        }
        mailbox.closes.insert(9, CloseRequest::Stop);
        assert!(matches!(
            mailbox.next().map(|queued| queued.command),
            Some(Command::Tick {
                machine: 9,
                tick: 1,
                ..
            })
        ));
        mailbox.closes.insert(1, CloseRequest::Stop);
        assert!(matches!(
            mailbox.next_for(Some(9)).map(|queued| queued.command),
            Some(Command::Tick {
                machine: 9,
                tick: 2,
                ..
            })
        ));
        assert!(matches!(
            mailbox.next_for(Some(9)).map(|queued| queued.command),
            Some(Command::Stop { machine: 9 })
        ));
        assert!(matches!(
            mailbox.next().map(|queued| queued.command),
            Some(Command::Stop { machine: 1 })
        ));
    }

    fn grant_until_terminal_outcome(fleet: &FleetScheduler, machine: u64) -> FleetEvent {
        for tick in 2..300 {
            fleet.grant(machine, tick, 512).unwrap();
            thread::sleep(Duration::from_millis(1));
            while let Some(event) = fleet.try_event() {
                if matches!(
                    event,
                    FleetEvent::Boundary {
                        outcome: ComputerAdvanceOutcome::WaitingForTerminalEvent,
                        ..
                    }
                ) {
                    continue;
                }
                return event;
            }
        }
        panic!("no terminal outcome: {:?}", fleet.metrics());
    }

    #[test]
    fn event_signal_observes_publication_between_poll_and_wait() {
        let signal = Arc::new(FleetEventSignal::default());
        let (sender, receiver) = event_queue::channel(
            1,
            FleetMemoryLimits::default().event_bytes,
            FleetMemoryLimits::default().event_bytes_per_machine,
        );
        let sender = EventSender {
            sender,
            signal: Arc::clone(&signal),
            completions: Arc::new(Mutex::new(BTreeMap::new())),
            pending: super::pending::PendingCapacity::new(FleetMemoryLimits::default(), 64),
            detach_memory: ByteBudget::new(FleetMemoryLimits::default().detach_output_bytes),
            detach_machine_limit: FleetMemoryLimits::default().detach_output_bytes_per_machine,
            detach_headroom: Arc::new(AtomicUsize::new(0)),
        };
        let generation = signal.generation();
        assert!(receiver.try_recv().is_err());
        let waiters = AtomicUsize::new(0);
        let sender = WorkerOutput::new(sender, &waiters);
        assert!(publish(
            &sender,
            &AtomicBool::new(false),
            FleetEvent::Quiescent {
                machine: 1,
                tick: 2,
                retired: 3,
                timers_pending: false,
            }
        ));
        assert!(signal.wait(generation, Duration::ZERO));
        assert!(receiver.try_recv().is_ok());
        assert!(!signal.wait(signal.generation(), Duration::ZERO));
    }

    #[test]
    fn closing_fleet_wakes_event_waiter_without_holding_fleet() {
        let fleet = FleetScheduler::new(Vec::new(), config()).unwrap();
        let signal = fleet.event_signal();
        let generation = signal.generation();
        let waiter_signal = Arc::clone(&signal);
        let waiter = thread::spawn(move || waiter_signal.wait(generation, Duration::from_secs(5)));
        drop(fleet);
        assert!(!waiter.join().unwrap());
        assert!(!signal.wait(generation, Duration::from_secs(5)));
    }

    #[test]
    fn process_lifecycle_events_do_not_stop_the_fleet() {
        use crate::{
            ComputerFileSystem, FileCapability, FileRights, FileSystemLimits, VirtualPath,
        };
        let limits = FileSystemLimits::testing();
        let home = VirtualPath::parse_utf8("/home", &limits).unwrap();
        let owner = FileCapability::new(home, FileRights::OWNER);
        let mut filesystem = ComputerFileSystem::with_limits(limits);
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
        let mut filesystem = ComputerFileSystem::with_limits(limits);
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
    fn a_machine_with_a_live_child_can_join_a_fleet() {
        use crate::{
            ComputerFileSystem, FileCapability, FileRights, FileSystemLimits, VirtualPath,
        };
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(
            VirtualPath::parse_utf8("/home", &limits).unwrap(),
            FileRights::OWNER,
        );
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        let child = crate::execution::fixtures::two_block_artifact(1, 1);
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
        let mut machine =
            ComputerMachine::start_in_filesystem(parent, profile(), &[], &[], filesystem, owner)
                .unwrap();
        loop {
            match machine.advance(64, 64, 64).unwrap() {
                ComputerAdvanceOutcome::ProcessEntered(1) => break,
                ComputerAdvanceOutcome::SliceExhausted => {}
                other => panic!("{other:?}"),
            }
        }
        let fleet = FleetScheduler::new(vec![], config()).unwrap();
        let id = fleet.attach(Box::new(machine)).unwrap();
        fleet.confirm_redstone_output(id, 0x123456).unwrap();
        fleet.detach_for_checkpoint(id, 0).unwrap();
        wait_lifecycle(&fleet, id);
        let FleetDetachResult::Detached { computer, timers } =
            fleet.try_detached(id).unwrap().result
        else {
            panic!("expected detached child");
        };
        assert_eq!(computer.fleet_process_scopes(), vec![0, 1]);
        assert_eq!(computer.fleet_confirmed_redstone_output(), 0x123456);
        assert!(timers.is_empty());
        let id = fleet.attach(computer).unwrap();
        fleet.grant(id, 1, 512).unwrap();
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
        let mut filesystem = ComputerFileSystem::with_limits(limits);
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
    fn deployment_preserves_candidate_after_conflict_and_owns_it_until_discarded() {
        use crate::{
            ComputerFileSystem, FileCapability, FileRights, FileSystemLimits, VirtualPath,
        };
        let limits = FileSystemLimits::testing();
        let home = VirtualPath::parse_utf8("/home", &limits).unwrap();
        let path = VirtualPath::parse_utf8("/home/demo", &limits).unwrap();
        let owner = FileCapability::new(home, FileRights::OWNER);
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        filesystem.write_file(&owner, &path, b"old", true).unwrap();
        let artifact = crate::execution::fixtures::two_block_artifact(1, 1);
        let bytes: Arc<[u8]> =
            Arc::from(crate::test_encode::encode_artifact(artifact.decoded()).unwrap());
        let machine =
            ComputerMachine::start_in_filesystem(artifact, profile(), &[], &[], filesystem, owner)
                .unwrap();
        let fleet = FleetScheduler::new(vec![(1, machine)], config()).unwrap();
        fleet.prepare_deployment(1, 10, Arc::clone(&bytes)).unwrap();
        let candidate = match next(&fleet) {
            FleetEvent::Deployment {
                request: 10,
                result: Ok(DeploymentResult::Prepared(candidate)),
                ..
            } => candidate,
            event => panic!("unexpected event {event:?}"),
        };
        fleet.detach(1).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::DetachRejected { machine: 1 }
        ));
        fleet
            .install_deployment(
                1,
                11,
                "/home/demo".into(),
                crate::ExecutableRevision::Absent,
                candidate,
            )
            .unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Deployment {
                request: 11,
                result: Err(DeploymentError::FileSystem(crate::FileSystemError::Busy)),
                ..
            }
        ));
        fleet
            .deployment_revision(1, 12, "/home/demo".into())
            .unwrap();
        let revision = match next(&fleet) {
            FleetEvent::Deployment {
                request: 12,
                result: Ok(DeploymentResult::Revision(revision)),
                ..
            } => revision,
            event => panic!("unexpected event {event:?}"),
        };
        fleet
            .install_deployment(1, 13, "/home/demo".into(), revision, candidate)
            .unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Deployment {
                request: 13,
                result: Ok(DeploymentResult::Installed(
                    crate::ExecutableRevision::Present(_)
                )),
                ..
            }
        ));
        fleet.discard_deployment(1, 14, candidate).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Deployment {
                request: 14,
                result: Err(DeploymentError::InvalidCandidate),
                ..
            }
        ));
        fleet.detach(1).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Detached { machine: 1, .. }
        ));
    }

    #[test]
    fn deployment_candidate_identity_is_not_reused_between_fleets() {
        let artifact = crate::execution::fixtures::two_block_artifact(1, 1);
        let bytes: Arc<[u8]> =
            Arc::from(crate::test_encode::encode_artifact(artifact.decoded()).unwrap());
        let machine = || ComputerMachine::start(artifact.clone(), profile(), &[], &[]).unwrap();
        let first = FleetScheduler::new(vec![(1, machine())], config()).unwrap();
        first.prepare_deployment(1, 1, Arc::clone(&bytes)).unwrap();
        let original = match next(&first) {
            FleetEvent::Deployment {
                result: Ok(DeploymentResult::Prepared(token)),
                ..
            } => token,
            event => panic!("unexpected event {event:?}"),
        };
        drop(first);
        let second = FleetScheduler::new(vec![(1, machine())], config()).unwrap();
        second.prepare_deployment(1, 1, bytes).unwrap();
        let replacement = match next(&second) {
            FleetEvent::Deployment {
                result: Ok(DeploymentResult::Prepared(token)),
                ..
            } => token,
            event => panic!("unexpected event {event:?}"),
        };
        assert_ne!(original, replacement);
        second.discard_deployment(1, 2, original).unwrap();
        assert!(matches!(
            next(&second),
            FleetEvent::Deployment {
                result: Err(DeploymentError::InvalidCandidate),
                ..
            }
        ));
    }

    #[test]
    fn deployment_candidate_capacity_is_bounded_and_discard_releases_it() {
        let artifact = crate::execution::fixtures::two_block_artifact(1, 1);
        let bytes: Arc<[u8]> =
            Arc::from(crate::test_encode::encode_artifact(artifact.decoded()).unwrap());
        let machine = ComputerMachine::start(artifact, profile(), &[], &[]).unwrap();
        let fleet = FleetScheduler::new(vec![(1, machine)], config()).unwrap();
        let mut first = 0;
        for request in 0..4 {
            fleet
                .prepare_deployment(1, request, Arc::clone(&bytes))
                .unwrap();
            match next(&fleet) {
                FleetEvent::Deployment {
                    result: Ok(DeploymentResult::Prepared(candidate)),
                    ..
                } => {
                    if request == 0 {
                        first = candidate
                    }
                }
                event => panic!("unexpected event {event:?}"),
            }
        }
        fleet.prepare_deployment(1, 5, Arc::clone(&bytes)).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Deployment {
                result: Err(DeploymentError::Capacity),
                ..
            }
        ));
        fleet.discard_deployment(1, 6, first).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Deployment {
                result: Ok(DeploymentResult::Discarded),
                ..
            }
        ));
        fleet.prepare_deployment(1, 7, bytes).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Deployment {
                result: Ok(DeploymentResult::Prepared(_)),
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
        let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
        let mut machine = ComputerMachine::start(
            crate::execution::fixtures::two_unit_capability_calls_artifact(),
            profile(),
            &[binding],
            &[],
        )
        .unwrap();
        let request = loop {
            match machine.advance(64, 64, 64).unwrap() {
                ComputerAdvanceOutcome::HostRequestBatch(batch) => break batch.requests[0].clone(),
                ComputerAdvanceOutcome::SliceExhausted => {}
                other => panic!("{other:?}"),
            }
        };
        let (reason, mut returned) = fleet.attach(Box::new(machine)).unwrap_err();
        assert_eq!(reason, SubmitError::InvalidMachine);
        returned
            .resume_host_request_for(
                request.task_id,
                request.id,
                HostResponse::Success(HostValueInput::Unit),
            )
            .unwrap();
        assert!(matches!(
            returned.advance(64, 64, 64).unwrap(),
            ComputerAdvanceOutcome::HostRequestBatch(_)
        ));
    }

    #[test]
    fn an_executed_quiescent_machine_can_join_a_fleet() {
        let fleet = FleetScheduler::new(vec![], config()).unwrap();
        let mut machine = ComputerMachine::start(
            crate::execution::fixtures::two_block_artifact(1, 1),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        machine.advance(1, 1, 1).unwrap();
        assert!(machine.resource_snapshot().executed_instructions > 0);
        let id = fleet.attach(Box::new(machine)).unwrap();
        fleet.grant(id, 1, 64).unwrap();
        assert!(
            matches!(next(&fleet), FleetEvent::Boundary { machine, outcome: ComputerAdvanceOutcome::Halted(_), .. } if machine == id)
        );
    }

    #[test]
    fn detached_machine_keeps_its_execution_and_filesystem_state() {
        let fleet = FleetScheduler::new(vec![], config()).unwrap();
        use crate::{
            ComputerFileSystem, FileCapability, FileRights, FileSystemLimits, VirtualPath,
        };
        let limits = FileSystemLimits::testing();
        let home = VirtualPath::parse_utf8("/home", &limits).unwrap();
        let file = VirtualPath::parse_utf8("/home/note", &limits).unwrap();
        let owner = FileCapability::new(home, FileRights::OWNER);
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        filesystem
            .write_file(&owner, &file, b"preserved", false)
            .unwrap();
        let machine = ComputerMachine::start_in_filesystem(
            crate::execution::fixtures::two_block_artifact(1, 1),
            profile(),
            &[],
            &[],
            filesystem,
            owner,
        )
        .unwrap();
        let id = fleet.attach(Box::new(machine)).unwrap();
        fleet.grant(id, 1, 64).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
        fleet.detach(id).unwrap();
        let mut machine = match next(&fleet) {
            FleetEvent::Detached {
                machine, computer, ..
            } if machine == id => computer,
            event => panic!("{event:?}"),
        };
        assert!(machine.resource_snapshot().retired_instructions > 0);
        assert!(machine.filesystem_stat(&file).is_ok());
        assert!(matches!(
            machine.advance(64, 64, 64).unwrap(),
            ComputerAdvanceOutcome::Halted(_)
        ));
        assert_eq!(fleet.grant(id, 2, 64), Err(SubmitError::UnknownMachine));
    }

    #[test]
    fn initial_machines_reject_untracked_external_requests() {
        let ops = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let binding = CapabilityBinding::new("app", "entry", 1, 0, &ops);
        let mut machine = ComputerMachine::start(
            crate::execution::fixtures::two_unit_capability_calls_artifact(),
            profile(),
            &[binding],
            &[],
        )
        .unwrap();
        assert!(matches!(
            machine.advance(64, 64, 64).unwrap(),
            ComputerAdvanceOutcome::HostRequestBatch(_)
        ));
        assert!(FleetScheduler::new(vec![(7, machine)], config()).is_err());
    }

    #[test]
    fn detach_rejects_pending_host_work_without_losing_the_machine() {
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
        fleet.detach(7).unwrap();
        assert_eq!(
            fleet.reply(7, request.task_id, request.id, ReplyValue::Unit),
            Err(SubmitError::UnknownMachine)
        );
        assert!(matches!(
            next(&fleet),
            FleetEvent::DetachRejected { machine: 7 }
        ));
        fleet
            .reply(7, request.task_id, request.id, ReplyValue::Unit)
            .unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Requests { machine: 7, .. }
        ));
    }

    #[test]
    fn terminal_input_resumes_a_parked_machine() {
        use crate::{TerminalKey, TerminalKeyAction, TerminalKeyEvent, TerminalModifiers};
        let machine = ComputerMachine::start(
            crate::execution::fixtures::stdio_conformance_artifact(&[]),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(7, machine)], config()).unwrap();
        fleet.grant(7, 1, 512).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::WaitingForTerminalEvent,
                ..
            }
        ));
        let before_input = fleet.metrics().advances;
        fleet
            .terminal_key(
                7,
                TerminalKeyEvent::new(
                    TerminalKey::Enter,
                    TerminalKeyAction::Press,
                    TerminalModifiers::default(),
                ),
            )
            .unwrap();
        wait_advances(&fleet, before_input + 1);
        fleet.grant(7, 2, 512).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(0))),
                ..
            }
        ));
    }

    #[test]
    fn canonical_line_resumes_a_parked_machine() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::stdio_conformance_artifact(&[]),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(
            vec![(7, machine)],
            FleetConfig {
                guest_budget: 1024,
                ..config()
            },
        )
        .unwrap();
        fleet.grant(7, 1, 512).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::WaitingForTerminalEvent,
                ..
            }
        ));
        fleet
            .canonical_line(7, "hello".encode_utf16().collect())
            .unwrap();
        assert!(matches!(
            grant_until_terminal_outcome(&fleet, 7),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
    }

    #[test]
    fn checked_input_reports_the_actual_result_with_request_identity() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::stdio_conformance_artifact(&[]),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(7, machine)], config()).unwrap();
        fleet.canonical_line_checked(7, 31, Box::new([])).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::InputResult {
                machine: 7,
                request: 31,
                result: Err(InputFailure::Canonical(
                    crate::CanonicalLineSubmissionError::NoPendingRead
                ))
            }
        ));
        fleet.grant(7, 1, 512).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::WaitingForTerminalEvent,
                ..
            }
        ));
        fleet.canonical_line_checked(7, 32, Box::new([])).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::InputResult {
                machine: 7,
                request: 32,
                result: Ok(())
            }
        ));
        assert!(matches!(
            grant_until_terminal_outcome(&fleet, 7),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
    }

    #[test]
    fn a_machine_waiting_for_terminal_input_can_move_between_owners() {
        let mut machine = ComputerMachine::start(
            crate::execution::fixtures::stdio_conformance_artifact(&[]),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        loop {
            match machine.advance(64, 64, 64).unwrap() {
                ComputerAdvanceOutcome::WaitingForTerminalEvent => break,
                ComputerAdvanceOutcome::SliceExhausted => {}
                other => panic!("{other:?}"),
            }
        }
        let fleet = FleetScheduler::new(vec![], config()).unwrap();
        let first = fleet.attach(Box::new(machine)).unwrap();
        fleet.detach(first).unwrap();
        let machine = match next(&fleet) {
            FleetEvent::Detached {
                machine, computer, ..
            } if machine == first => computer,
            other => panic!("{other:?}"),
        };
        let second = fleet.attach(machine).unwrap();
        fleet.grant(second, 1, 512).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::WaitingForTerminalEvent,
                ..
            }
        ));
        fleet.canonical_line(second, Box::new([])).unwrap();
        assert!(matches!(
            grant_until_terminal_outcome(&fleet, second),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
    }

    #[test]
    fn terminal_text_and_enter_resume_a_parked_machine() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::stdio_conformance_artifact(&[]),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(
            vec![(7, machine)],
            FleetConfig {
                guest_budget: 1024,
                ..config()
            },
        )
        .unwrap();
        fleet.grant(7, 1, 512).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::WaitingForTerminalEvent,
                ..
            }
        ));
        fleet.terminal_text(7, "hello".into()).unwrap();
        fleet
            .terminal_key(
                7,
                crate::TerminalKeyEvent::new(
                    crate::TerminalKey::Enter,
                    crate::TerminalKeyAction::Press,
                    crate::TerminalModifiers::default(),
                ),
            )
            .unwrap();
        assert!(matches!(
            grant_until_terminal_outcome(&fleet, 7),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
    }

    #[test]
    fn terminal_updates_commit_writes_and_preserve_revision() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::stdio_conformance_artifact(&[]),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(7, machine)], config()).unwrap();
        fleet.grant(7, 1, 512).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::WaitingForTerminalEvent,
                ..
            }
        ));
        fleet.terminal_update(7, 5, u64::MAX).unwrap();
        let revision = match next(&fleet) {
            FleetEvent::TerminalUpdate {
                machine: 7,
                request: 5,
                update: crate::TerminalUpdate::Full(snapshot),
            } => snapshot.revision(),
            other => panic!("{other:?}"),
        };
        fleet.terminal_update(7, 6, revision).unwrap();
        assert!(
            matches!(next(&fleet), FleetEvent::TerminalUpdate { machine: 7, request: 6, update: crate::TerminalUpdate::Unchanged { revision: r } } if r == revision)
        );
    }

    #[test]
    fn redstone_input_and_confirmed_output_are_ordered_before_execution() {
        let start = |artifact| ComputerMachine::start(artifact, profile(), &[], &[]).unwrap();
        let input = start(crate::execution::fixtures::redstone_i32_artifact(
            0,
            false,
            &[2],
        ));
        let output = start(crate::execution::fixtures::redstone_i32_artifact(
            5,
            false,
            &[],
        ));
        let fleet = FleetScheduler::new(vec![(1, input), (2, output)], config()).unwrap();
        assert_eq!(
            fleet.redstone_input(1, 1 << 30),
            Err(SubmitError::InvalidInput)
        );
        fleet
            .redstone_input(1, 1 << 2 | (11 << (6 + 2 * 4)))
            .unwrap();
        fleet.confirm_redstone_output(2, 0x1234_5678).unwrap();
        fleet.grant(1, 1, 512).unwrap();
        fleet.grant(2, 1, 512).unwrap();
        let mut outcomes = HashMap::new();
        for _ in 0..2 {
            match next(&fleet) {
                FleetEvent::Boundary {
                    machine,
                    outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(value))),
                    ..
                } => {
                    outcomes.insert(machine, value);
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(outcomes.get(&1), Some(&11));
        assert_eq!(outcomes.get(&2), Some(&0x1234_5678));
    }

    #[test]
    fn stop_captures_generation_after_accepted_mutation_without_ordinary_polling() {
        use crate::{
            ComputerFileSystem, FileCapability, FileRights, FileSystemLimits, VirtualPath,
        };
        for delivery in 0..3 {
            let limits = FileSystemLimits::testing();
            let owner = FileCapability::new(
                VirtualPath::parse_utf8("/home", &limits).unwrap(),
                FileRights::OWNER,
            );
            let mut filesystem = ComputerFileSystem::with_limits(limits);
            filesystem
                .write_file(
                    &owner,
                    &VirtualPath::parse_utf8("/home/a", &limits).unwrap(),
                    b"a",
                    false,
                )
                .unwrap();
            let artifact = crate::execution::fixtures::two_block_artifact(1, 1);
            let bytes: Arc<[u8]> =
                Arc::from(crate::test_encode::encode_artifact(artifact.decoded()).unwrap());
            let machine = ComputerMachine::start_in_filesystem(
                artifact,
                profile(),
                &[],
                &[],
                filesystem,
                owner,
            )
            .unwrap();
            let expected = machine.filesystem_generation() + 1;
            let other = ComputerMachine::start(
                crate::execution::fixtures::two_block_artifact(1, 1),
                profile(),
                &[],
                &[],
            )
            .unwrap();
            let fleet = FleetScheduler::new(
                vec![(1, machine), (2, other)],
                FleetConfig {
                    workers: 1,
                    event_capacity: 1,
                    ..config()
                },
            )
            .unwrap();
            fleet.prepare_deployment(1, 10, bytes).unwrap();
            let candidate = match next(&fleet) {
                FleetEvent::Deployment {
                    result: Ok(DeploymentResult::Prepared(candidate)),
                    ..
                } => candidate,
                other => panic!("{other:?}"),
            };
            fleet.resource_snapshot(2, 11).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while fleet.event_memory().retained_bytes == 0 {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            fleet
                .install_deployment(
                    1,
                    12,
                    "/home/demo".into(),
                    crate::ExecutableRevision::Absent,
                    candidate,
                )
                .unwrap();
            if delivery == 1 {
                fleet.stop_with_lifecycle_poll(1).unwrap();
            } else {
                fleet.stop(1).unwrap();
            }
            wait_lifecycle(&fleet, 1);
            if delivery == 0 {
                let stopped = fleet.try_stopped(1).unwrap();
                assert_eq!(stopped.filesystem_generation, expected);
                assert!(stopped.pending.is_empty());
            } else if delivery == 1 {
                assert!(
                    matches!(fleet.poll_lifecycle_event(1), Some(FleetEvent::Stopped { filesystem_generation, .. }) if filesystem_generation == expected)
                );
                assert!(fleet.consume_lifecycle_event(1));
            }
            assert!(matches!(
                next(&fleet),
                FleetEvent::ResourceSnapshot {
                    machine: 2,
                    request: 11,
                    ..
                }
            ));
            if delivery == 2 {
                assert!(
                    matches!(next(&fleet), FleetEvent::Stopped { filesystem_generation, .. } if filesystem_generation == expected)
                );
            }
            assert!(fleet.try_event().is_none());
        }
    }

    #[test]
    fn observes_resources_and_generation_on_the_machine_owner() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::two_block_artifact(1, 1),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let expected = machine.resource_snapshot();
        let generation = machine.filesystem_generation();
        let fleet = FleetScheduler::new(vec![(7, machine)], config()).unwrap();
        fleet.resource_snapshot(7, 11).unwrap();
        fleet.filesystem_generation(7, 12).unwrap();
        assert!(
            matches!(next(&fleet), FleetEvent::ResourceSnapshot { machine: 7, request: 11, snapshot } if snapshot == expected)
        );
        assert!(
            matches!(next(&fleet), FleetEvent::FilesystemGeneration { machine: 7, request: 12, generation: actual } if actual == generation)
        );
    }

    #[test]
    fn filesystem_queries_preserve_content_and_stale_generation_errors() {
        use crate::{
            ComputerFileSystem, FileCapability, FileRights, FileSystemLimits, VirtualPath,
        };
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(
            VirtualPath::parse_utf8("/home", &limits).unwrap(),
            FileRights::OWNER,
        );
        let path = VirtualPath::parse_utf8("/home/a", &limits).unwrap();
        let mut fs = ComputerFileSystem::with_limits(limits);
        fs.write_file(&owner, &path, b"hello", false).unwrap();
        let machine = ComputerMachine::start_in_filesystem(
            crate::execution::fixtures::two_block_artifact(1, 1),
            profile(),
            &[],
            &[],
            fs,
            owner,
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(7, machine)], config()).unwrap();
        fleet.file_stat(7, 1, "/home/a".into()).unwrap();
        let generation = match next(&fleet) {
            FleetEvent::Filesystem {
                machine: 7,
                request: 1,
                result: Ok(FilesystemResult::Stat(stat)),
            } => stat.metadata.generation,
            other => panic!("{other:?}"),
        };
        fleet.file_list(7, 2, "/home".into(), None, 32).unwrap();
        assert!(
            matches!(next(&fleet), FleetEvent::Filesystem { machine: 7, request: 2, result: Ok(FilesystemResult::List(list)) } if list.entries.iter().any(|entry| entry.name.as_ref() == "a"))
        );
        fleet
            .file_read(7, 3, "/home/a".into(), 0, 32, generation)
            .unwrap();
        assert!(
            matches!(next(&fleet), FleetEvent::Filesystem { machine: 7, request: 3, result: Ok(FilesystemResult::Read(chunk)) } if chunk.bytes.as_ref() == b"hello")
        );
        fleet
            .file_read(7, 4, "/home/a".into(), 0, 32, generation + 1)
            .unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Filesystem {
                machine: 7,
                request: 4,
                result: Err(crate::ComputerFileReadError::StaleGeneration { .. })
            }
        ));
    }

    #[test]
    fn redstone_input_wakes_a_waiting_guest() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::redstone_i32_artifact(1, true, &[0]),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(1, machine)], config()).unwrap();
        fleet.grant(1, 1, 512).unwrap();
        wait_advances(&fleet, 2);
        fleet.redstone_input(1, 1 | (9 << 6)).unwrap();
        for tick in 2..300 {
            fleet.grant(1, tick, 512).unwrap();
            thread::sleep(Duration::from_millis(1));
            if let Some(event) = fleet.try_event() {
                assert!(
                    matches!(
                        event,
                        FleetEvent::Boundary {
                            outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(9))),
                            ..
                        }
                    ),
                    "{event:?}"
                );
                return;
            }
        }
        panic!("redstone wait did not resume");
    }

    #[test]
    fn a_machine_with_a_redstone_wait_can_join_a_fleet() {
        let mut machine = ComputerMachine::start(
            crate::execution::fixtures::redstone_i32_artifact(1, true, &[0]),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        for _ in 0..2 {
            assert!(matches!(
                machine.advance(64, 64, 64).unwrap(),
                ComputerAdvanceOutcome::SliceExhausted
            ));
        }
        let fleet = FleetScheduler::new(vec![], config()).unwrap();
        let id = fleet.attach(Box::new(machine)).unwrap();
        fleet.redstone_input(id, 1 | (9 << 6)).unwrap();
        assert!(matches!(
            grant_until_terminal_outcome(&fleet, id),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(9))),
                ..
            }
        ));
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
            FleetEvent::Stopped {
                machine, pending, ..
            } => {
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
        assert_eq!(
            fleet.resource_snapshot(7, 1),
            Err(SubmitError::UnknownMachine)
        );
        match next(&fleet) {
            FleetEvent::Stopped {
                machine, pending, ..
            } => {
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
    fn timer_capacity_returns_guest_visible_failure() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::timer_unit_artifact(2),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let mut actor = Actor::new(machine, 64);
        for index in 0..256 {
            actor.timers.insert(
                (99, index),
                ScheduledTimer {
                    due: 1000,
                    duration: 2,
                },
            );
        }
        let commands = Arc::new(Mutex::new(LaneMailbox::new(4)));
        let worker_commands = Arc::clone(&commands);
        let (sender, events) = event_queue::channel(
            16,
            FleetMemoryLimits::default().event_bytes,
            FleetMemoryLimits::default().event_bytes_per_machine,
        );
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);
        let worker = thread::spawn(move || {
            run_worker(
                HashMap::from([(1, actor)]),
                worker_commands,
                EventSender {
                    sender,
                    signal: Arc::new(FleetEventSignal::default()),
                    completions: Arc::new(Mutex::new(BTreeMap::new())),
                    pending: super::pending::PendingCapacity::new(FleetMemoryLimits::default(), 64),
                    detach_memory: ByteBudget::new(
                        FleetMemoryLimits::default().detach_output_bytes,
                    ),
                    detach_machine_limit: FleetMemoryLimits::default()
                        .detach_output_bytes_per_machine,
                    detach_headroom: Arc::new(AtomicUsize::new(0)),
                },
                worker_stopped,
                Arc::new(Metrics::default()),
                Arc::new(AtomicUsize::new(0)),
                config(),
                Arc::new(Vec::new()),
                false,
            )
        });
        assert!(commands
            .lock()
            .unwrap()
            .submit(Command::Tick {
                machine: 1,
                tick: 1,
                retirements: 64,
                deadline: None,
            })
            .is_ok());
        worker.thread().unpark();
        let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
        match event {
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::UncaughtException(detail),
                ..
            } => {
                assert!(detail.contains("Timer pending request limit was reached"));
            }
            event => panic!("expected guest-visible timer capacity failure, got {event:?}"),
        }
        stopped.store(true, Ordering::Release);
        worker.thread().unpark();
        worker.join().unwrap();
    }

    #[test]
    fn quiescent_report_exposes_pending_timer_until_it_wakes() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::timer_unit_artifact(2),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new_with_coordinated_turns(
            vec![(1, machine)],
            config(),
            Vec::new(),
            true,
        )
        .unwrap();
        fleet.grant(1, 1, 64).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Quiescent {
                timers_pending: true,
                ..
            }
        ));
        fleet.grant(1, 3, 64).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
        assert!(matches!(
            next(&fleet),
            FleetEvent::Quiescent {
                timers_pending: false,
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
    fn detached_timer_preserves_remaining_ticks_and_rejects_invalid_metadata() {
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
        fleet.detach_with_timers(1, 2).unwrap();
        let (machine, timers) = match next(&fleet) {
            FleetEvent::Detached {
                computer, timers, ..
            } => (computer, timers),
            event => panic!("unexpected event {event:?}"),
        };
        assert_eq!(timers.len(), 1);
        assert_eq!(timers[0].duration, 2);
        assert_eq!(timers[0].remaining, 1);
        let mut invalid = timers.clone();
        invalid[0].duration = 3;
        let (reason, machine) = fleet.attach_with_timers(machine, 100, invalid).unwrap_err();
        assert_eq!(reason, SubmitError::InvalidMachine);
        let id = fleet.attach_with_timers(machine, 100, timers).unwrap();
        fleet.detach_with_timers(id, 99).unwrap();
        assert!(matches!(next(&fleet), FleetEvent::DetachRejected { machine } if machine == id));
        fleet.grant(id, 100, 64).unwrap();
        thread::sleep(Duration::from_millis(10));
        assert!(fleet.try_event().is_none());
        fleet.grant(id, 101, 64).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
    }

    #[test]
    fn detach_with_timers_refuses_a_tick_earlier_than_the_last_grant() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::timer_unit_artifact(2),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new(vec![(1, machine)], config()).unwrap();
        fleet.grant(1, 10, 64).unwrap();
        wait_advances(&fleet, 2);
        fleet.detach_with_timers(1, 9).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::DetachRejected { machine: 1 }
        ));
        fleet.grant(1, 12, 64).unwrap();
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
    fn stop_completion_does_not_consume_another_machines_event() {
        let machines = (0..2)
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
                workers: 1,
                event_capacity: 1,
                ..config()
            },
        )
        .unwrap();
        let signal = fleet.event_signal();
        let generation = signal.generation();
        fleet.resource_snapshot(0, 41).unwrap();
        assert!(signal.wait(generation, Duration::from_secs(5)));
        fleet.stop(1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(event) = fleet.try_stopped(1) {
                assert_eq!(event.machine, 1);
                break;
            }
            assert!(Instant::now() < deadline, "stop completion did not arrive");
            thread::yield_now();
        }
        assert!(matches!(
            fleet.try_event(),
            Some(FleetEvent::ResourceSnapshot {
                machine: 0,
                request: 41,
                ..
            })
        ));
        assert!(fleet.try_event().is_none());
        assert_eq!(fleet.stop(1), Err(SubmitError::UnknownMachine));
    }

    #[test]
    fn stop_keeps_final_accounting_when_the_event_queue_is_full() {
        for (targeted, already_reported) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let computer = ComputerMachine::start(
                crate::execution::fixtures::two_block_artifact(1, 1),
                profile(),
                &[],
                &[],
            )
            .unwrap();
            let mut actor = Actor::new(computer, config().host_budget);
            if already_reported {
                actor.tick_initialized = true;
                actor.tick = 7;
                actor.tick_retired = 9;
                actor.reported_retired = Some(9);
            }
            let actors = HashMap::from([(1, actor)]);
            let mailbox = Arc::new(Mutex::new(LaneMailbox::new(4)));
            if !already_reported {
                assert!(mailbox
                    .lock()
                    .unwrap()
                    .submit(Command::Tick {
                        machine: 1,
                        tick: 6,
                        retirements: 64,
                        deadline: None,
                    })
                    .is_ok());
                assert!(mailbox
                    .lock()
                    .unwrap()
                    .submit(Command::Tick {
                        machine: 1,
                        tick: 7,
                        retirements: 64,
                        deadline: None,
                    })
                    .is_ok());
            }
            mailbox.lock().unwrap().closes.insert(1, CloseRequest::Stop);
            let (event_tx, events) = event_queue::channel(
                1,
                FleetMemoryLimits::default().event_bytes,
                FleetMemoryLimits::default().event_bytes_per_machine,
            );
            event_tx
                .try_send(FleetEvent::Error {
                    machine: 0,
                    message: "occupied".into(),
                })
                .unwrap();
            let signal = Arc::new(FleetEventSignal::default());
            let completions = Arc::new(Mutex::new(BTreeMap::new()));
            let stopped = Arc::new(AtomicBool::new(false));
            let metrics = Arc::new(Metrics::default());
            let waiters = Arc::new(AtomicUsize::new(0));
            let sender = EventSender {
                sender: event_tx,
                signal: Arc::clone(&signal),
                completions: Arc::clone(&completions),
                pending: super::pending::PendingCapacity::new(FleetMemoryLimits::default(), 64),
                detach_memory: ByteBudget::new(FleetMemoryLimits::default().detach_output_bytes),
                detach_machine_limit: FleetMemoryLimits::default().detach_output_bytes_per_machine,
                detach_headroom: Arc::new(AtomicUsize::new(0)),
            };
            let worker_stopped = Arc::clone(&stopped);
            let worker_metrics = Arc::clone(&metrics);
            let worker_waiters = Arc::clone(&waiters);
            let worker_mailbox = Arc::clone(&mailbox);
            let pending_capacity = Arc::clone(&sender.pending);
            let worker = thread::spawn(move || {
                run_worker(
                    actors,
                    worker_mailbox,
                    sender,
                    worker_stopped,
                    worker_metrics,
                    worker_waiters,
                    config(),
                    Arc::new(Vec::new()),
                    true,
                )
            });
            let command_memory = Arc::clone(&mailbox.lock().unwrap().memory);
            let fleet = FleetScheduler {
                lanes: vec![mailbox],
                owners: Mutex::new(Owners {
                    lanes: HashMap::from([(1, 0)]),
                    closing: HashMap::from([(1, LifecycleReader::Ordinary)]),
                    next_id: Some(2),
                    next_lane: 0,
                }),
                events,
                completions,
                event_signal: signal,
                stopped,
                workers: vec![worker],
                metrics,
                waiting_publishers: waiters,
                command_memory,
                pending: pending_capacity,
                detach_memory: ByteBudget::new(FleetMemoryLimits::default().detach_output_bytes),
                detach_headroom: Arc::new(AtomicUsize::new(0)),
            };
            let deadline = Instant::now() + Duration::from_secs(5);
            while fleet.completions.lock().unwrap().is_empty() {
                assert!(
                    Instant::now() < deadline,
                    "stop blocked on final accounting"
                );
                thread::yield_now();
            }
            if targeted {
                let completion = fleet.try_stopped(1).unwrap();
                assert_eq!(
                    completion
                        .prior_reports
                        .iter()
                        .map(|report| report.tick)
                        .collect::<Vec<_>>(),
                    if already_reported {
                        Vec::new()
                    } else {
                        vec![6]
                    }
                );
                let report = completion.quiescent.unwrap();
                assert_eq!(
                    (completion.machine, report.tick, report.retired),
                    (1, 7, if already_reported { 9 } else { 0 })
                );
                assert!(matches!(
                    fleet.try_event(),
                    Some(FleetEvent::Error { machine: 0, .. })
                ));
            } else {
                assert!(matches!(
                    fleet.try_event(),
                    Some(FleetEvent::Error { machine: 0, .. })
                ));
                if !already_reported {
                    assert!(matches!(
                        fleet.try_event(),
                        Some(FleetEvent::Quiescent {
                            machine: 1,
                            tick: 6,
                            retired: 0,
                            ..
                        })
                    ));
                    assert!(matches!(
                        fleet.try_event(),
                        Some(FleetEvent::Quiescent {
                            machine: 1,
                            tick: 7,
                            retired: 0,
                            ..
                        })
                    ));
                }
                assert!(matches!(
                    fleet.try_event(),
                    Some(FleetEvent::Stopped { machine: 1, .. })
                ));
            }
        }
    }

    #[test]
    fn stop_fence_prevents_new_guest_execution_while_draining_commands() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::two_block_artifact(1, 1),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let mailbox = Arc::new(Mutex::new(LaneMailbox::new(8)));
        for tick in 1..=5 {
            assert!(mailbox
                .lock()
                .unwrap()
                .submit(Command::Tick {
                    machine: 1,
                    tick,
                    retirements: 64,
                    deadline: None,
                })
                .is_ok());
        }
        mailbox.lock().unwrap().closes.insert(1, CloseRequest::Stop);
        let (event_tx, _events) = event_queue::channel(
            8,
            FleetMemoryLimits::default().event_bytes,
            FleetMemoryLimits::default().event_bytes_per_machine,
        );
        let completions = Arc::new(Mutex::new(BTreeMap::new()));
        let sender = EventSender {
            sender: event_tx,
            signal: Arc::new(FleetEventSignal::default()),
            completions: Arc::clone(&completions),
            pending: super::pending::PendingCapacity::new(FleetMemoryLimits::default(), 64),
            detach_memory: ByteBudget::new(FleetMemoryLimits::default().detach_output_bytes),
            detach_machine_limit: FleetMemoryLimits::default().detach_output_bytes_per_machine,
            detach_headroom: Arc::new(AtomicUsize::new(0)),
        };
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);
        let metrics = Arc::new(Metrics::default());
        let worker_metrics = Arc::clone(&metrics);
        let worker = thread::spawn(move || {
            run_worker(
                HashMap::from([(1, Actor::new(machine, config().host_budget))]),
                mailbox,
                sender,
                worker_stopped,
                worker_metrics,
                Arc::new(AtomicUsize::new(0)),
                config(),
                Arc::new(Vec::new()),
                true,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while completions.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "stop did not finish");
            thread::yield_now();
        }
        assert_eq!(metrics.advances.load(Ordering::Relaxed), 0);
        let LifecycleCompletion::Stop(completion) = completions
            .lock()
            .unwrap()
            .remove(&1)
            .unwrap()
            .into_completion()
        else {
            panic!("expected stop")
        };
        assert_eq!(completion.quiescent.unwrap().tick, 5);
        assert_eq!(
            completion
                .prior_reports
                .iter()
                .map(|report| report.tick)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        stopped.store(true, Ordering::Release);
        worker.thread().unpark();
        worker.join().unwrap();
    }

    #[test]
    fn command_memory_is_shared_bounded_and_held_during_execution() {
        fn command(machine: u64) -> Command {
            Command::Reply {
                machine,
                task: 1,
                request: 1,
                value: ReplyValue::Bytes(vec![7; 1024].into_boxed_slice()),
            }
        }
        let charge = std::mem::size_of::<QueuedCommand>() + 1024;
        let memory = ByteBudget::new(2 * charge);
        let mut first = LaneMailbox::with_memory(8, Arc::clone(&memory), charge);
        let mut second = LaneMailbox::with_memory(8, Arc::clone(&memory), charge);
        assert!(first.submit(command(1)).is_ok());
        assert!(
            first.submit(command(1)).is_err(),
            "per-machine limit was not applied"
        );
        assert_eq!(
            memory.snapshot().retained_bytes,
            charge,
            "rejection leaked the global reservation"
        );
        assert!(second.submit(command(2)).is_ok());
        assert!(
            second.submit(command(3)).is_err(),
            "lanes exceeded their shared byte limit"
        );
        let executing = first.next().unwrap();
        assert_eq!(memory.snapshot().retained_bytes, 2 * charge);
        assert!(
            first.submit(command(1)).is_err(),
            "in-flight command escaped its machine limit"
        );
        drop(executing);
        assert_eq!(memory.snapshot().retained_bytes, charge);
        assert!(first.submit(command(1)).is_ok());
        drop(first);
        drop(second);
        assert_eq!(memory.snapshot().retained_bytes, 0);
        assert!(memory.snapshot().peak_bytes <= memory.snapshot().limit_bytes);
    }

    #[test]
    fn stop_with_a_full_mailbox_preserves_other_machines_observations() {
        let machines = (0..2)
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
        let limit = 3 * std::mem::size_of::<QueuedCommand>();
        let fleet = FleetScheduler::new_with_memory_limits(
            machines,
            FleetConfig {
                workers: 1,
                event_capacity: 1,
                mailbox_capacity: 3,
                ..config()
            },
            Vec::new(),
            false,
            FleetMemoryLimits {
                pending_output_bytes: 1,
                command_bytes: limit,
                command_bytes_per_machine: limit,
                ..FleetMemoryLimits::default()
            },
        )
        .unwrap();
        fleet.grant(0, 1, 64).unwrap();
        fleet.grant(1, 1, 64).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fleet.waiting_publishers.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "publisher did not block");
            thread::yield_now();
        }
        fleet.resource_snapshot(0, 41).unwrap();
        fleet.resource_snapshot(1, 42).unwrap();
        fleet.resource_snapshot(0, 43).unwrap();
        assert_eq!(fleet.lanes[0].lock().unwrap().commands.len(), 3);
        assert_eq!(fleet.command_memory().retained_bytes, limit);
        fleet.stop(1).unwrap();

        let mut other_snapshot = false;
        let mut later_snapshot = false;
        let mut stop = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !(other_snapshot && later_snapshot && stop) {
            if let Some(event) = fleet.try_event() {
                match event {
                    FleetEvent::ResourceSnapshot {
                        machine: 0,
                        request: 41,
                        ..
                    } => other_snapshot = true,
                    FleetEvent::ResourceSnapshot {
                        machine: 0,
                        request: 43,
                        ..
                    } => {
                        assert!(
                            other_snapshot,
                            "commands for the other machine were reordered"
                        );
                        later_snapshot = true;
                    }
                    FleetEvent::Stopped { machine: 1, .. } => {
                        stop = true;
                    }
                    _ => {}
                }
            } else {
                assert!(
                    Instant::now() < deadline,
                    "accepted command or stop was lost"
                );
                thread::yield_now();
            }
        }
    }

    #[test]
    fn stopping_one_machine_does_not_fence_other_mailbox_submissions() {
        let mut mailbox = LaneMailbox::new(2);
        mailbox.closes.insert(1, CloseRequest::Stop);
        assert!(mailbox
            .submit(Command::ResourceSnapshot {
                machine: 2,
                request: 42
            })
            .is_ok());
        assert!(matches!(
            mailbox.next().map(|queued| queued.command),
            Some(Command::Stop { machine: 1 })
        ));
        assert!(matches!(
            mailbox.next().map(|queued| queued.command),
            Some(Command::ResourceSnapshot {
                machine: 2,
                request: 42
            })
        ));
    }

    #[test]
    fn saturated_fleet_can_stop_every_machine_without_event_pumping() {
        let machines = (0..32)
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
        let fleet = FleetScheduler::new_with_coordinated_turns(
            machines,
            FleetConfig {
                workers: 2,
                event_capacity: 1,
                ..config()
            },
            Vec::new(),
            true,
        )
        .unwrap();
        for id in 0..32 {
            fleet.grant(id, 1, 64).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while fleet.waiting_publishers.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "event output did not saturate");
            thread::yield_now();
        }
        for id in 0..32 {
            fleet.stop(id).unwrap();
        }
        let mut remaining: HashSet<_> = (0..32).collect();
        while !remaining.is_empty() {
            remaining.retain(|&id| fleet.try_stopped(id).is_none());
            assert!(
                Instant::now() < deadline,
                "stop storm stalled: {remaining:?}"
            );
            thread::yield_now();
        }
        assert_eq!(fleet.waiting_publishers.load(Ordering::SeqCst), 0);
        assert!(fleet.owners.lock().unwrap().lanes.is_empty());
    }

    #[test]
    fn stopped_machine_completes_while_another_event_publisher_is_blocked() {
        let machines = (0..2)
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
                workers: 1,
                event_capacity: 1,
                ..config()
            },
        )
        .unwrap();
        fleet.grant(0, 1, 64).unwrap();
        fleet.grant(1, 1, 64).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fleet.waiting_publishers.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "publisher did not block");
            thread::yield_now();
        }
        fleet.stop(0).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(event) = fleet.try_stopped(0) {
                assert_eq!(event.machine, 0);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "stop was blocked by another machine's event"
            );
            thread::yield_now();
        }
        // Closing machine 0 may cancel its undelivered events, but it must
        // never consume or discard the event belonging to machine 1.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if matches!(
                fleet.try_event(),
                Some(FleetEvent::Boundary { machine: 1, .. })
            ) {
                break;
            }
            assert!(Instant::now() < deadline, "unrelated event was lost");
            thread::yield_now();
        }
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
    fn releasing_a_pause_during_selection_cannot_reorder_a_machines_commands() {
        let mut mailbox = LaneMailbox::new(8);
        let resume = Arc::new(AtomicBool::new(false));
        mailbox.paused.insert(1, Arc::clone(&resume));
        for (machine, request) in [(1, 10), (2, 20), (1, 11)] {
            assert!(mailbox
                .submit(Command::ResourceSnapshot { machine, request })
                .is_ok());
        }
        // A host may acknowledge a refusal between two candidates in one
        // mailbox scan. Both commands of VM 1 must use the same paused state.
        let selected = mailbox.next_eligible(None, |machine| {
            if machine == 2 {
                resume.store(true, Ordering::Release);
            }
            machine == 2
        });
        assert!(
            selected.is_none(),
            "later command overtook its paused predecessor"
        );
        for expected in [10, 11] {
            assert!(
                matches!(mailbox.next_eligible(None, |machine| machine == 2).map(|queued| queued.command),
                Some(Command::ResourceSnapshot { machine: 1, request }) if request == expected)
            );
        }
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
        let fleet = FleetScheduler::new_with_coordinated_turns(
            vec![(1, machine)],
            config(),
            Vec::new(),
            true,
        )
        .unwrap();
        for tick in [0, 0, 1] {
            fleet.grant(1, tick, 1).unwrap();
            assert!(
                matches!(next(&fleet), FleetEvent::Quiescent { machine: 1, tick: observed, retired: 1, .. } if observed == tick)
            );
            assert_eq!(fleet.metrics().retired, tick + 1);
        }
    }

    #[test]
    fn retirement_reports_are_per_machine_and_per_tick() {
        let make = || {
            ComputerMachine::start(
                crate::execution::fixtures::empty_loop_artifact(1),
                profile(),
                &[],
                &[],
            )
            .unwrap()
        };
        let fleet = FleetScheduler::new_with_coordinated_turns(
            vec![(1, make()), (2, make())],
            config(),
            Vec::new(),
            true,
        )
        .unwrap();
        fleet.grant(1, 3, 2).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Quiescent {
                machine: 1,
                tick: 3,
                retired: 2,
                ..
            }
        ));
        fleet.grant(2, 3, 1).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Quiescent {
                machine: 2,
                tick: 3,
                retired: 1,
                ..
            }
        ));
        fleet.grant(1, 4, 1).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Quiescent {
                machine: 1,
                tick: 4,
                retired: 1,
                ..
            }
        ));
    }

    #[test]
    fn retirement_reports_include_continuation_in_the_same_tick() {
        let ops = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let binding = CapabilityBinding::new("app", "entry", 1, 2, &ops);
        let machine = ComputerMachine::start(
            crate::execution::fixtures::capability_artifact(true, true, 1, 0),
            profile(),
            &[binding],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new_with_coordinated_turns(
            vec![(7, machine)],
            config(),
            Vec::new(),
            true,
        )
        .unwrap();
        fleet.grant(7, 1, 64).unwrap();
        let request = match next(&fleet) {
            FleetEvent::Requests { requests, .. } => requests[0].clone(),
            other => panic!("{other:?}"),
        };
        let before = match next(&fleet) {
            FleetEvent::Quiescent {
                machine: 7,
                tick: 1,
                retired,
                ..
            } => retired,
            other => panic!("{other:?}"),
        };
        fleet
            .reply(7, request.task_id, request.id, ReplyValue::Unit)
            .unwrap();
        fleet.terminal_update(7, 99, u64::MAX).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::TerminalUpdate {
                machine: 7,
                request: 99,
                ..
            }
        ));
        assert_eq!(fleet.metrics().retired, before);
        fleet.grant(7, 1, 64).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(_),
                ..
            }
        ));
        match next(&fleet) {
            FleetEvent::Quiescent {
                machine: 7,
                tick: 1,
                retired,
                ..
            } => assert!(retired > before),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn expired_deadline_reports_zero_retirements() {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::empty_loop_artifact(1),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new_with_coordinated_turns(
            vec![(1, machine)],
            config(),
            Vec::new(),
            true,
        )
        .unwrap();
        fleet.grant_until(1, 1, 10, Some(Instant::now())).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Quiescent {
                machine: 1,
                tick: 1,
                retired: 0,
                ..
            }
        ));
        fleet.grant(1, 2, 1).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Quiescent {
                machine: 1,
                tick: 2,
                retired: 1,
                ..
            }
        ));
    }

    #[test]
    fn checkpoint_detach_preserves_exact_compiler_input_without_completion() {
        use crate::{
            ComputerFileSystem, FileCapability, FileRights, FileSystemLimits, VirtualPath,
        };
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(
            VirtualPath::parse_utf8("/home", &limits).unwrap(),
            FileRights::OWNER,
        );
        let source = VirtualPath::parse_utf8("/home/main.kt", &limits).unwrap();
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        filesystem
            .write_file(&owner, &source, b"fun main() = 42", false)
            .unwrap();
        let artifact = crate::execution::fixtures::compiler_compile_artifact(
            &"/home/main.kt".encode_utf16().collect::<Vec<_>>(),
            &"/home/main".encode_utf16().collect::<Vec<_>>(),
        );
        let machine =
            ComputerMachine::start_in_filesystem(artifact, profile(), &[], &[], filesystem, owner)
                .unwrap();
        let fleet = FleetScheduler::new(vec![(7, machine)], config()).unwrap();
        fleet.grant(7, 1, 256).unwrap();
        let request = match next(&fleet) {
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::CompilationRequested(request),
                ..
            } => request,
            other => panic!("{other:?}"),
        };

        fleet.detach_for_checkpoint(7, 1).unwrap();
        wait_lifecycle(&fleet, 7);
        let FleetDetachResult::Detached { computer, timers } =
            fleet.try_detached(7).unwrap().result
        else {
            panic!("compiler wait blocked checkpoint")
        };
        assert!(timers.is_empty());
        let id = crate::ComputerId::from_bytes([7; 16]);
        let bytes = computer
            .checkpoint(
                id,
                b"host-state",
                crate::ComputerCheckpointLimits::default(),
            )
            .unwrap();
        let limits = FileSystemLimits::testing();
        let capability = FileCapability::new(
            VirtualPath::parse_utf8("/home", &limits).unwrap(),
            FileRights::OWNER,
        );
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        filesystem
            .write_file(&capability, &source, b"fun main() = 42", false)
            .unwrap();
        let (mut computer, host_state) = ComputerMachine::restore_checkpoint(
            crate::ComputerRestoreEnvironment {
                id,
                profile: profile(),
                process_limits: crate::ProcessLimits::default(),
                addon_bindings: &[],
                filesystem,
                initial_file_capability: capability,
            },
            &bytes,
            crate::ComputerCheckpointLimits::default(),
        )
        .unwrap();
        assert_eq!(host_state, b"host-state");
        let pending = computer
            .pending_compilation_request()
            .expect("compiler input was lost");
        assert_eq!(request, pending);
        computer
            .complete_compilation_failure(pending.token, "resubmitted after restoration")
            .unwrap();
        assert!(matches!(
            computer.advance(64, 64, 64).unwrap(),
            ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(1)))
        ));
    }

    #[test]
    fn compilation_is_parked_until_the_matching_result_arrives() {
        use crate::{
            ComputerFileSystem, FileCapability, FileRights, FileSystemLimits, VirtualPath,
        };
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(
            VirtualPath::parse_utf8("/home", &limits).unwrap(),
            FileRights::OWNER,
        );
        let source = VirtualPath::parse_utf8("/home/main.kt", &limits).unwrap();
        let output = VirtualPath::parse_utf8("/home/main", &limits).unwrap();
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        filesystem
            .write_file(&owner, &source, b"fun main() = 42", false)
            .unwrap();
        let artifact = crate::execution::fixtures::compiler_compile_artifact(
            &"/home/main.kt".encode_utf16().collect::<Vec<_>>(),
            &"/home/main".encode_utf16().collect::<Vec<_>>(),
        );
        let machine =
            ComputerMachine::start_in_filesystem(artifact, profile(), &[], &[], filesystem, owner)
                .unwrap();
        let fleet = FleetScheduler::new(vec![(7, machine)], config()).unwrap();
        fleet.grant(7, 1, 256).unwrap();
        let request = match next(&fleet) {
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::CompilationRequested(request),
                ..
            } => request,
            other => panic!("{other:?}"),
        };
        assert_eq!(request.sources[0].utf8.as_ref(), b"fun main() = 42");
        let advances = fleet.metrics().advances;
        fleet
            .terminal_key(
                7,
                crate::TerminalKeyEvent::new(
                    crate::TerminalKey::Enter,
                    crate::TerminalKeyAction::Press,
                    crate::TerminalModifiers::default(),
                ),
            )
            .unwrap();
        fleet
            .complete_compilation(
                7,
                request.token + 1,
                CompilationResult::Failure("stale".into()),
            )
            .unwrap();
        fleet.detach(7).unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::DetachRejected { machine: 7 }
        ));
        assert_eq!(fleet.metrics().advances, advances);
        let compiled = crate::execution::fixtures::two_block_artifact(1, 1);
        let bytes = crate::test_encode::encode_artifact(compiled.decoded()).unwrap();
        fleet
            .complete_compilation(
                7,
                request.token,
                CompilationResult::Artifact(bytes.clone().into_boxed_slice()),
            )
            .unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(0))),
                ..
            }
        ));
        fleet.detach(7).unwrap();
        let computer = match next(&fleet) {
            FleetEvent::Detached { computer, .. } => computer,
            other => panic!("{other:?}"),
        };
        assert!(
            computer
                .filesystem_stat(&output)
                .unwrap()
                .metadata
                .executable
        );
    }
}
