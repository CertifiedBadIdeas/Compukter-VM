/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

//! Private opt-in experiment; this Rust interface is not part of the C ABI.

use compukter_vm::experimental_fleet_scheduler::FleetEvent;
use compukter_vm::experimental_fleet_scheduler::{FleetConfig, FleetScheduler, RequestCostPolicy};
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
    create_with_request_cost(
        artifact,
        schemas,
        count,
        config,
        Arc::new(
            |_| compukter_vm::experimental_fleet_scheduler::RequestCost {
                units: 1,
                over_budget_detail: "Host request exceeds per-tick budget",
            },
        ),
    )
}

/// The embedder must derive costs from the authoritative host protocol; this
/// adapter does not interpret game-specific requests.
pub fn create_with_request_cost(
    artifact: &[u8],
    schemas: &[u8],
    count: usize,
    config: FleetConfig,
    request_cost: Arc<RequestCostPolicy>,
) -> Result<FleetScheduler, String> {
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
    FleetScheduler::new_with_request_cost(machines, config, request_cost).map_err(str::to_owned)
}

/// Encodes a single event with the normal outcome wire format after its machine
/// and tick. Experimental errors are returned separately and never encoded as a
/// successful VM outcome.
pub fn poll(fleet: &FleetScheduler) -> Result<Option<Vec<u8>>, String> {
    match poll_event(fleet)? {
        Some(WireEvent::Outcome(bytes)) => Ok(Some(bytes)),
        Some(WireEvent::Stopped { .. }) => Err("stop requires the typed fleet event API".into()),
        None => Ok(None),
    }
}

pub enum WireEvent {
    Outcome(Vec<u8>),
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
    let (machine, tick, outcome) = match event {
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
        } => (machine, tick, outcome),
        FleetEvent::Error { machine, message } => {
            return Err(format!("machine {machine}: {message}"))
        }
    };
    let encoded = crate::wire::encode_outcome(crate::bridge::copy_outcome(outcome));
    let mut bytes = Vec::with_capacity(16 + encoded.len());
    bytes.extend_from_slice(&machine.to_le_bytes());
    bytes.extend_from_slice(&tick.to_le_bytes());
    bytes.extend_from_slice(&encoded);
    Ok(Some(WireEvent::Outcome(bytes)))
}
