use super::{
    error::AdmissionError,
    fixtures,
    host::{
        AdvanceOutcome, CapabilityBinding, ExecutionProfile, HostFailure, HostFailureKind,
        HostResponse, HostValueInput, HostValueType, HostValueView, OperationSchema, QuotaKind,
        RequestId, ResumeError,
    },
    session::Session,
};
use crate::artifact::{Constant, ValueType};

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

fn scalar_case(
    value_type: ValueType,
    constant: Constant,
    host_type: HostValueType,
    expected: HostValueView<'static>,
    response: HostValueInput<'static>,
) {
    let arguments = [host_type];
    let operations = [OperationSchema::asynchronous(&arguments, host_type)];
    let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
    let mut session = Session::admit(
        fixtures::scalar_capability_artifact(value_type, constant),
        profile(),
        &[binding],
    )
    .unwrap();
    session.start(&[]).unwrap();

    let request_id = {
        let AdvanceOutcome::HostRequest(request) = session.advance(64, 0).unwrap() else {
            panic!("scalar call did not suspend");
        };
        assert_eq!("app", request.namespace());
        assert_eq!("entry", request.name());
        assert_eq!(0, request.operation());
        assert_eq!(Some(expected), request.arguments().get(0));
        request.id()
    };
    session
        .resume(request_id, HostResponse::Success(response))
        .unwrap();
    assert_eq!(
        AdvanceOutcome::Halted(Some(expected)),
        session.advance(64, 0).unwrap()
    );
}

#[test]
fn scalar_requests_round_trip_every_primitive_type() {
    for (value_type, constant, host_type, expected, response) in [
        (
            ValueType {
                kind: 1,
                flags: 0,
                nominal_type: crate::artifact::TypeId(u32::MAX),
            },
            Constant::I32(42),
            HostValueType::I32,
            HostValueView::I32(42),
            HostValueInput::I32(42),
        ),
        (
            ValueType {
                kind: 2,
                flags: 0,
                nominal_type: crate::artifact::TypeId(u32::MAX),
            },
            Constant::I64(42),
            HostValueType::I64,
            HostValueView::I64(42),
            HostValueInput::I64(42),
        ),
        (
            ValueType {
                kind: 3,
                flags: 0,
                nominal_type: crate::artifact::TypeId(u32::MAX),
            },
            Constant::F32(42),
            HostValueType::F32,
            HostValueView::F32(42),
            HostValueInput::F32(42),
        ),
        (
            ValueType {
                kind: 4,
                flags: 0,
                nominal_type: crate::artifact::TypeId(u32::MAX),
            },
            Constant::F64(42),
            HostValueType::F64,
            HostValueView::F64(42),
            HostValueInput::F64(42),
        ),
        (
            ValueType {
                kind: 5,
                flags: 0,
                nominal_type: crate::artifact::TypeId(u32::MAX),
            },
            Constant::Bool(true),
            HostValueType::Bool,
            HostValueView::Bool(true),
            HostValueInput::Bool(true),
        ),
        (
            ValueType {
                kind: 6,
                flags: 0,
                nominal_type: crate::artifact::TypeId(u32::MAX),
            },
            Constant::Char(42),
            HostValueType::Char,
            HostValueView::Char(42),
            HostValueInput::Char(42),
        ),
    ] {
        scalar_case(value_type, constant, host_type, expected, response);
    }
}

