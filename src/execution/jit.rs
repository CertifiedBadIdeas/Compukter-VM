/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

//! Experimental shared baseline tier. No lifecycle work or native pointer enters Guest state.
use std::sync::{
    atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
    mpsc::{sync_channel, SyncSender},
    Arc, OnceLock, Weak,
};

use super::{
    error::{GuestTrap, VmFault},
    frame::PreparedFrame,
    machine::InstructionFailure,
    prepared::{PreparedCode, RunBudget, RunEnd, SimpleRun},
};

mod lower;
mod resources;
use resources::{Denied, ProgramBudget, Reservation, Scope, MODULE_BYTES};
#[cfg(feature = "guest-benchmark")]
pub(crate) fn benchmark_ssa(enabled: bool) {
    lower::slots::benchmark_enabled(enabled);
}
#[cfg(feature = "guest-benchmark")]
pub(crate) fn benchmark_block_budget(enabled: bool) {
    lower::benchmark_block_budget(enabled);
}
pub(super) use lower::RunState;
const SLOTS: usize = 256;
const HOT: u32 = 64;
const MAX_OPS: usize = 128;
const EMPTY: usize = usize::MAX;
const COLD: u64 = 0;
const QUEUED: u64 = 1;
const FINISHED: u64 = 2;
const WAIT_GLOBAL: u64 = 3;
const WAIT_PROGRAM: u64 = 4;

#[derive(Debug)]
struct Slot {
    key: AtomicUsize,
    // One atomic word prevents a late cold visitor corrupting a waiting epoch.
    // [epoch/visits:32][unused:20][queue backoff:4][state:8]
    progress: AtomicU64,
    native: OnceLock<Arc<lower::CompiledRun>>,
}

#[cfg(feature = "jit-diagnostics")]
pub(super) mod calls;
#[cfg(feature = "jit-diagnostics")]
pub(super) use calls::record_call;

#[derive(Debug)]
pub(super) struct JitState {
    slots: Box<[Slot]>,
    budget: Arc<ProgramBudget>,
    hits: AtomicU64,
    attempts: AtomicU64,
    compile_ns: AtomicU64,
    failures: AtomicU64,
    #[cfg(feature = "jit-diagnostics")]
    exits: [AtomicU64; 6],
    #[cfg(feature = "jit-diagnostics")]
    calls: std::sync::Mutex<std::collections::BTreeMap<(usize, usize, usize), u64>>,
}

impl Default for JitState {
    fn default() -> Self {
        Self {
            slots: (0..SLOTS)
                .map(|_| Slot {
                    key: AtomicUsize::new(EMPTY),
                    progress: AtomicU64::new(COLD),
                    native: OnceLock::new(),
                })
                .collect(),
            budget: Arc::new(ProgramBudget::default()),
            hits: AtomicU64::new(0),
            attempts: AtomicU64::new(0),
            compile_ns: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            #[cfg(feature = "jit-diagnostics")]
            exits: std::array::from_fn(|_| AtomicU64::new(0)),
            #[cfg(feature = "jit-diagnostics")]
            calls: Default::default(),
        }
    }
}

impl JitState {
    #[inline]
    fn record_native(&self, attempts: u64) {
        // Hot telemetry is diagnostic work. Updating one shared cache line on
        // every native exit serializes otherwise independent execution workers.
        #[cfg(any(test, feature = "jit-telemetry"))]
        {
            self.hits.fetch_add(1, Ordering::Relaxed);
            self.attempts.fetch_add(attempts, Ordering::Relaxed);
        }
        #[cfg(not(any(test, feature = "jit-telemetry")))]
        let _ = attempts;
    }
}

impl Slot {
    fn status(&self) -> u64 {
        self.progress.load(Ordering::Acquire) & 0xff
    }

