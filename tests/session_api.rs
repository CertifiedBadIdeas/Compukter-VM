#[allow(dead_code)]
mod support;

use std::sync::Arc;

use compukter_vm::{
    verify_artifact, AdvanceOutcome, ArtifactLimits, CapabilityBinding, ExecutionProfile,
    HostValueType, OperationSchema, Session,
};

fn profile() -> ExecutionProfile {
    ExecutionProfile {
        heap_bytes: 1024 * 1024,
        frame_storage_bytes: 1024 * 1024,
        maximum_call_depth: 64,
        maximum_coroutines: 64,
        maximum_host_requests: 64,
        maximum_events: 64,
        maximum_slice_budget: u32::MAX,
        compiler_abi: [0; 32],
        standard_library_abi: [0; 32],
        maximum_host_arguments: 16,
        maximum_outbound_utf16_code_units: 4096,
        maximum_inbound_utf16_code_units: 4096,
        maximum_accepted_responses: 64,
    }
}

#[test]
fn verified_artifact_admits_into_a_public_session() {
    let artifact = verify_artifact(
        Arc::from(support::executable_minimal_vector()),
        ArtifactLimits::default(),
    )
    .unwrap();

    let mut session = Session::admit(artifact, profile(), &[]).unwrap();
    session.start(&[]).unwrap();
    assert_eq!(
        AdvanceOutcome::Halted(None),
        session.advance(64, 0).unwrap()
    );
    let accounting = session.accounting();
    assert_eq!(0, accounting.published_requests);
    assert_eq!(0, accounting.accepted_responses);
}

#[test]
fn public_capability_schema_is_host_owned_input() {
    let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
    let binding = CapabilityBinding::new("example.host", "clock", 1, 0, &operations);

    assert_eq!("example.host", binding.namespace());
    assert_eq!("clock", binding.name());
    assert_eq!(1, binding.abi_major());
    assert_eq!(0, binding.abi_minor());
    assert_eq!(operations.as_slice(), binding.operations());
}
