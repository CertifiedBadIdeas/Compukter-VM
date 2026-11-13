/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

//! Cold-path admission. Reservations never retain the program or a VM instance.
use super::*;
use std::sync::{Mutex, MutexGuard};

pub(super) const MODULE_BYTES: usize = 64 * 1024;
const GLOBAL_MODULES: usize = 2048;
const GLOBAL_BYTES: usize = 16 * 1024 * 1024;
const PROGRAM_MODULES: usize = 64;
const PROGRAM_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, Debug)]
struct Limits {
    modules: usize,
    bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Usage {
    pub modules: usize,
    pub bytes: usize,
    pub pending: usize,
}

#[derive(Debug)]
struct Budget {
    limits: Limits,
    usage: Mutex<Usage>,
    epoch: AtomicU32,
}

impl Budget {
    fn new(modules: usize, bytes: usize) -> Self {
        Self {
            limits: Limits { modules, bytes },
            usage: Mutex::new(Usage::default()),
            epoch: AtomicU32::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Usage> {
        self.usage.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fits(&self, usage: &Usage) -> bool {
        usage.modules < self.limits.modules
            && usage.bytes <= self.limits.bytes.saturating_sub(MODULE_BYTES)
            && self.limits.bytes >= MODULE_BYTES
    }
}

#[derive(Debug)]
pub(super) struct ProgramBudget {
    global: Arc<Budget>,
    local: Budget,
}

impl Default for ProgramBudget {
    fn default() -> Self {
        static GLOBAL: OnceLock<Arc<Budget>> = OnceLock::new();
        Self {
            global: GLOBAL
                .get_or_init(|| Arc::new(Budget::new(GLOBAL_MODULES, GLOBAL_BYTES)))
                .clone(),
            local: Budget::new(PROGRAM_MODULES, PROGRAM_BYTES),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    Global,
    Program,
}

#[derive(Debug)]
pub(super) struct Denied {
    pub scope: Scope,
    pub epoch: u32,
}

impl ProgramBudget {
    pub fn epoch(&self, scope: Scope) -> u32 {
        match scope {
            Scope::Global => &self.global,
            Scope::Program => &self.local,
        }
        .epoch
        .load(Ordering::Acquire)
    }

    #[cfg(any(test, feature = "guest-benchmark"))]
    pub fn usage(&self) -> (Usage, Usage) {
        // Every operation acquires the global lock before the local lock.
        let global = self.global.lock();
        let local = self.local.lock();
        (*global, *local)
    }
}

#[derive(Debug)]
pub(super) struct Reservation {
    program: Arc<ProgramBudget>,
    bytes: usize,
    pending: bool,
}

impl Reservation {
    pub fn try_acquire(program: Arc<ProgramBudget>) -> Result<Self, Denied> {
        {
            let mut global = program.global.lock();
            let mut local = program.local.lock();
            let scope = if local.pending != 0 || !program.local.fits(&local) {
                Some(Scope::Program)
            } else if !program.global.fits(&global) {
                Some(Scope::Global)
            } else {
                None
            };
            if let Some(scope) = scope {
                // Capture under the same locks as release: no lost wakeup between
                // observing a full budget and publishing the waiting slot.
                return Err(Denied {
                    scope,
                    epoch: program.epoch(scope),
                });
            }
            for used in [&mut *global, &mut *local] {
                used.modules += 1;
                used.bytes += MODULE_BYTES;
                used.pending += 1;
            }
        }
        Ok(Self {
            program,
            bytes: MODULE_BYTES,
            pending: true,
        })
    }

    #[cfg(test)]
    pub fn acquire() -> Option<Self> {
        Self::try_acquire(Arc::new(ProgramBudget::default())).ok()
    }

    /// LimitedMemory has already bounded every mapping request by MODULE_BYTES.
    /// Return unused in-flight headroom; actual pages remain charged to this owner.
    pub fn settle(&mut self, mapped_bytes: usize) {
        assert!(self.pending && mapped_bytes <= self.bytes);
        let mut global = self.program.global.lock();
        let mut local = self.program.local.lock();
        let refund = self.bytes - mapped_bytes;
        for used in [&mut *global, &mut *local] {
            used.bytes -= refund;
            used.pending -= 1;
        }
        self.bytes = mapped_bytes;
        self.pending = false;
        if refund != 0 {
            self.program.global.epoch.fetch_add(1, Ordering::Release);
        }
        self.program.local.epoch.fetch_add(1, Ordering::Release);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut global = self.program.global.lock();
        let mut local = self.program.local.lock();
        for used in [&mut *global, &mut *local] {
            used.modules -= 1;
            used.bytes -= self.bytes;
            used.pending -= usize::from(self.pending);
        }
        self.program.global.epoch.fetch_add(1, Ordering::Release);
        self.program.local.epoch.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
