/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use super::{Command, CompilationResult, DeploymentCommand, FilesystemQuery, ReplyValue};

#[derive(Clone, Copy, Debug)]
pub struct FleetMemoryLimits {
    /// Command values and retained payload buffers, including commands executing
    /// on workers. VM state, allocator metadata and queue capacity slack are separate.
    pub command_bytes: usize,
    pub command_bytes_per_machine: usize,
    /// Ordinary queued observations, including event envelopes and retained
    /// payloads. Collection/allocator slack and worker in-flight batches are
    /// separate. One oversized event may bypass these limits only while the
    /// queue is otherwise empty; it occupies separately reported headroom and
    /// blocks further ordinary publication until consumed.
    pub event_bytes: usize,
    pub event_bytes_per_machine: usize,
    /// Observation values, payloads and reservation handles retained while
    /// draining detach. Allocator metadata, collection slack and detached VM
    /// state are separate. One in-flight operation per worker is separate
    /// headroom; exhaustion rejects detach and pauses ordinary worker execution
    /// until that completion is consumed. Ordinary queue output is separate.
    pub detach_output_bytes: usize,
    pub detach_output_bytes_per_machine: usize,
}

impl Default for FleetMemoryLimits {
    fn default() -> Self {
        let artifact = crate::ArtifactLimits::default().artifact_bytes;
        Self {
            event_bytes: 4 * artifact,
            event_bytes_per_machine: 2 * artifact,
            command_bytes: 4 * artifact,
            command_bytes_per_machine: 2 * artifact,
            detach_output_bytes: 4 * artifact,
            detach_output_bytes_per_machine: 2 * artifact,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FleetMemoryUsage {
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

    pub(super) fn snapshot(&self) -> FleetMemoryUsage {
        let retained_bytes = self.used.load(Ordering::Acquire);
        FleetMemoryUsage {
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

/// One operation's output may already exist when its escrow budget fills. Its
/// worker lends this headroom to the completion and cannot produce more ordinary
/// output until the host takes ownership of it. Stop processing remains possible.
pub(super) struct OutputHeadroom {
    available: Arc<AtomicBool>,
    retained: Arc<AtomicUsize>,
    bytes: usize,
}

impl OutputHeadroom {
    pub(super) fn retain(
        available: Arc<AtomicBool>,
        retained: Arc<AtomicUsize>,
        bytes: usize,
    ) -> Self {
        assert!(
            available.swap(false, Ordering::AcqRel),
            "worker headroom already retained"
        );
        retained.fetch_add(bytes, Ordering::AcqRel);
        Self {
            available,
            retained,
            bytes,
        }
    }
}

impl Drop for OutputHeadroom {
    fn drop(&mut self) {
        self.retained.fetch_sub(self.bytes, Ordering::AcqRel);
        self.available.store(true, Ordering::Release);
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

/// Storage owned by an ordinary observation. Detached VM state belongs to the
/// lifecycle owner, not the transport budget, and must never enter this path.
pub(super) fn observation_bytes(event: &super::FleetEvent) -> usize {
    use super::{FilesystemResult, FleetEvent};
    use crate::{
        ComputerAdvanceOutcome as Outcome, ComputerHostMerge, ComputerHostRequest, ComputerValue,
    };
    fn value_bytes(value: &ComputerValue) -> usize {
        match value {
            ComputerValue::String(text) => text.len() * size_of::<u16>(),
            ComputerValue::ByteArray(bytes) => bytes.len(),
            ComputerValue::I32(_)
            | ComputerValue::I64(_)
            | ComputerValue::F32(_)
            | ComputerValue::F64(_)
            | ComputerValue::Bool(_)
            | ComputerValue::Char(_) => 0,
        }
    }
    fn request_bytes(request: &ComputerHostRequest) -> usize {
        request.namespace.len()
            + request.name.len()
            + std::mem::size_of_val(&*request.arguments)
            + request.arguments.iter().map(value_bytes).sum::<usize>()
            + match &request.merge {
                ComputerHostMerge::Ordinary => 0,
                ComputerHostMerge::LastWriteWins { entries, .. } => {
                    std::mem::size_of_val(&**entries)
                }
            }
    }
    let payload = match event {
        FleetEvent::Filesystem {
            result: Ok(result), ..
        } => match result {
            FilesystemResult::Stat(_) => 0,
            FilesystemResult::Read(chunk) => chunk.bytes.len(),
            FilesystemResult::List(list) => {
                std::mem::size_of_val(&*list.entries)
                    + list
                        .entries
                        .iter()
                        .map(|entry| entry.name.len())
                        .sum::<usize>()
            }
        },
        FleetEvent::TerminalUpdate { update, .. } => {
            use crate::{TerminalChange, TerminalUpdate};
            match update {
                TerminalUpdate::Unchanged { .. } => 0,
                TerminalUpdate::Full(snapshot) => {
                    std::mem::size_of_val(snapshot.cells())
                        + std::mem::size_of_val(snapshot.history_cells())
                }
                TerminalUpdate::Delta(delta) => {
                    std::mem::size_of_val(delta.changes())
                        + delta
                            .changes()
                            .iter()
                            .map(|change| match change {
                                TerminalChange::Patch { cells, .. }
                                | TerminalChange::HistoryAppend { cells, .. }
                                | TerminalChange::HistoryReplace { cells, .. } => {
                                    std::mem::size_of_val(&**cells)
                                }
                                TerminalChange::Fill { .. }
                                | TerminalChange::Scroll { .. }
                                | TerminalChange::Cursor { .. }
                                | TerminalChange::Reset => 0,
                            })
                            .sum::<usize>()
                }
            }
        }
        FleetEvent::Inspect { request, .. } => request_bytes(request),
        FleetEvent::Requests { requests, .. } => {
            requests.capacity() * size_of::<ComputerHostRequest>()
                + requests.iter().map(request_bytes).sum::<usize>()
        }
        FleetEvent::Boundary { outcome, .. } => match outcome {
            Outcome::Halted(value) => value.as_ref().map_or(0, value_bytes),
            Outcome::UncaughtException(detail) => detail.capacity(),
            Outcome::HostFailed(failure) => failure.detail().len(),
            Outcome::CompilationRequested(request) => {
                std::mem::size_of_val(&*request.sources)
                    + request
                        .sources
                        .iter()
                        .map(|source| source.path.len() + source.utf8.len())
                        .sum::<usize>()
            }
            Outcome::HostRequestBatch(batch) => {
                std::mem::size_of_val(&*batch.requests)
                    + batch.requests.iter().map(request_bytes).sum::<usize>()
            }
            Outcome::SliceExhausted
            | Outcome::ProcessEntered(_)
            | Outcome::ProcessExited(_)
            | Outcome::ProcessReplaced { .. }
            | Outcome::WaitingForHostQuota
            | Outcome::WaitingForTerminalEvent
            | Outcome::AllocationExhausted(_)
            | Outcome::QuotaExhausted(_)
            | Outcome::Crashed(_)
            | Outcome::Faulted(_) => 0,
        },
        FleetEvent::Stopped { pending, .. } | FleetEvent::ScopeStopped { pending, .. } => {
            pending.capacity() * size_of::<(u32, u64)>()
        }
        FleetEvent::Error { message, .. } => message.capacity(),
        FleetEvent::Deployment { .. }
        | FleetEvent::InputResult { .. }
        | FleetEvent::Filesystem { result: Err(_), .. }
        | FleetEvent::ResourceSnapshot { .. }
        | FleetEvent::FilesystemGeneration { .. }
        | FleetEvent::Quiescent { .. }
        | FleetEvent::DetachRejected { .. } => 0,
        FleetEvent::Detached { .. } => panic!("detached VM is not an ordinary observation"),
    };
    size_of::<super::FleetEvent>() + payload
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
