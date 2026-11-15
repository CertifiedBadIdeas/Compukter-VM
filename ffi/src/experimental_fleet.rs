/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

//! Private opt-in experiment; this Rust interface is not part of the C ABI.

use compukter_vm::experimental_fleet_scheduler::FleetEvent;
use compukter_vm::experimental_fleet_scheduler::{FleetConfig, FleetScheduler, RequestInspection};
use compukter_vm::{verify_artifact, ArtifactLimits, ComputerMachine};
use compukter_vm::{ComputerAdvanceOutcome, ComputerHostRequestBatch};
use std::collections::HashMap;
use std::sync::Arc;

/// Creates equally configured computers from one verified artifact and the normal
/// host capability schema wire format. The caller retains the returned scheduler.
pub fn create(
    artifact: &[u8],
    schemas: &[u8],
    count: usize,
    config: FleetConfig,
) -> Result<FleetScheduler, String> {
    FleetScheduler::new(create_machines(artifact, schemas, count)?, config).map_err(str::to_owned)
}

pub fn create_with_request_inspections(
    artifact: &[u8],
    schemas: &[u8],
    count: usize,
    config: FleetConfig,
    inspections: Vec<RequestInspection>,
) -> Result<FleetScheduler, String> {
    create_with_coordinated_turns(artifact, schemas, count, config, inspections, false)
}

pub fn create_with_coordinated_turns(
    artifact: &[u8],
    schemas: &[u8],
    count: usize,
    config: FleetConfig,
    inspections: Vec<RequestInspection>,
    coordinated_turns: bool,
) -> Result<FleetScheduler, String> {
    FleetScheduler::new_with_coordinated_turns(
        create_machines(artifact, schemas, count)?,
        config,
        inspections,
        coordinated_turns,
    )
    .map_err(str::to_owned)
}

fn create_machines(
    artifact: &[u8],
    schemas: &[u8],
    count: usize,
) -> Result<Vec<(u64, ComputerMachine)>, String> {
    if count > 4096 {
        return Err("fleet size must be between 0 and 4096".into());
    }
    let schemas =
        crate::wire::decode_capability_schemas(schemas).ok_or("invalid capability schemas")?;
    if count == 0 {
        return Ok(Vec::new());
    }
    let artifact = verify_artifact(Arc::from(artifact), ArtifactLimits::default())
        .map_err(|error| format!("artifact verification: {error:?}"))?;
    let machines = crate::bridge::with_capability_bindings(&schemas, |bindings| {
        (0..count)
            .map(|id| {
                ComputerMachine::start(artifact.clone(), crate::bridge::profile(), bindings, &[])
                    .map(|machine| (id as u64, machine))
                    .map_err(|error| format!("admission: {error:?}"))
            })
            .collect::<Result<Vec<_>, _>>()
    })?;
    Ok(machines)
}

pub enum WireEvent {
    Deployment {
        machine: u64,
        request: u64,
        kind: u8,
        status: crate::FfiStatus,
        value: u64,
    },
    InputResult {
        machine: u64,
        request: u64,
        status: crate::FfiStatus,
    },
    Filesystem {
        machine: u64,
        request: u64,
        kind: u8,
        status: crate::FfiStatus,
        bytes: Vec<u8>,
    },
    ResourceSnapshot(Vec<u8>),
    FilesystemGeneration {
        machine: u64,
        request: u64,
        generation: u64,
    },
    TerminalUpdate(Vec<u8>),
    Compilation(Vec<u8>),
    Quiescent {
        machine: u64,
        tick: u64,
        retired: u64,
        timers_pending: bool,
    },
    Outcome(Vec<u8>),
    Inspection(Vec<u8>),
    Detached {
        machine: u64,
        computer: Box<ComputerMachine>,
        timers: Vec<compukter_vm::experimental_fleet_scheduler::FleetTimer>,
    },
    DetachRejected {
        machine: u64,
    },
    Error {
        machine: u64,
        message: String,
    },
    ScopeStopped {
        machine: u64,
        scope: u64,
        pending: Vec<(u32, u64)>,
    },
    Stopped {
        machine: u64,
        pending: Vec<(u32, u64)>,
    },
}