    fn try_queue(&self, budget: &ProgramBudget) -> bool {
        let before = self.progress.load(Ordering::Acquire);
        let state = before & 0xff;
        let backoff = before & 0xf00;
        let counter = (before >> 32) as u32;
        let after = match state {
            COLD => {
                let visits = counter + 1;
                if visits < HOT << (backoff >> 8) {
                    (u64::from(visits) << 32) | backoff | COLD
                } else {
                    backoff | QUEUED
                }
            }
            WAIT_GLOBAL | WAIT_PROGRAM => {
                let scope = if state == WAIT_GLOBAL {
                    Scope::Global
                } else {
                    Scope::Program
                };
                if budget.epoch(scope) == counter {
                    return false;
                }
                backoff | COLD
            }
            _ => return false,
        };
        self.progress
            .compare_exchange(before, after, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && after & 0xff == QUEUED
    }

    fn defer(&self, denied: Denied) {
        let backoff = self.progress.load(Ordering::Relaxed) & 0xf00;
        let state = match denied.scope {
            Scope::Global => WAIT_GLOBAL,
            Scope::Program => WAIT_PROGRAM,
        };
        self.progress.store(
            (u64::from(denied.epoch) << 32) | backoff | state,
            Ordering::Release,
        );
    }

    fn retry_queue(&self) {
        // Back off to at most 16K visits, without permanently rejecting a burst.
        let retries = ((self.progress.load(Ordering::Relaxed) >> 8) & 0xf)
            .saturating_add(1)
            .min(8);
        self.progress
            .store((retries << 8) | COLD, Ordering::Release);
    }
}

struct Job {
    owner: Weak<JitState>,
    slot: usize,
    plan: lower::Plan,
    reservation: Reservation,
}

fn compiler() -> Option<&'static SyncSender<Job>> {
    static COMPILER: OnceLock<Option<SyncSender<Job>>> = OnceLock::new();
    COMPILER
        .get_or_init(|| {
            let (sender, receiver) = sync_channel::<Job>(8);
            std::thread::Builder::new()
                .name("compukter-jit".into())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        if job.owner.strong_count() == 0 {
                            continue;
                        }
                        let start = std::time::Instant::now();
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            lower::compile(job.plan, job.reservation)
                        }))
                        .unwrap_or_else(|_| Err("compiler panicked".into()));
                        // Do not pin an otherwise dead program during compilation.
                        if let Some(owner) = job.owner.upgrade() {
                            owner.compile_ns.fetch_add(
                                start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                                Ordering::Relaxed,
                            );
                            match result {
                                Ok(run) => {
                                    let _ = owner.slots[job.slot].native.set(Arc::new(run));
                                }
                                Err(_) => {
                                    owner.failures.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            owner.slots[job.slot]
                                .progress
                                .store(FINISHED, Ordering::Release);
                        }
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}

fn slot_index(key: usize) -> usize {
    // Mix high bits as block starts need not be uniformly distributed.
    let mixed = (key as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    (mixed ^ (mixed >> 32)) as usize % SLOTS
}

pub(super) struct Location {
    pub block: Option<usize>,
    pub code_start: usize,
    pub instruction: usize,
    pub block_len: usize,
}

pub(super) struct HeapAccess<'a> {
    pub image: &'a super::image::ExecutionImage,
    pub heap: &'a mut super::heap::Heap,
}

pub(super) fn try_execute(
    code: &PreparedCode,
    frame: &mut PreparedFrame<'_>,
    location: Location,
    budget: &RunBudget,
    heap: Option<HeapAccess<'_>>,
    region_state: impl FnOnce() -> Option<RunState>,
) -> Option<Execution> {
    let Location {
        block,
        code_start,
        instruction,
        block_len,
    } = location;
    if budget.limit == 0 || budget.headroom == 0 {
        return None;
    }
    let key = code_start.checked_add(instruction)?;
    let owner = code.jit.get_or_init(|| Arc::new(JitState::default()));
    let index = slot_index(key);
    let slot = &owner.slots[index];
    let claimed = slot.key.load(Ordering::Acquire);
    if claimed != key
        && (claimed != EMPTY
            || slot
                .key
                .compare_exchange(EMPTY, key, Ordering::AcqRel, Ordering::Acquire)
                .is_err_and(|actual| actual != key))
    {
        return None;
    }
    if let Some(native) = slot.native.get() {
        if native.is_region() {
            if frame.jit_bytes().len() < native.required_bytes as usize {
                return None;
            }
            let state = region_state()?;
            let heap = heap?;
            #[cfg(feature = "jit-diagnostics")]
            let image = heap.image;
            let execution = native.execute_region(
                frame,
                HeapAccess {
                    image: heap.image,
                    heap: &mut *heap.heap,
                },
                state,
            );
            let native_attempts = execution.native_attempts;
            let result = execution.finish(frame, heap);
            #[cfg(feature = "jit-diagnostics")]
            {
                let category = if result.failure.is_some() {
                    4
                } else if result.exhausted {
                    5
                } else {
                    let block = image
                        .block(result.state.block as usize)
                        .expect("verified region exit");
                    if result.state.instruction as usize >= block.instruction_count {
                        1
                    } else if code
                        .operation(block.code_start + result.state.instruction as usize)
                        .is_some_and(|op| op.opcode == 0)
                    {
                        2
                    } else {
                        3
                    }
                };
                owner.exits[category].fetch_add(1, Ordering::Relaxed);
            }
            owner.record_native(native_attempts);
            return Some(Execution::Region(result));
        }
        if native.len > block_len.checked_sub(instruction)?
            || native.len as u64 > budget.limit.min(budget.headroom)
            || frame.jit_bytes().len() < native.required_bytes as usize
        {
            return None;
        }
        // This borrow keeps the program, slot and executable module alive through the call.
        let result = native.execute(frame, instruction, heap);
        #[cfg(feature = "jit-diagnostics")]
        {
            // [branch, end-of-block, unsupported lifecycle, supported suffix, failure].
            let category = match &result.end {
                RunEnd::Branch(_) => 0,
                RunEnd::Boundary if result.instruction >= block_len => 1,
                RunEnd::Boundary
                    if code
                        .operation(code_start + result.instruction)
                        .is_some_and(|op| op.opcode == 0) =>
                {
                    2
                }
                RunEnd::Boundary => 3,
                RunEnd::Failure(_) => 4,
            };
            owner.exits[category].fetch_add(1, Ordering::Relaxed);
        }
        owner.record_native(result.attempts);
        return Some(Execution::Prefix(result));
    }
    if !slot.try_queue(&owner.budget) {
        return None;
    }
    let reservation = match Reservation::try_acquire(owner.budget.clone()) {
        Ok(reservation) => reservation,
        Err(denied) => {
            slot.defer(denied);
            return None;
        }
    };
    let plan = block
        .filter(|_| instruction == 0)
        .and_then(|block| {
            heap.as_ref()
                .and_then(|access| lower::Plan::region(access.image, block))
        })
        .or_else(|| lower::Plan::read(code, key, block_len.saturating_sub(instruction)));
    let submitted = plan.and_then(|plan| {
        Some(compiler()?.try_send(Job {
            owner: Arc::downgrade(owner),
            slot: index,
            plan,
            reservation,
        }))
    });
    match submitted {
        Some(Ok(())) => {}
        Some(Err(std::sync::mpsc::TrySendError::Full(_))) => slot.retry_queue(),
        _ => slot.progress.store(FINISHED, Ordering::Release),
    }
    None
}

pub(super) enum Execution {
    Prefix(SimpleRun),
    Region(lower::RegionRun),
}

#[cfg(feature = "guest-benchmark")]
pub(crate) fn benchmark_quiet_passes() -> u32 {
    HOT + 1
}

#[cfg(feature = "guest-benchmark")]
pub(crate) fn benchmark_resources() -> (usize, usize, usize) {
    let (usage, _) = ProgramBudget::default().usage();
    (usage.modules, usage.bytes, usage.pending)
}

/// Benchmark telemetry is shared per program, not retained per machine.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Statistics {
    pub tracked: usize,
    pub compiled: usize,
    pub regions: usize,
    pub pending: usize,
    #[cfg(feature = "guest-benchmark")]
    pub wait_global: usize,
    #[cfg(feature = "guest-benchmark")]
    pub wait_program: usize,
    #[cfg(feature = "guest-benchmark")]
    pub reserved_bytes: usize,
    #[cfg(feature = "guest-benchmark")]
    pub admission_epoch: u32,
    pub code_bytes: usize,
    pub mapped_bytes: usize,
    pub hot_telemetry: bool,
    pub hits: u64,
    pub attempts: u64,
    pub compile_ns: u64,
    pub failures: u64,
    pub state_bytes: usize,
    #[cfg(feature = "jit-diagnostics")]
    pub exits: [u64; 6],
}
pub(super) fn statistics(code: &PreparedCode) -> Statistics {
    let Some(owner) = code.jit.get() else {
        return Statistics::default();
    };
    let mut result = Statistics {
        #[cfg(feature = "jit-diagnostics")]
        exits: std::array::from_fn(|i| owner.exits[i].load(Ordering::Relaxed)),
        hot_telemetry: cfg!(any(test, feature = "jit-telemetry")),
        hits: owner.hits.load(Ordering::Relaxed),
        attempts: owner.attempts.load(Ordering::Relaxed),
        compile_ns: owner.compile_ns.load(Ordering::Relaxed),
        failures: owner.failures.load(Ordering::Relaxed),
        state_bytes: std::mem::size_of::<JitState>()
            + SLOTS * std::mem::size_of::<Slot>()
            + std::mem::size_of::<ProgramBudget>(),
        ..Statistics::default()
    };
    #[cfg(feature = "guest-benchmark")]
    {
        result.reserved_bytes = owner.budget.usage().1.bytes;
        result.admission_epoch = owner.budget.epoch(Scope::Program);
    }
    for slot in &owner.slots {
        result.tracked += usize::from(slot.key.load(Ordering::Relaxed) != EMPTY);
        let status = slot.status();
        result.pending += usize::from(status == QUEUED);
        #[cfg(feature = "guest-benchmark")]
        {
            result.wait_global += usize::from(status == WAIT_GLOBAL);
            result.wait_program += usize::from(status == WAIT_PROGRAM);
        }
        if let Some(native) = slot.native.get() {
            result.compiled += 1;
            result.regions += usize::from(native.is_region());
            result.code_bytes += native.code_bytes;
            result.mapped_bytes += native.mapped_bytes;
        }
    }
    result
}

#[cfg(test)]
mod tests;
