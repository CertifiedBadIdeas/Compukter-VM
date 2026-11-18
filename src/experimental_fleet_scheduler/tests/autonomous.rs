/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

use super::*;

fn fleet(mut config: FleetConfig, inspect: bool) -> FleetScheduler {
    config.workers = 1;
    let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
    let binding = CapabilityBinding::new("app", "entry", 1, 0, &operations);
    let machine = ComputerMachine::start(
        crate::execution::fixtures::two_unit_capability_calls_artifact(),
        profile(),
        &[binding],
        &[],
    )
    .unwrap();
    let inspections = if inspect {
        vec![RequestInspection {
            namespace: "app".into(),
            name: "entry".into(),
            abi_major: 1,
            abi_minor: 0,
            operation: 0,
            over_budget_detail: "group exceeds budget".into(),
        }]
    } else {
        Vec::new()
    };
    FleetScheduler::new_with_autonomous_frames(
        vec![(1, machine)],
        config,
        inspections,
        FleetMemoryLimits::default(),
    )
    .unwrap()
}

fn grant(fleet: &FleetScheduler, tick: u64) {
    fleet
        .grant_frame(
            1,
            tick,
            64,
            Duration::from_secs(1),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
}

fn request(fleet: &FleetScheduler) -> ComputerHostRequest {
    loop {
        match next(fleet) {
            FleetEvent::Requests { mut requests, .. } => return requests.remove(0),
            FleetEvent::Quiescent { .. } => {}
            other => panic!("{other:?}"),
        }
    }
}

fn quiescent(fleet: &FleetScheduler) -> u64 {
    match next(fleet) {
        FleetEvent::Quiescent { retired, .. } => retired,
        other => panic!("{other:?}"),
    }
}

#[test]
fn reply_resumes_after_observational_report_without_another_grant() {
    let fleet = fleet(config(), false);
    grant(&fleet, 1);
    let first = request(&fleet);
    let before = quiescent(&fleet);
    fleet
        .reply(1, first.task_id, first.id, ReplyValue::Unit)
        .unwrap();
    let second = request(&fleet);
    assert_ne!(first.id, second.id);
    assert!(quiescent(&fleet) > before);
    fleet
        .reply(1, second.task_id, second.id, ReplyValue::Unit)
        .unwrap();
    assert!(matches!(
        next(&fleet),
        FleetEvent::Boundary {
            outcome: ComputerAdvanceOutcome::Halted(_),
            tick: 1,
            ..
        }
    ));
}

#[test]
fn cost_resolution_resumes_without_another_grant() {
    let fleet = fleet(config(), true);
    grant(&fleet, 1);
    let inspected = match next(&fleet) {
        FleetEvent::Inspect { request, .. } => request,
        other => panic!("{other:?}"),
    };
    quiescent(&fleet);
    fleet
        .resolve_cost(1, inspected.task_id, inspected.id, 2)
        .unwrap();
    let dispatched = request(&fleet);
    assert_eq!(inspected.id, dispatched.id);
}

#[test]
fn replies_do_not_refill_request_credit_and_duplicate_frames_do_not_either() {
    let mut config = config();
    config.host_budget = 1;
    let fleet = fleet(config, false);
    grant(&fleet, 1);
    let first = request(&fleet);
    quiescent(&fleet);
    fleet
        .reply(1, first.task_id, first.id, ReplyValue::Unit)
        .unwrap();
    quiescent(&fleet);
    let retired = fleet.metrics().retired;
    grant(&fleet, 1);
    // A query is an ordered mailbox barrier, without injecting execution credit.
    fleet.terminal_update(1, 99, u64::MAX).unwrap();
    assert!(matches!(
        next(&fleet),
        FleetEvent::TerminalUpdate { request: 99, .. }
    ));
    assert_eq!(fleet.metrics().retired, retired);
    grant(&fleet, 2);
    assert_ne!(request(&fleet).id, first.id);
}

#[test]
fn expired_authorization_blocks_even_with_unspent_compute_credit() {
    let fleet = fleet(config(), false);
    fleet
        .grant_frame(1, 1, 64, Duration::from_secs(1), Instant::now())
        .unwrap();
    assert_eq!(quiescent(&fleet), 0);
    assert_eq!(fleet.metrics().advances, 0);
    grant(&fleet, 1);
    fleet.terminal_update(1, 99, u64::MAX).unwrap();
    assert!(matches!(next(&fleet), FleetEvent::TerminalUpdate { .. }));
    assert_eq!(fleet.metrics().advances, 0);
    grant(&fleet, 2);
    request(&fleet);
}

#[test]
fn zero_compute_credit_blocks_all_machines_on_the_same_worker() {
    let make = || {
        ComputerMachine::start(
            crate::execution::fixtures::empty_loop_artifact(1),
            profile(),
            &[],
            &[],
        )
        .unwrap()
    };
    let mut config = config();
    config.workers = 1;
    let fleet = FleetScheduler::new_with_autonomous_frames(
        vec![(1, make()), (2, make())],
        config,
        Vec::new(),
        FleetMemoryLimits::default(),
    )
    .unwrap();
    fleet
        .grant_frame(
            1,
            1,
            64,
            Duration::ZERO,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(quiescent(&fleet), 0);
    fleet
        .grant_frame(
            2,
            1,
            64,
            Duration::from_secs(1),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(quiescent(&fleet), 0);
    assert_eq!(fleet.metrics().advances, 0);
    grant(&fleet, 2);
    assert!(quiescent(&fleet) > 0);
}

#[test]
fn frame_and_legacy_grants_are_not_interchangeable() {
    let fleet = fleet(config(), false);
    assert_eq!(fleet.grant(1, 1, 64), Err(SubmitError::InvalidInput));
    let legacy = FleetScheduler::new(Vec::new(), config()).unwrap();
    assert_eq!(
        legacy.grant_frame(1, 1, 64, Duration::ZERO, Instant::now()),
        Err(SubmitError::InvalidInput)
    );
}

#[test]
fn host_wait_does_not_spend_execution_time() {
    let fleet = fleet(config(), false);
    let allowance = Duration::from_millis(100);
    fleet
        .grant_frame(1, 1, 64, allowance, Instant::now() + Duration::from_secs(5))
        .unwrap();
    let first = request(&fleet);
    quiescent(&fleet);
    // Deliberately exceed the whole execution allowance while no native slice runs.
    thread::sleep(allowance + Duration::from_millis(20));
    fleet
        .reply(1, first.task_id, first.id, ReplyValue::Unit)
        .unwrap();
    assert_ne!(request(&fleet).id, first.id);
}

#[test]
fn reply_is_retained_without_execution_credit_until_a_fresh_frame() {
    let mut config = config();
    config.maximum_advances_per_tick = 1;
    let fleet = fleet(config, false);
    grant(&fleet, 1);
    let first = request(&fleet);
    quiescent(&fleet);
    fleet
        .reply(1, first.task_id, first.id, ReplyValue::Unit)
        .unwrap();
    // The query is an ordered mailbox barrier: the reply has been accepted,
    // but it must not authorize another native slice.
    fleet.terminal_update(1, 99, u64::MAX).unwrap();
    assert!(matches!(next(&fleet), FleetEvent::TerminalUpdate { .. }));
    let retired = fleet.metrics().retired;
    thread::sleep(Duration::from_millis(20));
    assert_eq!(fleet.metrics().advances, 1);
    assert_eq!(fleet.metrics().retired, retired);
    assert!(fleet.try_event().is_none());

    // A new Grant wakes the worker; the reply must not need retransmission.
    grant(&fleet, 2);
    let second = request(&fleet);
    assert_ne!(second.id, first.id);
    quiescent(&fleet);
    fleet
        .reply(1, second.task_id, second.id, ReplyValue::Unit)
        .unwrap();
    fleet.terminal_update(1, 100, u64::MAX).unwrap();
    assert!(matches!(next(&fleet), FleetEvent::TerminalUpdate { .. }));
    assert_eq!(fleet.metrics().advances, 2);
    grant(&fleet, 3);
    assert!(matches!(
        next(&fleet),
        FleetEvent::Boundary {
            outcome: ComputerAdvanceOutcome::Halted(_),
            tick: 3,
            ..
        }
    ));
}

#[test]
fn automatic_frames_preserve_retirement_limit() {
    let machine = ComputerMachine::start(
        crate::execution::fixtures::empty_loop_artifact(1),
        profile(),
        &[],
        &[],
    )
    .unwrap();
    let fleet = FleetScheduler::new_with_autonomous_frames(
        vec![(1, machine)],
        config(),
        Vec::new(),
        FleetMemoryLimits::default(),
    )
    .unwrap();
    for tick in [1, 2] {
        fleet
            .grant_frame(
                1,
                tick,
                1,
                Duration::from_secs(1),
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(quiescent(&fleet), 1);
        assert_eq!(fleet.metrics().retired, tick);
    }
}

#[test]
fn progress_explains_budget_exhaustion_and_waiting_without_losing_unused_credit() {
    let fleet = fleet(config(), false);
    fleet
        .grant_frame(
            1,
            1,
            64,
            Duration::ZERO,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    let FleetEvent::Quiescent {
        frame: Some(progress),
        ..
    } = next(&fleet)
    else {
        panic!("missing frame accounting");
    };
    assert_eq!(
        progress,
        FleetFrameProgress {
            execution_nanos: 0,
            instructions_left: 64,
            requests_left: 64,
            advances_left: 32,
            reason: FleetWaitReason::ExecutionTime,
        }
    );
    grant(&fleet, 2);
    request(&fleet);
    let FleetEvent::Quiescent {
        frame: Some(progress),
        ..
    } = next(&fleet)
    else {
        panic!("missing frame accounting");
    };
    assert_eq!(progress.reason, FleetWaitReason::Host);
    assert!(progress.instructions_left > 0);
    assert_eq!(progress.requests_left, 63);
    assert!(progress.execution_nanos > 0);
    fleet.stop_with_lifecycle_poll(1).unwrap();
    wait_lifecycle(&fleet, 1);
    let stopped = fleet.try_stopped(1).unwrap();
    let final_progress = stopped.quiescent.unwrap().frame.unwrap();
    assert_eq!(final_progress.execution_nanos, progress.execution_nanos);
}

#[test]
fn terminal_input_resumes_the_same_frame() {
    use crate::{TerminalKey, TerminalKeyAction, TerminalKeyEvent, TerminalModifiers};
    for canonical in [false, true] {
        let machine = ComputerMachine::start(
            crate::execution::fixtures::stdio_conformance_artifact(&[]),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        let fleet = FleetScheduler::new_with_autonomous_frames(
            vec![(1, machine)],
            config(),
            Vec::new(),
            FleetMemoryLimits::default(),
        )
        .unwrap();
        fleet
            .grant_frame(
                1,
                1,
                100_000,
                Duration::from_secs(1),
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
        assert!(matches!(
            next(&fleet),
            FleetEvent::Boundary {
                outcome: ComputerAdvanceOutcome::WaitingForTerminalEvent,
                ..
            }
        ));
        quiescent(&fleet);
        if canonical {
            fleet
                .canonical_line(1, "hello".encode_utf16().collect())
                .unwrap();
        } else {
            fleet.terminal_text(1, "hello".into()).unwrap();
            fleet.terminal_update(1, 99, u64::MAX).unwrap();
            loop {
                if matches!(next(&fleet), FleetEvent::TerminalUpdate { request: 99, .. }) {
                    break;
                }
            }
            fleet
                .terminal_key(
                    1,
                    TerminalKeyEvent::new(
                        TerminalKey::Enter,
                        TerminalKeyAction::Press,
                        TerminalModifiers::default(),
                    ),
                )
                .unwrap();
        }
        loop {
            match next(&fleet) {
                FleetEvent::Boundary {
                    outcome: ComputerAdvanceOutcome::Halted(_),
                    tick: 1,
                    ..
                } => break,
                FleetEvent::Quiescent {
                    frame: Some(progress),
                    ..
                } => {
                    assert!(
                        progress.instructions_left > 0 && progress.advances_left > 0,
                        "canonical={canonical}: {progress:?}"
                    );
                }
                FleetEvent::Boundary {
                    outcome: ComputerAdvanceOutcome::WaitingForTerminalEvent,
                    ..
                } => {}
                other => panic!("canonical={canonical}: {other:?}"),
            }
        }
    }
}
