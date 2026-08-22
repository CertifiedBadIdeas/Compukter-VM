#[allow(dead_code)]
mod support;

use std::sync::Arc;

use compukter_vm::{
    verify_artifact, AdvanceOutcome, ArtifactLimits, CapabilityBinding, ExecutionProfile,
    HostResponse, HostValueInput, HostValueType, HostValueView, OperationSchema, RequestId,
    Session,
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

fn decode_hex(encoded: &str) -> Vec<u8> {
    let encoded = encoded.trim().as_bytes();
    assert_eq!(0, encoded.len() % 2);
    encoded
        .chunks_exact(2)
        .map(|pair| {
            let digit = |value: u8| match value {
                b'0'..=b'9' => value - b'0',
                b'a'..=b'f' => value - b'a' + 10,
                _ => panic!("invalid fixture hex"),
            };
            (digit(pair[0]) << 4) | digit(pair[1])
        })
        .collect()
}

#[test]
fn public_terminal_session_round_trips_utf16_requests_and_response() {
    const OUTPUT: &[u16] = &[0x003e, 0x0020, 0xd83d, 0xde00];
    const INPUT: &[u16] = &[0x0041, 0xd800, 0x0100, 0xdc00, 0x0042];
    let bytes = decode_hex(include_str!("fixtures/terminal-session.hex"));
    let artifact = verify_artifact(Arc::from(bytes), ArtifactLimits::default()).unwrap();
    let string_argument = [HostValueType::String];
    let operations = [
        OperationSchema::asynchronous(&string_argument, HostValueType::Unit),
        OperationSchema::asynchronous(&string_argument, HostValueType::Unit),
        OperationSchema::asynchronous(&[], HostValueType::String),
    ];
    let binding = CapabilityBinding::new("compukter", "terminal", 1, 0, &operations);
    let mut session = Session::admit(artifact, profile(), &[binding]).unwrap();
    session.start(&[]).unwrap();

    let next_request = |session: &mut Session, operation, argument: Option<&[u16]>| -> RequestId {
        loop {
            match session.advance(64, 64).unwrap() {
                AdvanceOutcome::SliceExhausted => {}
                AdvanceOutcome::HostRequest(request) => {
                    assert_eq!("compukter", request.namespace());
                    assert_eq!("terminal", request.name());
                    assert_eq!(operation, request.operation());
                    match argument {
                        Some(units) => assert_eq!(
                            Some(HostValueView::String(units)),
                            request.arguments().get(0)
                        ),
                        None => assert!(request.arguments().is_empty()),
                    }
                    return request.id();
                }
                other => panic!("{other:?}"),
            }
        }
    };

    for operation in [0, 1] {
        let id = next_request(&mut session, operation, Some(OUTPUT));
        session
            .resume(id, HostResponse::Success(HostValueInput::Unit))
            .unwrap();
    }
    let id = next_request(&mut session, 2, None);
    session
        .resume(id, HostResponse::Success(HostValueInput::String(INPUT)))
        .unwrap();
    loop {
        match session.advance(64, 64).unwrap() {
            AdvanceOutcome::SliceExhausted => {}
            AdvanceOutcome::Halted(Some(HostValueView::I32(_))) => break,
            other => panic!("{other:?}"),
        }
    }
    let accounting = session.accounting();
    assert_eq!(3, accounting.published_requests);
    assert_eq!(3, accounting.accepted_responses);
    assert_eq!(
        [
            0x51, 0x58, 0x89, 0x60, 0x02, 0x3f, 0xe3, 0xb9, 0xd4, 0xc1, 0xfe, 0x95, 0x35, 0xa5,
            0x12, 0x42, 0x7d, 0x34, 0xa4, 0x0a, 0xf5, 0x57, 0x40, 0xb9, 0xbc, 0x32, 0x9d, 0x4f,
            0x17, 0x55, 0x52, 0xf4,
        ],
        accounting.trace_digest
    );
}
