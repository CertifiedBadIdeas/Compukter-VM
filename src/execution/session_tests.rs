use super::{
    error::AdmissionError,
    fixtures,
    host::{CapabilityBinding, ExecutionProfile, HostValueType, OperationSchema},
    session::Session,
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

fn operation() -> [OperationSchema<'static>; 1] {
    [OperationSchema::asynchronous(&[], HostValueType::Unit)]
}

#[test]
fn admission_resolves_exact_identity_major_and_minimum_minor() {
    let operations = operation();
    let binding = CapabilityBinding::new("app", "entry", 1, 2, &operations);
    Session::admit(
        fixtures::capability_artifact(true, true, 1, 0),
        profile(),
        &[binding],
    )
    .unwrap();
}

#[test]
fn admission_rejects_missing_and_incompatible_capabilities() {
    let artifact = || fixtures::capability_artifact(true, true, 1, 0);
    assert_eq!(
        AdmissionError::MissingCapability { index: 0 },
        Session::admit(artifact(), profile(), &[]).unwrap_err()
    );

    let operations = operation();
    for binding in [
        CapabilityBinding::new("wrong", "entry", 1, 2, &operations),
        CapabilityBinding::new("app", "wrong", 1, 2, &operations),
        CapabilityBinding::new("app", "entry", 2, 2, &operations),
        CapabilityBinding::new("app", "entry", 1, 1, &operations),
    ] {
        assert_eq!(
            AdmissionError::MissingCapability { index: 0 },
            Session::admit(artifact(), profile(), &[binding]).unwrap_err()
        );
    }
}

#[test]
fn admission_rejects_duplicate_bindings_and_short_operation_tables() {
    let operations = operation();
    let first = CapabilityBinding::new("app", "entry", 1, 2, &operations);
    let duplicate = CapabilityBinding::new("app", "entry", 1, 3, &operations);
    assert!(matches!(
        Session::admit(
            fixtures::capability_artifact(true, true, 1, 0),
            profile(),
            &[first, duplicate],
        ),
        Err(AdmissionError::DuplicateCapabilityBinding)
    ));

    let empty = [];
    let binding = CapabilityBinding::new("app", "entry", 1, 2, &empty);
    assert!(matches!(
        Session::admit(
            fixtures::capability_artifact(true, true, 1, 0),
            profile(),
            &[binding],
        ),
        Err(AdmissionError::CapabilityOperationCount { .. })
    ));
}

#[test]
fn admission_rejects_invoked_unbound_optional_and_sync_calls() {
    assert!(matches!(
        Session::admit(
            fixtures::capability_artifact(true, false, 1, 0),
            profile(),
            &[],
        ),
        Err(AdmissionError::MissingCapability { index: 0 })
    ));

    let operations = operation();
    let binding = CapabilityBinding::new("app", "entry", 1, 2, &operations);
    assert_eq!(
        AdmissionError::SynchronousCapabilityUnsupported,
        Session::admit(
            fixtures::capability_artifact(false, true, 1, 0),
            profile(),
            &[binding],
        )
        .unwrap_err()
    );
}
