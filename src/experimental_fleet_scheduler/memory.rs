/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::{Command, CompilationResult, DeploymentCommand, FilesystemQuery, ReplyValue};

#[derive(Clone, Copy, Debug)]
pub struct FleetMemoryLimits {
    /// Command values and retained payload buffers, including commands executing
    /// on workers. VM state, allocator metadata and queue capacity slack are separate.
    pub command_bytes: usize,
    pub command_bytes_per_machine: usize,
}

impl Default for FleetMemoryLimits {
    fn default() -> Self {
        let artifact = crate::ArtifactLimits::default().artifact_bytes;
        Self {
            command_bytes: 4 * artifact,
            command_bytes_per_machine: 2 * artifact,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FleetCommandMemory {
    pub retained_bytes: usize,
    pub peak_bytes: usize,
    pub limit_bytes: usize,
}

pub(super) struct ByteBudget {
    limit: usize,
    used: AtomicUsize,
    peak: AtomicUsize,
}

impl ByteBudget {
    pub(super) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }

    pub(super) fn reserve(self: &Arc<Self>, bytes: usize) -> Option<Reservation> {
        let previous = self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |used| {
                let next = used.checked_add(bytes)?;
                (next <= self.limit).then_some(next)
            })
            .ok()?;
        self.peak.fetch_max(previous + bytes, Ordering::Relaxed);
        Some(Reservation {
            budget: Arc::clone(self),
            bytes,
        })
    }

    pub(super) fn snapshot(&self) -> FleetCommandMemory {
        let retained_bytes = self.used.load(Ordering::Acquire);
        FleetCommandMemory {
            retained_bytes,
            peak_bytes: self.peak.load(Ordering::Relaxed).max(retained_bytes),
            limit_bytes: self.limit,
        }
    }
}

pub(super) struct Reservation {
    budget: Arc<ByteBudget>,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// Arc payloads are charged in full for each retaining command. This is a
/// conservative bound and does not allocate a table of shared backing identities.
pub(super) fn command_payload_bytes(command: &Command) -> Option<usize> {
    match command {
        Command::Deployment { operation, .. } => Some(match operation {
            DeploymentCommand::Prepare(bytes) => bytes.len().checked_add(2 * size_of::<usize>())?,
            DeploymentCommand::Revision(path) | DeploymentCommand::Install { path, .. } => {
                path.len()
            }
            DeploymentCommand::Discard(_) => 0,
        }),
        Command::Filesystem { query, .. } => Some(match query {
            FilesystemQuery::Stat(path) | FilesystemQuery::Read { path, .. } => path.len(),
            FilesystemQuery::List { path, cursor, .. } => path
                .len()
                .checked_add(cursor.as_ref().map_or(0, |value| value.len()))?,
        }),
        // The machine moves between owners; its resident state is not a copied
        // transport payload. Registration/lifecycle capacity continues to own it.
        Command::Add { timers, .. } => timers
            .capacity()
            .checked_mul(size_of::<super::FleetTimer>()),
        Command::Reply { value, .. } => Some(match value {
            ReplyValue::String(value) => value.len().checked_mul(2)?,
            ReplyValue::Bytes(value) => value.len(),
            ReplyValue::Failure(_, detail) => detail.len(),
            ReplyValue::Record(record) => record.checked_resident_bytes()?,
            _ => 0,
        }),
        Command::TerminalText { text, .. } => Some(text.len()),
        Command::CanonicalLine { line, .. } => line.len().checked_mul(2),
        Command::CompilationComplete { result, .. } => Some(match result {
            CompilationResult::Artifact(bytes) => bytes.len(),
            CompilationResult::Failure(detail) => detail.len(),
        }),
        Command::Tick { .. }
        | Command::Stop { .. }
        | Command::Detach { .. }
        | Command::TerminalKey { .. }
        | Command::TerminalUpdate { .. }
        | Command::RedstoneInput { .. }
        | Command::RedstoneOutput { .. }
        | Command::ResourceSnapshot { .. }
        | Command::FilesystemGeneration { .. }
        | Command::Cost { .. } => Some(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_workers_cannot_overcommit_the_shared_budget() {
        let budget = ByteBudget::new(300);
        let attempted = std::sync::Barrier::new(9);
        let release = std::sync::Barrier::new(9);
        let full = std::thread::scope(|scope| {
            for _ in 0..8 {
                let budget = &budget;
                let attempted = &attempted;
                let release = &release;
                scope.spawn(move || {
                    let reservation = budget.reserve(100);
                    attempted.wait();
                    release.wait();
                    drop(reservation);
                });
            }
            attempted.wait();
            let full = budget.snapshot();
            release.wait();
            full
        });
        assert_eq!(full.retained_bytes, 300);
        assert_eq!(full.peak_bytes, 300);
        assert_eq!(budget.snapshot().retained_bytes, 0);
        assert_eq!(budget.snapshot().peak_bytes, 300);
    }

    #[test]
    fn record_payload_counts_retained_capacity_and_bounds_depth() {
        let record = crate::HostRecordValue {
            type_name: String::with_capacity(128),
            fields: Vec::with_capacity(16),
        };
        let expected = record.type_name.capacity()
            + record.fields.capacity() * size_of::<crate::HostRecordMember>();
        let command = Command::Reply {
            machine: 1,
            task: 1,
            request: 1,
            value: ReplyValue::Record(record),
        };
        assert_eq!(command_payload_bytes(&command), Some(expected));

        let mut record = crate::HostRecordValue {
            type_name: String::new(),
            fields: Vec::new(),
        };
        for _ in 0..16 {
            record = crate::HostRecordValue {
                type_name: String::new(),
                fields: vec![crate::HostRecordMember {
                    name: String::new(),
                    value: crate::HostRecordScalar::Record(Box::new(record)),
                }],
            };
        }
        let command = Command::Reply {
            machine: 1,
            task: 1,
            request: 1,
            value: ReplyValue::Record(record),
        };
        assert_eq!(command_payload_bytes(&command), None);
    }
}
