use super::{
    error::{AdmissionError, EntryArgumentLimit, RunError},
    fixtures,
    host::{
        AdvanceOutcome, CapabilityBinding, EntryArgumentLimits, EntryValue, ExecutionProfile,
        HostFailure, HostFailureKind, HostResponse, HostValueInput, HostValueType, HostValueView,
        OperationSchema, QuotaKind, RequestId, ResumeError,
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
        entry_argument_limits: EntryArgumentLimits {
            maximum_count: 64,
            maximum_code_units_per_argument: 4096,
            maximum_total_code_units: 16_384,
        },
    }
}

fn operation() -> [OperationSchema<'static>; 1] {
    [OperationSchema::asynchronous(&[], HostValueType::Unit)]
}

#[test]
fn entry_string_array_is_materialized_as_one_owned_guest_argument() {
    let mut session = Session::admit(
        fixtures::entry_string_array_length_artifact(),
        profile(),
        &[],
    )
    .unwrap();
    let arguments = [
        Vec::<u16>::new().into_boxed_slice(),
        vec![0x0041, 0x0000, 0xd800, 0x0042].into_boxed_slice(),
    ];

    session
        .start(&[EntryValue::StringArray(&arguments)])
        .unwrap();
    assert_eq!(
        AdvanceOutcome::Halted(Some(super::host::HostValueView::I32(2))),
        session.advance(64, 0).unwrap(),
    );
}

#[test]
fn entry_string_array_bounds_are_checked_before_start() {
    let cases = [
        (
            EntryArgumentLimits {
                maximum_count: 1,
                maximum_code_units_per_argument: 8,
                maximum_total_code_units: 8,
            },
            vec![vec![1].into_boxed_slice(), vec![2].into_boxed_slice()],
            EntryArgumentLimit::Count,
        ),
        (
            EntryArgumentLimits {
                maximum_count: 2,
                maximum_code_units_per_argument: 1,
                maximum_total_code_units: 8,
            },
            vec![vec![1, 2].into_boxed_slice()],
            EntryArgumentLimit::ArgumentCodeUnits,
        ),
        (
            EntryArgumentLimits {
                maximum_count: 2,
                maximum_code_units_per_argument: 8,
                maximum_total_code_units: 1,
            },
            vec![vec![1].into_boxed_slice(), vec![2].into_boxed_slice()],
            EntryArgumentLimit::TotalCodeUnits,
        ),
    ];

    for (limits, arguments, expected) in cases {
        let mut execution_profile = profile();
        execution_profile.entry_argument_limits = limits;
        let mut session = Session::admit(
            fixtures::entry_string_array_length_artifact(),
            execution_profile,
            &[],
        )
        .unwrap();
        assert_eq!(
            Err(RunError::EntryArgumentLimit(expected)),
            session.start(&[EntryValue::StringArray(&arguments)]),
        );

        let empty: [Box<[u16]>; 0] = [];
        session.start(&[EntryValue::StringArray(&empty)]).unwrap();
        assert_eq!(
            AdvanceOutcome::Halted(Some(HostValueView::I32(0))),
            session.advance(64, 0).unwrap(),
        );
    }
}

#[test]
fn entry_string_array_preserves_utf16_code_units_verbatim() {
    let arguments = [
        vec![0x0041, 0x0000, 0xd800, 0x0042].into_boxed_slice(),
        Vec::<u16>::new().into_boxed_slice(),
    ];
    for (index, expected) in [(1, 0x0000), (2, 0xd800)] {
        let mut session = Session::admit(
            fixtures::entry_string_array_code_unit_artifact(0, index),
            profile(),
            &[],
        )
        .unwrap();
        session
            .start(&[EntryValue::StringArray(&arguments)])
            .unwrap();
        assert_eq!(
            AdvanceOutcome::Halted(Some(HostValueView::Char(expected))),
            session.advance(64, 0).unwrap(),
        );
    }
}

