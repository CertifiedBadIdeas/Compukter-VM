/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

//! Experimental shared baseline tier. No lifecycle work or native pointer enters Guest state.
use std::sync::{
    atomic::{AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering},
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
const MAX_MODULES: usize = 256;
const MODULE_BYTES: usize = 64 * 1024;
const EMPTY: usize = usize::MAX;
const COLD: u8 = 0;
const QUEUED: u8 = 1;
const FINISHED: u8 = 2;

#[derive(Debug)]
struct Slot {
    key: AtomicUsize,
    visits: AtomicU32,
    state: AtomicU8,
    retries: AtomicU8,
    native: OnceLock<Arc<lower::CompiledRun>>,
}

#[cfg(feature = "jit-diagnostics")]
pub(super) mod calls;
#[cfg(feature = "jit-diagnostics")]
pub(super) use calls::record_call;

#[derive(Debug)]
pub(super) struct JitState {
    slots: Box<[Slot]>,
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
                    visits: AtomicU32::new(0),
                    state: AtomicU8::new(COLD),
                    retries: AtomicU8::new(0),
                    native: OnceLock::new(),
                })
                .collect(),
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

// A token bounds *all* in-flight and published modules, including active calls.
// The memory provider separately caps every module's page-rounded allocations.
static LIVE_MODULES: AtomicUsize = AtomicUsize::new(0);
struct Reservation;
fn reserve_count(counter: &AtomicUsize) -> bool {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < MAX_MODULES).then_some(n + 1)
        })
        .is_ok()
}
impl Reservation {
    fn acquire() -> Option<Self> {
        reserve_count(&LIVE_MODULES).then(|| Self)
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        LIVE_MODULES.fetch_sub(1, Ordering::Release);
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
                                .state
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
            owner.hits.fetch_add(1, Ordering::Relaxed);
            owner.attempts.fetch_add(native_attempts, Ordering::Relaxed);
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
        owner.hits.fetch_add(1, Ordering::Relaxed);
        owner.attempts.fetch_add(result.attempts, Ordering::Relaxed);
        return Some(Execution::Prefix(result));
    }
    if slot.state.load(Ordering::Acquire) != COLD {
        return None;
    }
    if slot.visits.fetch_add(1, Ordering::Relaxed) + 1 < HOT << slot.retries.load(Ordering::Relaxed)
    {
        return None;
    }
    if slot
        .state
        .compare_exchange(COLD, QUEUED, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return None;
    }
    let submitted = (|| {
        let plan = block
            .filter(|_| instruction == 0)
            .and_then(|block| {
                heap.as_ref()
                    .and_then(|access| lower::Plan::region(access.image, block))
            })
            .or_else(|| lower::Plan::read(code, key, block_len.checked_sub(instruction)?))?;
        let reservation = Reservation::acquire()?;
        Some(compiler()?.try_send(Job {
            owner: Arc::downgrade(owner),
            slot: index,
            plan,
            reservation,
        }))
    })();
    match submitted {
        Some(Ok(())) => {}
        Some(Err(std::sync::mpsc::TrySendError::Full(_)))
            if slot.retries.load(Ordering::Relaxed) < 8 =>
        {
            // A burst from one hot loop must not permanently exclude later runs.
            // Retry at exponentially increasing visit intervals, at most eight times.
            slot.retries.fetch_add(1, Ordering::Relaxed);
            slot.visits.store(0, Ordering::Relaxed);
            slot.state.store(COLD, Ordering::Release);
        }
        _ => {
            slot.state.store(FINISHED, Ordering::Release);
        }
    }
    None
}

pub(super) enum Execution {
    Prefix(SimpleRun),
    Region(lower::RegionRun),
}

/// Benchmark telemetry is shared per program, not retained per machine.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Statistics {
    pub tracked: usize,
    pub compiled: usize,
    pub regions: usize,
    pub pending: usize,
    pub code_bytes: usize,
    pub mapped_bytes: usize,
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
        hits: owner.hits.load(Ordering::Relaxed),
        attempts: owner.attempts.load(Ordering::Relaxed),
        compile_ns: owner.compile_ns.load(Ordering::Relaxed),
        failures: owner.failures.load(Ordering::Relaxed),
        state_bytes: std::mem::size_of::<JitState>() + SLOTS * std::mem::size_of::<Slot>(),
        ..Statistics::default()
    };
    for slot in &owner.slots {
        result.tracked += usize::from(slot.key.load(Ordering::Relaxed) != EMPTY);
        result.pending += usize::from(slot.state.load(Ordering::Acquire) == QUEUED);
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
