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
    FleetScheduler::new_with_request_inspections(
        create_machines(artifact, schemas, count)?,
        config,
        inspections,
    )
    .map_err(str::to_owned)
}

fn create_machines(
    artifact: &[u8],
    schemas: &[u8],
    count: usize,
) -> Result<Vec<(u64, ComputerMachine)>, String> {
    if count == 0 || count > 4096 {
        return Err("fleet size must be between 1 and 4096".into());
    }
    let schemas =
        crate::wire::decode_capability_schemas(schemas).ok_or("invalid capability schemas")?;
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

/// Encodes a single event with the normal outcome wire format after its machine
/// and tick. Experimental errors are returned separately and never encoded as a
/// successful VM outcome.
pub fn poll(fleet: &FleetScheduler) -> Result<Option<Vec<u8>>, String> {
    match poll_event(fleet)? {
        Some(WireEvent::Outcome(bytes)) => Ok(Some(bytes)),
        Some(WireEvent::Stopped { .. }) | Some(WireEvent::ScopeStopped { .. }) => {
            Err("stop requires the typed fleet event API".into())
        }
        Some(WireEvent::Inspection(_)) => {
            Err("inspection requires the typed fleet event API".into())
        }
        None => Ok(None),
    }
}

pub enum WireEvent {
    Outcome(Vec<u8>),
    Inspection(Vec<u8>),
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

/// Preserves lifecycle notifications separately from the VM outcome wire.
pub fn poll_event(fleet: &FleetScheduler) -> Result<Option<WireEvent>, String> {
    let Some(event) = fleet.try_event() else {
        return Ok(None);
    };
    let (machine, tick, outcome, inspection) = match event {
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
        FleetEvent::ScopeStopped {
            machine,
            scope,
            pending,
        } => {
            return Ok(Some(WireEvent::ScopeStopped {
                machine,
                scope,
                pending,
            }));
        }
        FleetEvent::Stopped { machine, pending } => {
            return Ok(Some(WireEvent::Stopped { machine, pending }));
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
            outcome: ComputerAdvanceOutcome::CompilationRequested(_),
        } => {
            return Err(format!(
                "machine {machine} requested compilation at tick {tick}; unsupported by experiment"
            ))
        }
        FleetEvent::Boundary {
            machine,
            tick,
            outcome,
        } => (machine, tick, outcome, false),
        FleetEvent::Error { machine, message } => {
            return Err(format!("machine {machine}: {message}"))
        }
    };
    let encoded = crate::wire::encode_outcome(crate::bridge::copy_outcome(outcome));
    let mut bytes = Vec::with_capacity(16 + encoded.len());
    bytes.extend_from_slice(&machine.to_le_bytes());
    bytes.extend_from_slice(&tick.to_le_bytes());
    bytes.extend_from_slice(&encoded);
    Ok(Some(if inspection {
        WireEvent::Inspection(bytes)
    } else {
        WireEvent::Outcome(bytes)
    }))
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