/// Encodes the common event protocol used by JVM transports. Ownership-bearing
/// events remain owned by the caller and must be retained until recovery.
pub fn encode_event(event: &WireEvent) -> Vec<u8> {
    let mut bytes = Vec::new();
    match event {
        WireEvent::Deployment {
            machine,
            request,
            kind,
            status,
            value,
        } => {
            bytes.push(17);
            bytes.extend_from_slice(&machine.to_le_bytes());
            bytes.extend_from_slice(&request.to_le_bytes());
            bytes.push(*kind);
            bytes.extend_from_slice(&(*status as u32).to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        WireEvent::InputResult {
            machine,
            request,
            status,
        } => {
            bytes.push(14);
            bytes.extend_from_slice(&machine.to_le_bytes());
            bytes.extend_from_slice(&request.to_le_bytes());
            bytes.extend_from_slice(&(*status as u32).to_le_bytes());
        }
        WireEvent::Filesystem {
            machine,
            request,
            kind,
            status,
            bytes: body,
        } => {
            bytes.push(13);
            bytes.extend_from_slice(&machine.to_le_bytes());
            bytes.extend_from_slice(&request.to_le_bytes());
            bytes.push(*kind);
            bytes.extend_from_slice(&(*status as u32).to_le_bytes());
            bytes.extend_from_slice(body);
        }
        WireEvent::ResourceSnapshot(body) => {
            bytes.push(11);
            bytes.extend_from_slice(body);
        }
        WireEvent::FilesystemGeneration {
            machine,
            request,
            generation,
        } => {
            bytes.push(12);
            bytes.extend_from_slice(&machine.to_le_bytes());
            bytes.extend_from_slice(&request.to_le_bytes());
            bytes.extend_from_slice(&generation.to_le_bytes());
        }
        WireEvent::TerminalUpdate(body) => {
            bytes.push(10);
            bytes.extend_from_slice(body);
        }
        WireEvent::Compilation(body) => {
            bytes.push(9);
            bytes.extend_from_slice(body);
        }
        WireEvent::Quiescent {
            machine,
            tick,
            retired,
            timers_pending,
        } => {
            bytes.push(8);
            bytes.extend_from_slice(&machine.to_le_bytes());
            bytes.extend_from_slice(&tick.to_le_bytes());
            bytes.extend_from_slice(&retired.to_le_bytes());
            bytes.push(u8::from(*timers_pending));
        }
        WireEvent::Outcome(body) => {
            bytes.push(0);
            bytes.extend_from_slice(body);
        }
        WireEvent::Inspection(body) => {
            bytes.push(1);
            bytes.extend_from_slice(body);
        }
        WireEvent::ScopeStopped {
            machine,
            scope,
            pending,
        } => {
            bytes.push(4);
            bytes.extend_from_slice(&machine.to_le_bytes());
            bytes.extend_from_slice(&scope.to_le_bytes());
            bytes.extend_from_slice(&(pending.len() as u32).to_le_bytes());
            for (task, request) in pending {
                bytes.extend_from_slice(&task.to_le_bytes());
                bytes.extend_from_slice(&request.to_le_bytes());
            }
        }
        WireEvent::Stopped { machine, pending } => {
            bytes.push(2);
            bytes.extend_from_slice(&machine.to_le_bytes());
            bytes.extend_from_slice(&(pending.len() as u32).to_le_bytes());
            for (task, request) in pending {
                bytes.extend_from_slice(&task.to_le_bytes());
                bytes.extend_from_slice(&request.to_le_bytes());
            }
        }
        WireEvent::Detached {
            machine, timers, ..
        } => {
            bytes.push(if timers.is_empty() { 6 } else { 16 });
            bytes.extend_from_slice(&machine.to_le_bytes());
            if !timers.is_empty() {
                bytes.extend_from_slice(&(timers.len() as u32).to_le_bytes());
                for timer in timers {
                    bytes.extend_from_slice(&timer.task.to_le_bytes());
                    bytes.extend_from_slice(&timer.request.to_le_bytes());
                    bytes.extend_from_slice(&timer.scope.to_le_bytes());
                    bytes.extend_from_slice(&timer.duration.to_le_bytes());
                    bytes.extend_from_slice(&timer.remaining.to_le_bytes());
                }
            }
        }
        WireEvent::DetachRejected { machine } => {
            bytes.push(7);
            bytes.extend_from_slice(&machine.to_le_bytes());
        }
        WireEvent::Error { machine, message } => {
            bytes.push(3);
            bytes.extend_from_slice(&machine.to_le_bytes());
            bytes.extend_from_slice(message.as_bytes());
        }
    }
    bytes
}

pub fn encode_poll_error(error: &str) -> Vec<u8> {
    let mut bytes = vec![5];
    bytes.extend_from_slice(error.as_bytes());
    bytes
}

/// Decodes the bounded timer transfer representation shared by JVM transports.
pub fn decode_timers(
    bytes: &[u8],
) -> Option<Vec<compukter_vm::experimental_fleet_scheduler::FleetTimer>> {
    if bytes.len() > 256 * 32 || !bytes.as_chunks::<32>().1.is_empty() {
        return None;
    }
    Some(
        bytes
            .as_chunks::<32>()
            .0
            .iter()
            .map(
                |item| compukter_vm::experimental_fleet_scheduler::FleetTimer {
                    task: u32::from_le_bytes(item[0..4].try_into().unwrap()),
                    request: u64::from_le_bytes(item[4..12].try_into().unwrap()),
                    scope: u64::from_le_bytes(item[12..20].try_into().unwrap()),
                    duration: u32::from_le_bytes(item[20..24].try_into().unwrap()),
                    remaining: u64::from_le_bytes(item[24..32].try_into().unwrap()),
                },
            )
            .collect(),
    )
}

/// Transport-owned session state shared by JNI and other embedders.
/// An encoded event is retained until explicitly consumed, including on small-buffer retries.
pub struct FleetTransport {
    pub scheduler: FleetScheduler,
    sessions: HashMap<u64, SessionMetadata>,
    detached: HashMap<u64, Box<ComputerMachine>>,
    pending: Option<Vec<u8>>,
    lifecycle_pending: HashMap<u64, Vec<u8>>,
}

impl FleetTransport {
    pub fn new(scheduler: FleetScheduler) -> Self {
        Self {
            scheduler,
            sessions: HashMap::new(),
            detached: HashMap::new(),
            pending: None,
            lifecycle_pending: HashMap::new(),
        }
    }

    /// On refusal the original session remains valid.
    pub fn attach(
        &mut self,
        session: u64,
        tick: u64,
        timers: Vec<compukter_vm::experimental_fleet_scheduler::FleetTimer>,
    ) -> Result<u64, crate::FfiStatus> {
        // Includes detached sessions awaiting recovery, not only worker actors.
        if self.sessions.len() >= 4096 {
            return Err(crate::FfiStatus::BusyHandle);
        }
        self.sessions
            .try_reserve(1)
            .map_err(|_| crate::FfiStatus::Internal)?;
        // All attached sessions may detach before the host recovers any of them.
        // Reserve their slots before taking ownership so polling cannot need to grow this map.
        let detached_slots = self
            .sessions
            .len()
            .saturating_add(1)
            .saturating_sub(self.detached.len());
        self.detached
            .try_reserve(detached_slots)
            .map_err(|_| crate::FfiStatus::Internal)?;
        let (machine, metadata) =
            attach_session_with_timers(&self.scheduler, session, tick, timers)?;
        self.sessions.insert(machine, metadata);
        Ok(machine)
    }

    pub fn owns_session(&self, machine: u64) -> bool {
        self.sessions.contains_key(&machine)
    }

    pub fn can_close(&self) -> bool {
        self.sessions.is_empty() && self.pending.is_none() && self.lifecycle_pending.is_empty()
    }

    /// A failed recovery leaves both the machine and its metadata available for retry.
    pub fn recover(&mut self, machine: u64) -> Result<u64, crate::FfiStatus> {
        if self.lifecycle_pending.contains_key(&machine) {
            return Err(crate::FfiStatus::BusyHandle);
        }
        if !self.detached.contains_key(&machine) || !self.sessions.contains_key(&machine) {
            return Err(crate::FfiStatus::InvalidArgument);
        }
        let computer = self.detached.remove(&machine).expect("checked computer");
        let metadata = self.sessions.remove(&machine).expect("checked metadata");
        match restore_session(computer, metadata) {
            Ok(session) => Ok(session),
            Err((status, computer, metadata)) => {
                self.detached.insert(machine, computer);
                self.sessions.insert(machine, metadata);
                Err(status)
            }
        }
    }

    pub fn poll(&mut self) -> Option<&[u8]> {
        if self.pending.is_none() {
            self.pending = match poll_event(&self.scheduler) {
                Ok(None) => None,
                Ok(Some(event)) => Some(self.retain_event(event)),
                Err(error) => Some(encode_poll_error(&error)),
            };
        }
        self.pending.as_deref()
    }

    pub fn consume_event(&mut self) {
        self.pending = None;
    }

    fn retain_event(&mut self, event: WireEvent) -> Vec<u8> {
        let encoded = encode_event(&event);
        match event {
            WireEvent::Stopped { machine, .. } => {
                self.sessions.remove(&machine);
            }
            WireEvent::Detached {
                machine, computer, ..
            } => {
                self.detached.insert(machine, computer);
            }
            _ => {}
        }
        encoded
    }

    /// One encoded event per claimed machine; the native slot retains its
    /// remaining output, reservations and fence until the terminal delivery ack.
    pub fn poll_lifecycle(&mut self, machine: u64) -> Result<Option<&[u8]>, crate::FfiStatus> {
        if !self.lifecycle_pending.contains_key(&machine) {
            self.lifecycle_pending
                .try_reserve(1)
                .map_err(|_| crate::FfiStatus::Internal)?;
            let Some(event) = self.scheduler.poll_lifecycle_event(machine) else {
                return Ok(None);
            };
            let encoded = match convert_event(event) {
                Ok(event) => self.retain_event(event),
                Err(error) => encode_poll_error(&error),
            };
            self.lifecycle_pending.insert(machine, encoded);
        }
        Ok(self.lifecycle_pending.get(&machine).map(Vec::as_slice))
    }

    pub fn consume_lifecycle_event(&mut self, machine: u64) {
        if self.lifecycle_pending.remove(&machine).is_some() {
            assert!(
                self.scheduler.consume_lifecycle_event(machine),
                "native lifecycle delivery missing"
            );
        }
    }
}

/// Preserves lifecycle notifications separately from the VM outcome wire.
pub fn poll_event(fleet: &FleetScheduler) -> Result<Option<WireEvent>, String> {
    fleet.try_event().map(convert_event).transpose()
}

fn convert_event(event: FleetEvent) -> Result<WireEvent, String> {
    let (machine, tick, outcome, inspection) = match event {
        FleetEvent::Deployment {
            machine,
            request,
            result,
        } => {
            use compukter_vm::experimental_fleet_scheduler::{
                DeploymentError as E, DeploymentResult as R,
            };
            let revision =
                |value: compukter_vm::ExecutableRevision| value.generation().unwrap_or(0);
            let (kind, status, value) = match result {
                Ok(R::Prepared(token)) => (0, crate::FfiStatus::Ok, token),
                Ok(R::Revision(value)) => (1, crate::FfiStatus::Ok, revision(value)),
                Ok(R::Installed(value)) => (2, crate::FfiStatus::Ok, revision(value)),
                Ok(R::Discarded) => (3, crate::FfiStatus::Ok, 0),
                Err(error) => (
                    4,
                    match error {
                        E::Verification => crate::FfiStatus::Verification,
                        E::Admission => crate::FfiStatus::Admission,
                        E::InvalidCandidate => crate::FfiStatus::StaleHandle,
                        E::Capacity => crate::FfiStatus::BusyHandle,
                        E::WrongMachine => crate::FfiStatus::DeploymentWrongMachine,
                        E::ProfileChanged => crate::FfiStatus::DeploymentProfileChanged,
                        E::FileSystem(error) => crate::ffi_api::file_read_status(error.into()),
                    },
                    0,
                ),
            };
            return Ok(WireEvent::Deployment {
                machine,
                request,
                kind,
                status,
                value,
            });
        }
        FleetEvent::InputResult {
            machine,
            request,
            result,
        } => {
            use compukter_vm::experimental_fleet_scheduler::InputFailure;
            use compukter_vm::CanonicalLineSubmissionError as C;
            let status = match result {
                Ok(()) => crate::FfiStatus::Ok,
                Err(InputFailure::Terminal(_)) => crate::FfiStatus::InvalidArgument,
                Err(InputFailure::Canonical(C::NoPendingRead)) => {
                    crate::FfiStatus::InputNoPendingRead
                }
                Err(InputFailure::Canonical(C::InputBusy)) => crate::FfiStatus::InputBusy,
                Err(InputFailure::Canonical(C::PartialInput)) => crate::FfiStatus::InputPartial,
                Err(InputFailure::Canonical(C::UnsupportedCodeUnit)) => {
                    crate::FfiStatus::InputUnsupportedCodeUnit
                }
                Err(InputFailure::Canonical(C::LineTooLong)) => crate::FfiStatus::InputLineTooLong,
                Err(InputFailure::Canonical(C::Terminal)) => crate::FfiStatus::InputTerminal,
                Err(InputFailure::Canonical(C::Resume)) => crate::FfiStatus::InputResume,
            };
            return Ok(WireEvent::InputResult {
                machine,
                request,
                status,
            });
        }
        FleetEvent::Filesystem {
            machine,
            request,
            result,
        } => {
            use compukter_vm::experimental_fleet_scheduler::FilesystemResult;
            let (kind, status, bytes) = match result {
                Ok(FilesystemResult::Stat(stat)) => (
                    0,
                    crate::FfiStatus::Ok,
                    crate::wire::encode_filesystem_stat(&stat),
                ),
                Ok(FilesystemResult::List(list)) => {
                    match crate::wire::encode_filesystem_list(&list) {
                        Some(bytes) if bytes.len() <= 1_048_576 - 22 => {
                            (1, crate::FfiStatus::Ok, bytes)
                        }
                        _ => (3, crate::FfiStatus::FilesystemLimitExceeded, Vec::new()),
                    }
                }
                Ok(FilesystemResult::Read(chunk)) => (
                    2,
                    crate::FfiStatus::Ok,
                    crate::wire::encode_filesystem_chunk(&chunk),
                ),
                Err(error) => (3, crate::ffi_api::file_read_status(error), Vec::new()),
            };
            return Ok(WireEvent::Filesystem {
                machine,
                request,
                kind,
                status,
                bytes,
            });
        }
        FleetEvent::ResourceSnapshot {
            machine,
            request,
            snapshot,
        } => {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&machine.to_le_bytes());
            bytes.extend_from_slice(&request.to_le_bytes());
            bytes.extend_from_slice(&crate::wire::encode_resource_snapshot(snapshot));
            return Ok(WireEvent::ResourceSnapshot(bytes));
        }
        FleetEvent::FilesystemGeneration {
            machine,
            request,
            generation,
        } => {
            return Ok(WireEvent::FilesystemGeneration {
                machine,
                request,
                generation,
            })
        }
        FleetEvent::TerminalUpdate {
            machine,
            request,
            update,
        } => {
            let mut bytes = vec![0; 16 + 256 * 1024];
            bytes[..8].copy_from_slice(&machine.to_le_bytes());
            bytes[8..16].copy_from_slice(&request.to_le_bytes());
            let size = crate::wire::encode_terminal_update_into(&mut bytes[16..], &update)
                .map_err(|_| "terminal update exceeds limit")?;
            bytes.truncate(16 + size);
            return Ok(WireEvent::TerminalUpdate(bytes));
        }
        FleetEvent::Quiescent {
            machine,
            tick,
            retired,
            timers_pending,
        } => {
            return Ok(WireEvent::Quiescent {
                machine,
                tick,
                retired,
                timers_pending,
            })
        }
        FleetEvent::Inspect {
            machine,
            tick,
            request,
        } => (
            machine,
            tick,
            ComputerAdvanceOutcome::HostRequestBatch(ComputerHostRequestBatch {
                requests: vec![request].into_boxed_slice(),
            }),
            true,
        ),
        FleetEvent::Detached {
            machine,
            computer,
            timers,
        } => {
            return Ok(WireEvent::Detached {
                machine,
                computer,
                timers,
            });
        }
        FleetEvent::DetachRejected { machine } => return Ok(WireEvent::DetachRejected { machine }),
        FleetEvent::ScopeStopped {
            machine,
            scope,
            pending,
        } => {
            return Ok(WireEvent::ScopeStopped {
                machine,
                scope,
                pending,
            });
        }
        FleetEvent::Stopped { machine, pending } => {
            return Ok(WireEvent::Stopped { machine, pending });
        }
        FleetEvent::Requests {
            machine,
            tick,
            requests,
        } => (
            machine,
            tick,
            ComputerAdvanceOutcome::HostRequestBatch(ComputerHostRequestBatch {
                requests: requests.into_boxed_slice(),
            }),
            false,
        ),
        FleetEvent::Boundary {
            machine,
            tick,
            outcome: ComputerAdvanceOutcome::CompilationRequested(request),
        } => {
            let size = crate::wire::compilation_request_size(&request)
                .ok_or("invalid compilation request")?;
            let mut bytes = vec![0; 16 + size];
            bytes[..8].copy_from_slice(&machine.to_le_bytes());
            bytes[8..16].copy_from_slice(&tick.to_le_bytes());
            crate::wire::encode_compilation_request_into(&mut bytes[16..], &request)
                .map_err(|_| "invalid compilation request")?;
            return Ok(WireEvent::Compilation(bytes));
        }
        FleetEvent::Boundary {
            machine,
            tick,
            outcome,
        } => (machine, tick, outcome, false),
        FleetEvent::Error { machine, message } => return Ok(WireEvent::Error { machine, message }),
    };
    let encoded = crate::wire::encode_outcome(crate::bridge::copy_outcome(outcome));
    let mut bytes = Vec::with_capacity(16 + encoded.len());
    bytes.extend_from_slice(&machine.to_le_bytes());
    bytes.extend_from_slice(&tick.to_le_bytes());
    bytes.extend_from_slice(&encoded);
    Ok(if inspection {
        WireEvent::Inspection(bytes)
    } else {
        WireEvent::Outcome(bytes)
    })
}

/// Decode the existing bounded host response wire before sending it to a worker.
pub fn decode_reply(
    bytes: &[u8],
) -> Option<compukter_vm::experimental_fleet_scheduler::ReplyValue> {
    use crate::bridge::OwnedResponse;
    use compukter_vm::experimental_fleet_scheduler::ReplyValue;
    Some(match crate::wire::decode_response(bytes)? {
        OwnedResponse::SuccessUnit => ReplyValue::Unit,
        OwnedResponse::SuccessI32(v) => ReplyValue::I32(v),
        OwnedResponse::SuccessI64(v) => ReplyValue::I64(v),
        OwnedResponse::SuccessF32(v) => ReplyValue::F32(v),
        OwnedResponse::SuccessF64(v) => ReplyValue::F64(v),
        OwnedResponse::SuccessBool(v) => ReplyValue::Bool(v),
        OwnedResponse::SuccessChar(v) => ReplyValue::Char(v),
        OwnedResponse::SuccessString(v) => ReplyValue::String(v.into_boxed_slice()),
        OwnedResponse::SuccessByteArray(v) => ReplyValue::Bytes(v.into_boxed_slice()),
        OwnedResponse::SuccessRecord(v) => ReplyValue::Record(v),
        OwnedResponse::Failure { kind, detail } => {
            ReplyValue::Failure(kind, detail.into_boxed_str())
        }
    })
}

/// Opaque bridge state retained while a session's computer belongs to a fleet.
pub struct SessionMetadata(crate::bridge::FleetSessionMetadata);

/// On refusal the original session handle remains valid and owns the machine.
pub fn attach_session(
    fleet: &FleetScheduler,
    handle: u64,
) -> Result<(u64, SessionMetadata), crate::FfiStatus> {
    attach_session_with_timers(fleet, handle, 0, Vec::new())
}

/// Transfers an owned session with validated timer metadata; refusal leaves it intact.
pub fn attach_session_with_timers(
    fleet: &FleetScheduler,
    handle: u64,
    base_tick: u64,
    timers: Vec<compukter_vm::experimental_fleet_scheduler::FleetTimer>,
) -> Result<(u64, SessionMetadata), crate::FfiStatus> {
    let (machine, metadata) =
        crate::bridge::attach_fleet_session(fleet, handle, base_tick, timers)?;
    Ok((machine, SessionMetadata(metadata)))
}

/// On failure both the machine and its metadata are returned for retry.
#[allow(clippy::result_large_err)]
pub fn restore_session(
    computer: Box<ComputerMachine>,
    metadata: SessionMetadata,
) -> Result<u64, (crate::FfiStatus, Box<ComputerMachine>, SessionMetadata)> {
    crate::bridge::restore_fleet_session(computer, metadata.0)
        .map_err(|(status, computer, metadata)| (status, computer, SessionMetadata(metadata)))
}

#[cfg(test)]
mod reply_tests {
    use super::decode_reply;
    use compukter_vm::experimental_fleet_scheduler::ReplyValue;
    use compukter_vm::{HostRecordMember, HostRecordScalar};

    #[test]
    fn fleet_decodes_record_reply_with_unchanged_bits_and_rejects_truncation() {
        let mut bytes = vec![1, 8, 14, 0];
        bytes.extend_from_slice(b"fixture.Vector");
        bytes.extend_from_slice(&[1, 1, b'x', 4]);
        bytes.extend_from_slice(&0x7ff0000000000042u64.to_le_bytes());
        let Some(ReplyValue::Record(record)) = decode_reply(&bytes) else {
            panic!("record missing")
        };
        assert_eq!(record.type_name.as_str(), "fixture.Vector");
        assert_eq!(
            record.fields.as_slice(),
            &[HostRecordMember {
                name: "x".into(),
                value: HostRecordScalar::F64(0x7ff0000000000042)
            }]
        );
        for end in 0..bytes.len() {
            assert!(decode_reply(&bytes[..end]).is_none());
        }
    }
}