#[test]
fn entry_string_array_allocation_failure_leaves_session_pristine() {
    let mut execution_profile = profile();
    execution_profile.heap_bytes = 256;
    execution_profile
        .entry_argument_limits
        .maximum_code_units_per_argument = 4096;
    execution_profile
        .entry_argument_limits
        .maximum_total_code_units = 4096;
    let mut session = Session::admit(
        fixtures::entry_string_array_length_artifact(),
        execution_profile,
        &[],
    )
    .unwrap();
    let large = [vec![0x0100; 1024].into_boxed_slice()];
    assert_eq!(
        Err(RunError::EntryAllocationFailed),
        session.start(&[EntryValue::StringArray(&large)]),
    );

    let empty: [Box<[u16]>; 0] = [];
    session.start(&[EntryValue::StringArray(&empty)]).unwrap();
    assert_eq!(
        AdvanceOutcome::Halted(Some(HostValueView::I32(0))),
        session.advance(64, 0).unwrap(),
    );
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
fn admission_rejects_invoked_unbound_optional_calls_and_runs_sync_calls() {
    assert!(matches!(
        Session::admit(
            fixtures::capability_artifact(true, false, 1, 0),
            profile(),
            &[],
        ),
        Err(AdmissionError::MissingCapability { index: 0 })
    ));

    let operations = [OperationSchema::synchronous(&[], HostValueType::Unit)];
    let binding = CapabilityBinding::new("app", "entry", 1, 2, &operations);
    let mut session = Session::admit(
        fixtures::capability_artifact(false, true, 1, 0),
        profile(),
        &[binding],
    )
    .unwrap();
    session.start(&[]).unwrap();

    let request_id = match session.advance(64, 0).unwrap() {
        AdvanceOutcome::HostRequest(request) => {
            assert!(!request.asynchronous());
            request.id()
        }
        other => panic!("synchronous capability did not publish a request: {other:?}"),
    };
    session
        .resume(request_id, HostResponse::Success(HostValueInput::Unit))
        .unwrap();
    assert_eq!(
        AdvanceOutcome::Halted(None),
        session.advance(64, 0).unwrap()
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
fn suspend_callee_can_wait_for_and_return_a_host_response() {
    let operations = [OperationSchema::asynchronous(&[], HostValueType::I32)];
    let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
    let mut session = Session::admit(
        fixtures::suspend_scalar_capability_artifact(),
        profile(),
        &[binding],
    )
    .unwrap();
    session.start(&[]).unwrap();

    let request_id = match session.advance(128, 0).unwrap() {
        AdvanceOutcome::HostRequest(request) => {
            assert!(request.asynchronous());
            request.id()
        }
        other => panic!("expected host request, got {other:?}"),
    };
    session
        .resume(request_id, HostResponse::Success(HostValueInput::I32(7)))
        .unwrap();
    assert_eq!(
        AdvanceOutcome::Halted(Some(HostValueView::I32(7))),
        session.advance(128, 0).unwrap()
    );
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
    let waiting_accounting = session.accounting();
    let repeated_id = match session.advance(1, 1).unwrap() {
        AdvanceOutcome::HostRequest(request) => request.id(),
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert_eq!(first_id, repeated_id);
    assert_eq!(waiting_accounting, session.accounting());
    assert_eq!(
        ResumeError::WrongResponseType,
        session
            .resume(first_id, HostResponse::Success(HostValueInput::I32(7)))
            .unwrap_err()
    );
    assert_eq!(waiting_accounting, session.accounting());
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
    assert_eq!(waiting_accounting, session.accounting());
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
fn string_request_id_overflow_faults_before_copying_or_dynamic_charge() {
    let units = [
        0x0041, 0xd800, 0x0100, 0x0042, 0x0043, 0x0044, 0x0045, 0x0046, 0x0047,
    ];
    let mut session = string_session(&units, false, false, profile());
    session.test_set_next_request_id(u64::MAX);

    assert_eq!(
        AdvanceOutcome::Faulted(super::error::VmFault::AccountingOverflow),
        session.advance(64, 0).unwrap()
    );
    let accounting = session.accounting();
    assert_eq!(0, accounting.dynamic_guest_units);
    assert_eq!(0, accounting.published_requests);
    assert_eq!(0, accounting.accepted_responses);
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

fn unit_loop_session(maximum_requests: u32, maximum_responses: u64) -> Session {
    let mut execution_profile = profile();
    execution_profile.maximum_host_arguments = 0;
    execution_profile.maximum_host_requests = maximum_requests;
    execution_profile.maximum_accepted_responses = maximum_responses;
    let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
    let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
    let mut session = Session::admit(
        fixtures::unit_capability_loop_artifact(maximum_requests),
        execution_profile,
        &[binding],
    )
    .unwrap();
    session.start(&[]).unwrap();
    session
}

#[test]
fn determinism_is_independent_of_wait_poll_count() {
    let run = |polls: usize| {
        let mut session = unit_loop_session(1, 1);
        let id = match session.advance(64, 0).unwrap() {
            AdvanceOutcome::HostRequest(request) => request.id(),
            other => panic!("{other:?}"),
        };
        let waiting = session.accounting();
        for _ in 0..polls {
            assert!(matches!(
                session.advance(1, 1).unwrap(),
                AdvanceOutcome::HostRequest(_)
            ));
            assert_eq!(waiting, session.accounting());
        }
        session
            .resume(id, HostResponse::Success(HostValueInput::Unit))
            .unwrap();
        assert!(matches!(
            session.advance(64, 0).unwrap(),
            AdvanceOutcome::QuotaExhausted(_)
        ));
        let accounting = session.accounting();
        assert_eq!(1, accounting.published_requests);
        assert_eq!(1, accounting.accepted_responses);
        accounting
    };
    assert_eq!(run(0), run(17));
}

#[test]
fn request_and_response_quotas_are_distinct_stable_outcomes() {
    let mut request_limited = unit_loop_session(1, 1);
    let id = match request_limited.advance(64, 0).unwrap() {
        AdvanceOutcome::HostRequest(request) => request.id(),
        other => panic!("{other:?}"),
    };
    request_limited
        .resume(id, HostResponse::Success(HostValueInput::Unit))
        .unwrap();
    let request_exhaustion = match request_limited.advance(64, 0).unwrap() {
        AdvanceOutcome::QuotaExhausted(value) => value,
        other => panic!("{other:?}"),
    };
    assert_eq!(QuotaKind::HostRequests, request_exhaustion.kind);
    assert_eq!(1, request_exhaustion.limit);
    assert_eq!(1, request_exhaustion.consumed);
    assert_eq!(1, request_limited.accounting().published_requests);
    assert_eq!(1, request_limited.accounting().accepted_responses);
    assert_eq!(
        AdvanceOutcome::QuotaExhausted(request_exhaustion),
        request_limited.advance(1, 1).unwrap()
    );

    let mut response_limited = unit_loop_session(1, 0);
    let id = match response_limited.advance(64, 0).unwrap() {
        AdvanceOutcome::HostRequest(request) => request.id(),
        other => panic!("{other:?}"),
    };
    response_limited
        .resume(id, HostResponse::Success(HostValueInput::Unit))
        .unwrap();
    let response_exhaustion = match response_limited.advance(1, 1).unwrap() {
        AdvanceOutcome::QuotaExhausted(value) => value,
        other => panic!("{other:?}"),
    };
    assert_eq!(QuotaKind::AcceptedResponses, response_exhaustion.kind);
    assert_eq!(0, response_exhaustion.limit);
    assert_eq!(0, response_exhaustion.consumed);
    assert_eq!(1, response_limited.accounting().published_requests);
    assert_eq!(0, response_limited.accounting().accepted_responses);
    assert_eq!(
        AdvanceOutcome::QuotaExhausted(response_exhaustion),
        response_limited.advance(1, 1).unwrap()
    );
}

#[test]
fn steady_state_request_resume_allocates_nothing() {
    const ITERATIONS: u32 = 10_000;
    let mut session = unit_loop_session(ITERATIONS + 1, u64::from(ITERATIONS + 1));
    super::tests::allocation_counter::reset_and_enable();
    for _ in 0..ITERATIONS {
        let id = match session.advance(64, 0).unwrap() {
            AdvanceOutcome::HostRequest(request) => request.id(),
            other => panic!("{other:?}"),
        };
        session
            .resume(id, HostResponse::Success(HostValueInput::Unit))
            .unwrap();
    }
    let allocations = super::tests::allocation_counter::disable_and_read();
    assert_eq!(0, allocations);

    let mut execution_profile = profile();
    execution_profile.maximum_host_arguments = 0;
    execution_profile.maximum_host_requests = ITERATIONS + 1;
    execution_profile.maximum_accepted_responses = u64::from(ITERATIONS + 1);
    let operations = [OperationSchema::asynchronous(&[], HostValueType::String)];
    let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
    let mut string_session = Session::admit(
        fixtures::string_response_loop_artifact(ITERATIONS + 1),
        execution_profile,
        &[binding],
    )
    .unwrap();
    string_session.start(&[]).unwrap();
    let units = [0x0041, 0xd800, 0xdc00, 0x0100];
    super::tests::allocation_counter::reset_and_enable();
    for _ in 0..ITERATIONS {
        let id = loop {
            match string_session.advance(64, 64).unwrap() {
                AdvanceOutcome::SliceExhausted => {}
                AdvanceOutcome::HostRequest(request) => break request.id(),
                other => panic!("{other:?}"),
            }
        };
        string_session
            .resume(id, HostResponse::Success(HostValueInput::String(&units)))
            .unwrap();
    }
    let allocations = super::tests::allocation_counter::disable_and_read();
    assert_eq!(0, allocations);
}

#[test]
fn terminal_vertical_conformance() {
    const OUTPUT: &[u16] = &[0x003e, 0x0020, 0xd83d, 0xde00];
    const INPUT: &[u16] = &[0x0041, 0xd800, 0x0100, 0xdc00, 0x0042];
    let mut execution_profile = profile();
    execution_profile.maximum_host_arguments = 1;
    execution_profile.maximum_host_requests = 3;
    execution_profile.maximum_accepted_responses = 3;
    let string_argument = [HostValueType::String];
    let operations = [
        OperationSchema::asynchronous(&string_argument, HostValueType::Unit),
        OperationSchema::asynchronous(&string_argument, HostValueType::Unit),
        OperationSchema::asynchronous(&[], HostValueType::String),
    ];
    let binding = CapabilityBinding::new("compukter", "terminal", 1, 0, &operations);
    let mut session = Session::admit(
        fixtures::terminal_conformance_artifact(OUTPUT),
        execution_profile,
        &[binding],
    )
    .unwrap();
    session.start(&[]).unwrap();

    for (operation, expected_id) in [(0, 1), (1, 2)] {
        let id = loop {
            match session.advance(64, 64).unwrap() {
                AdvanceOutcome::SliceExhausted => {}
                AdvanceOutcome::HostRequest(request) => {
                    assert_eq!("compukter", request.namespace());
                    assert_eq!("terminal", request.name());
                    assert_eq!(1, request.abi_major());
                    assert_eq!(0, request.abi_minor());
                    assert_eq!(operation, request.operation());
                    assert_eq!(
                        Some(HostValueView::String(OUTPUT)),
                        request.arguments().get(0)
                    );
                    assert_eq!(expected_id, request.id().get());
                    break request.id();
                }
                other => panic!("{other:?}"),
            }
        };
        session
            .resume(id, HostResponse::Success(HostValueInput::Unit))
            .unwrap();
    }

    let read_id = loop {
        match session.advance(64, 64).unwrap() {
            AdvanceOutcome::SliceExhausted => {}
            AdvanceOutcome::HostRequest(request) => {
                assert_eq!("compukter", request.namespace());
                assert_eq!("terminal", request.name());
                assert_eq!(1, request.abi_major());
                assert_eq!(0, request.abi_minor());
                assert_eq!(2, request.operation());
                assert!(request.arguments().is_empty());
                assert_eq!(3, request.id().get());
                break request.id();
            }
            other => panic!("{other:?}"),
        }
    };
    session
        .resume(
            read_id,
            HostResponse::Success(HostValueInput::String(INPUT)),
        )
        .unwrap();

    let expected_terminal =
        AdvanceOutcome::Halted(Some(HostValueView::I32(kotlin_string_hash(INPUT))));
    loop {
        match session.advance(64, 64).unwrap() {
            AdvanceOutcome::SliceExhausted => {}
            outcome => {
                assert_eq!(expected_terminal, outcome);
                break;
            }
        }
    }
    assert_eq!(expected_terminal, session.advance(1, 1).unwrap());

    let accounting = session.accounting();
    assert_eq!(23, accounting.fixed_guest_units);
    assert_eq!(6, accounting.dynamic_guest_units);
    assert_eq!(0, accounting.maintenance_units);
    assert_eq!(4, accounting.entered_blocks);
    assert_eq!(6, accounting.executed_instructions);
    assert_eq!(3, accounting.published_requests);
    assert_eq!(3, accounting.accepted_responses);
    assert_eq!(
        [
            0x90, 0x57, 0x7f, 0x50, 0x3a, 0xaf, 0x6f, 0x43, 0x69, 0x24, 0xc3, 0xf0, 0xa7, 0x9c,
            0xb0, 0x6c, 0xe9, 0x78, 0xa7, 0x0d, 0x8a, 0xac, 0x6d, 0x10, 0xf5, 0x34, 0x19, 0x0b,
            0x91, 0xf5, 0x07, 0x5e,
        ],
        accounting.trace_digest
    );
}

#[test]
#[ignore = "records a hardware-specific host-session performance baseline"]
fn host_session_performance_baseline() {
    use std::time::Instant;

    const WARMUP: u32 = 10_000;
    const ITERATIONS: u32 = 100_000;
    let mut session =
        unit_loop_session(WARMUP + ITERATIONS + 1, u64::from(WARMUP + ITERATIONS + 1));
    let cycle = |session: &mut Session| {
        let id = match session.advance(64, 0).unwrap() {
            AdvanceOutcome::HostRequest(request) => request.id(),
            other => panic!("{other:?}"),
        };
        session
            .resume(id, HostResponse::Success(HostValueInput::Unit))
            .unwrap();
    };
    for _ in 0..WARMUP {
        cycle(&mut session);
    }
    let reserved = session.test_reserved_mutable_bytes();
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        cycle(&mut session);
    }
    let elapsed = started.elapsed();
    println!("iterations\telapsed_ns\trequest_resume_per_s\treserved_mutable_bytes");
    println!(
        "{ITERATIONS}\t{}\t{:.0}\t{reserved}",
        elapsed.as_nanos(),
        f64::from(ITERATIONS) / elapsed.as_secs_f64(),
    );
}
