/*
 * The Compukters Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

#[allow(dead_code)]
mod support;

use std::sync::Arc;

use compukter_vm::{
    verify_artifact, ArtifactLimits, ComputerError, ComputerMachine, ComputerTerminalEventKind,
    ExecutionProfile, TerminalKey, TerminalKeyAction, TerminalKeyEvent, TerminalModifiers,
};

#[test]
fn computer_active_terminal_event_is_typed_fifo_and_lifetime_bounded() {
    let artifact =
        verify_artifact(Arc::from(terminal_artifact()), ArtifactLimits::default()).unwrap();
    let mut computer = ComputerMachine::start(artifact.clone(), profile(), &[], &[]).unwrap();
    let key = TerminalKeyEvent::new(
        TerminalKey::Enter,
        TerminalKeyAction::Press,
        TerminalModifiers::new(TerminalModifiers::CONTROL).unwrap(),
    );
    computer.terminal_mut().push_text("😀ab").unwrap();
    computer.terminal_mut().push_key(key).unwrap();

    assert_eq!(
        Some(ComputerTerminalEventKind::Text),
        computer.terminal_await_event().unwrap()
    );
    assert_eq!("😀ab", computer.terminal_event_text().unwrap());
    assert_eq!(
        ComputerError::WrongTerminalEventKind,
        computer.terminal_event_key().unwrap_err()
    );
    assert_eq!(
        ComputerError::ActiveTerminalEvent,
        computer.terminal_await_event().unwrap_err()
    );
    computer.terminal_finish_event().unwrap();

    assert_eq!(
        Some(ComputerTerminalEventKind::Key),
        computer.terminal_await_event().unwrap()
    );
    assert_eq!(
        TerminalKey::Enter.code(),
        computer.terminal_event_key().unwrap()
    );
    assert_eq!(1, computer.terminal_event_action().unwrap());
    assert_eq!(
        TerminalModifiers::CONTROL,
        computer.terminal_event_modifiers().unwrap()
    );
    computer.terminal_finish_event().unwrap();
    assert_eq!(None, computer.terminal_await_event().unwrap());
    assert_eq!(
        ComputerError::NoActiveTerminalEvent,
        computer.terminal_finish_event().unwrap_err()
    );

    let replacement = ComputerMachine::start(artifact, profile(), &[], &[]).unwrap();
    assert_eq!(
        ComputerError::NoActiveTerminalEvent,
        replacement.terminal_event_text().unwrap_err()
    );
}

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

fn terminal_artifact() -> Vec<u8> {
    support::executable_minimal_vector()
}