#[test]
fn waiting_poll_is_stable_and_invalid_responses_are_atomic() {
    let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
    let binding = CapabilityBinding::new("app", "entry", 1, 2, &operations);
    let mut session = Session::admit(
        fixtures::capability_artifact(true, true, 1, 0),
        profile(),
        &[binding],
    )
    .unwrap();
    session.start(&[]).unwrap();
    let first_id = match session.advance(64, 0).unwrap() {
        AdvanceOutcome::HostRequest(request) => request.id(),
        other => panic!("unexpected outcome: {other:?}"),
    };
    let repeated_id = match session.advance(1, 1).unwrap() {
        AdvanceOutcome::HostRequest(request) => request.id(),
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert_eq!(first_id, repeated_id);
    assert_eq!(
        ResumeError::WrongResponseType,
        session
            .resume(first_id, HostResponse::Success(HostValueInput::I32(7)))
            .unwrap_err()
    );
    assert!(matches!(
        session.advance(1, 1).unwrap(),
        AdvanceOutcome::HostRequest(_)
    ));
    assert_eq!(
        ResumeError::WrongRequestId,
        session
            .resume(
                RequestId::new(first_id.get() + 1).unwrap(),
                HostResponse::Success(HostValueInput::Unit)
            )
            .unwrap_err()
    );
    assert!(matches!(
        session.advance(1, 1).unwrap(),
        AdvanceOutcome::HostRequest(_)
    ));
    session
        .resume(first_id, HostResponse::Success(HostValueInput::Unit))
        .unwrap();
    assert_eq!(
        ResumeError::NoPendingRequest,
        session
            .resume(first_id, HostResponse::Success(HostValueInput::Unit))
            .unwrap_err()
    );
    assert_eq!(
        AdvanceOutcome::Halted(None),
        session.advance(64, 0).unwrap()
    );
}

#[test]
fn request_ids_are_monotonic_and_overflow_faults_before_publication() {
    let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
    let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
    let mut session = Session::admit(
        fixtures::two_unit_capability_calls_artifact(),
        profile(),
        &[binding],
    )
    .unwrap();
    session.start(&[]).unwrap();
    let first = match session.advance(64, 0).unwrap() {
        AdvanceOutcome::HostRequest(r) => r.id(),
        other => panic!("{other:?}"),
    };
    session
        .resume(first, HostResponse::Success(HostValueInput::Unit))
        .unwrap();
    let second = match session.advance(64, 0).unwrap() {
        AdvanceOutcome::HostRequest(r) => r.id(),
        other => panic!("{other:?}"),
    };
    assert!(second.get() > first.get());

    let overflow_binding = CapabilityBinding::new("app", "entry", 1, 2, &operations);
    let mut overflow = Session::admit(
        fixtures::capability_artifact(true, true, 1, 0),
        profile(),
        &[overflow_binding],
    )
    .unwrap();
    overflow.start(&[]).unwrap();
    overflow.test_set_next_request_id(u64::MAX);
    assert!(matches!(
        overflow.advance(64, 0).unwrap(),
        AdvanceOutcome::Faulted(_)
    ));
}

#[test]
fn explicit_host_failure_is_a_stable_terminal_outcome() {
    let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
    let binding = CapabilityBinding::new("app", "entry", 1, 2, &operations);
    let mut session = Session::admit(
        fixtures::capability_artifact(true, true, 1, 0),
        profile(),
        &[binding],
    )
    .unwrap();
    session.start(&[]).unwrap();
    let id = match session.advance(64, 0).unwrap() {
        AdvanceOutcome::HostRequest(r) => r.id(),
        other => panic!("{other:?}"),
    };
    let failure = HostFailure::new(HostFailureKind::Unavailable, 17);
    session.resume(id, HostResponse::Failure(failure)).unwrap();
    assert_eq!(
        AdvanceOutcome::HostFailed(failure),
        session.advance(64, 0).unwrap()
    );
    assert_eq!(
        AdvanceOutcome::HostFailed(failure),
        session.advance(1, 1).unwrap()
    );
}

fn string_session(
    code_units: &[u16],
    dynamic: bool,
    duplicate_argument: bool,
    mut execution_profile: ExecutionProfile,
) -> Session {
    execution_profile.maximum_host_arguments = 2;
    let argument_types: &[HostValueType] = if duplicate_argument {
        &[HostValueType::String, HostValueType::String]
    } else {
        &[HostValueType::String]
    };
    let operations = [OperationSchema::asynchronous(
        argument_types,
        HostValueType::Unit,
    )];
    let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
    let mut session = Session::admit(
        fixtures::string_capability_artifact(code_units, dynamic, duplicate_argument),
        execution_profile,
        &[binding],
    )
    .unwrap();
    session.start(&[]).unwrap();
    session
}

fn advance_to_request(session: &mut Session) -> (RequestId, Vec<Vec<u16>>) {
    assert_eq!(
        AdvanceOutcome::SliceExhausted,
        session.advance(64, 1).unwrap()
    );
    loop {
        match session.advance(1, 1).unwrap() {
            AdvanceOutcome::SliceExhausted => {}
            AdvanceOutcome::HostRequest(request) => {
                let values = (0..request.arguments().len())
                    .map(|index| match request.arguments().get(index).unwrap() {
                        HostValueView::String(value) => value.to_vec(),
                        other => panic!("unexpected argument: {other:?}"),
                    })
                    .collect();
                return (request.id(), values);
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
}

#[test]
fn outbound_string_preserves_literal_utf16_without_prefix_publication() {
    let units = [
        0x0041, 0xd83d, 0xde00, 0xd800, 0x00ff, 0x0100, 0xdc00, 0x0042, 0x0043,
    ];
    let mut session = string_session(&units, false, false, profile());
    let (id, arguments) = advance_to_request(&mut session);
    assert_eq!(vec![units.to_vec()], arguments);
    session
        .resume(id, HostResponse::Success(HostValueInput::Unit))
        .unwrap();
    assert_eq!(
        AdvanceOutcome::Halted(None),
        session.advance(64, 0).unwrap()
    );
}

#[test]
fn outbound_string_reads_compact_latin1_and_utf16_dynamic_backings() {
    for units in [&[0x0041, 0x00ff][..], &[0x0100, 0xd800][..]] {
        let mut session = string_session(units, true, true, profile());
        let (id, arguments) = advance_to_request(&mut session);
        let doubled = units
            .iter()
            .chain(units.iter())
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(vec![doubled.clone(), doubled], arguments);
        session
            .resume(id, HostResponse::Success(HostValueInput::Unit))
            .unwrap();
        assert_eq!(
            AdvanceOutcome::Halted(None),
            session.advance(64, 0).unwrap()
        );
    }
}

#[test]
fn outbound_string_limit_exhausts_before_request_publication() {
    let units = [0x0041; 9];
    let mut limited = profile();
    limited.maximum_outbound_utf16_code_units = 8;
    let mut session = string_session(&units, false, false, limited);
    let outcome = session.advance(64, 0).unwrap();
    let AdvanceOutcome::QuotaExhausted(exhaustion) = outcome else {
        panic!("unexpected outcome: {outcome:?}");
    };
    assert_eq!(QuotaKind::HostRequestCodeUnits, exhaustion.kind);
    assert_eq!(8, exhaustion.limit);
    assert_eq!(9, exhaustion.consumed);
    let expected = exhaustion;
    assert_eq!(
        AdvanceOutcome::QuotaExhausted(expected),
        session.advance(1, 1).unwrap()
    );
}

fn string_response_session(mut execution_profile: ExecutionProfile) -> (Session, RequestId) {
    execution_profile.maximum_host_arguments = 0;
    let operations = [OperationSchema::asynchronous(&[], HostValueType::String)];
    let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
    let mut session = Session::admit(
        fixtures::string_response_capability_artifact(),
        execution_profile,
        &[binding],
    )
    .unwrap();
    session.start(&[]).unwrap();
    let id = match session.advance(64, 0).unwrap() {
        AdvanceOutcome::HostRequest(request) => request.id(),
        other => panic!("unexpected outcome: {other:?}"),
    };
    (session, id)
}

fn kotlin_string_hash(units: &[u16]) -> i32 {
    units.iter().fold(0_i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(*unit))
    })
}

#[test]
fn inbound_string_materializes_exact_utf16_in_compact_managed_storage() {
    for units in [
        &[][..],
        &[0x0041, 0x00ff][..],
        &[0x0100, 0xd83d, 0xde00, 0xd800, 0xdc00][..],
    ] {
        let (mut session, id) = string_response_session(profile());
        session
            .resume(id, HostResponse::Success(HostValueInput::String(units)))
            .unwrap();
        assert_eq!(
            ResumeError::NoPendingRequest,
            session
                .resume(id, HostResponse::Success(HostValueInput::String(units)))
                .unwrap_err()
        );
        if !units.is_empty() {
            assert_eq!(
                AdvanceOutcome::SliceExhausted,
                session.advance(1, 1).unwrap()
            );
        }
        let terminal = loop {
            match session.advance(64, 1).unwrap() {
                AdvanceOutcome::SliceExhausted => {}
                outcome => break outcome,
            }
        };
        assert_eq!(
            AdvanceOutcome::Halted(Some(HostValueView::I32(kotlin_string_hash(units)))),
            terminal
        );
    }
}

#[test]
fn oversized_inbound_string_is_correctable_and_keeps_request_pending() {
    let mut limited = profile();
    limited.maximum_inbound_utf16_code_units = 2;
    let (mut session, id) = string_response_session(limited);
    assert_eq!(
        ResumeError::ResponseTooLarge,
        session
            .resume(
                id,
                HostResponse::Success(HostValueInput::String(&[0x41, 0x42, 0x43])),
            )
            .unwrap_err()
    );
    assert!(matches!(
        session.advance(1, 1).unwrap(),
        AdvanceOutcome::HostRequest(request) if request.id() == id
    ));
    session
        .resume(
            id,
            HostResponse::Success(HostValueInput::String(&[0x41, 0x42])),
        )
        .unwrap();
}

#[test]
fn inbound_string_oom_uses_the_managed_allocation_outcome() {
    let mut tiny = profile();
    tiny.heap_bytes = 32;
    let (mut session, id) = string_response_session(tiny);
    let units = [0x0100; 64];
    session
        .resume(id, HostResponse::Success(HostValueInput::String(&units)))
        .unwrap();
    let outcome = loop {
        match session.advance(1, 1).unwrap() {
            AdvanceOutcome::SliceExhausted => {}
            outcome => break outcome,
        }
    };
    assert!(matches!(outcome, AdvanceOutcome::AllocationExhausted(_)));
}

#[test]
fn inbound_string_collects_dead_guest_string_and_retries_once() {
    let mut constrained = profile();
    constrained.heap_bytes = 64;
    constrained.maximum_host_arguments = 0;
    let operations = [OperationSchema::asynchronous(&[], HostValueType::String)];
    let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
    let mut session = Session::admit(
        fixtures::string_response_gc_retry_artifact(),
        constrained,
        &[binding],
    )
    .unwrap();
    session.start(&[]).unwrap();
    let id = loop {
        match session.advance(64, 1).unwrap() {
            AdvanceOutcome::SliceExhausted => {}
            AdvanceOutcome::HostRequest(request) => break request.id(),
            other => panic!("unexpected outcome: {other:?}"),
        }
    };
    let units = [0x0101; 8];
    session
        .resume(id, HostResponse::Success(HostValueInput::String(&units)))
        .unwrap();
    let terminal = loop {
        match session.advance(64, 1).unwrap() {
            AdvanceOutcome::SliceExhausted => {}
            outcome => break outcome,
        }
    };
    assert_eq!(
        AdvanceOutcome::Halted(Some(HostValueView::I32(kotlin_string_hash(&units)))),
        terminal
    );
}
