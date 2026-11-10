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

#[derive(Debug)]
pub(super) struct JitState {
    slots: Box<[Slot]>,
    hits: AtomicU64,
    attempts: AtomicU64,
    compile_ns: AtomicU64,
    failures: AtomicU64,
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

pub(super) fn try_execute(
    code: &PreparedCode,
    frame: &mut PreparedFrame<'_>,
    code_start: usize,
    instruction: usize,
    block_len: usize,
    budget: &RunBudget,
) -> Option<SimpleRun> {
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
        if native.len > block_len.checked_sub(instruction)?
            || native.len as u64 > budget.limit.min(budget.headroom)
            || frame.jit_bytes().len() < native.required_bytes as usize
        {
            return None;
        }
        // This borrow keeps the program, slot and executable module alive through the call.
        let result = native.execute(frame, instruction);
        owner.hits.fetch_add(1, Ordering::Relaxed);
        owner.attempts.fetch_add(result.attempts, Ordering::Relaxed);
        return Some(result);
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
        let plan = lower::Plan::read(code, key, block_len.checked_sub(instruction)?)?;
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

/// Benchmark telemetry is shared per program, not retained per machine.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Statistics {
    pub tracked: usize,
    pub compiled: usize,
    pub pending: usize,
    pub code_bytes: usize,
    pub mapped_bytes: usize,
    pub hits: u64,
    pub attempts: u64,
    pub compile_ns: u64,
    pub failures: u64,
    pub state_bytes: usize,
}
pub(super) fn statistics(code: &PreparedCode) -> Statistics {
    let Some(owner) = code.jit.get() else {
        return Statistics::default();
    };
    let mut result = Statistics {
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
            result.code_bytes += native.code_bytes;
            result.mapped_bytes += native.mapped_bytes;
        }
    }
    result
}

#[cfg(test)]
mod tests;
