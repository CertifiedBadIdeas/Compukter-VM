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

use std::sync::Arc;

use compukter_vm::{
    verify_artifact, ArtifactLimits, ComputerAdvanceOutcome, ComputerMachine, ComputerValue,
    ExecutionProfile, TerminalCell,
};

#[test]
fn computer_owns_terminal_waiting_input_and_halted_screen_for_its_lifetime() {
    let artifact =
        verify_artifact(Arc::from(terminal_artifact()), ArtifactLimits::default()).unwrap();
    let mut computer = ComputerMachine::start(artifact.clone(), profile(), &[], &[]).unwrap();

    for _ in 0..1_024 {
        if matches!(
            computer.advance(64, 64).unwrap(),
            ComputerAdvanceOutcome::WaitingForLine
        ) {
            break;
        }
    }
    assert!(matches!(
        computer.advance(64, 64).unwrap(),
        ComputerAdvanceOutcome::WaitingForLine
    ));
    assert_eq!("> 😀> 😀\n", terminal_text(&computer));

    computer
        .provide_compatibility_line(&"answer".encode_utf16().collect::<Vec<_>>())
        .unwrap();
    let halted = (0..1_024).find_map(|_| match computer.advance(64, 64).unwrap() {
        ComputerAdvanceOutcome::Halted(value) => Some(value),
        ComputerAdvanceOutcome::SliceExhausted => None,
        other => panic!("unexpected machine outcome: {other:?}"),
    });
    assert!(matches!(halted, Some(Some(ComputerValue::I32(_)))));
    assert_eq!("> 😀> 😀\n", terminal_text(&computer));

    let replacement = ComputerMachine::start(artifact, profile(), &[], &[]).unwrap();
    assert_eq!(
        TerminalCell::default(),
        replacement.terminal().cell(0, 0).unwrap()
    );
}

fn terminal_text(computer: &ComputerMachine) -> String {
    let mut text = String::new();
    for y in 0..19 {
        for x in 0..51 {
            let scalar =
                char::from_u32(computer.terminal().cell(x, y).unwrap().code_point()).unwrap();
            text.push(scalar);
        }
        text.push('\n');
    }
    text.trim_end_matches([' ', '\n']).to_owned() + "\n"
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
    include_str!("fixtures/terminal-session.hex")
        .trim()
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |value| match value {
                b'0'..=b'9' => value - b'0',
                b'a'..=b'f' => value - b'a' + 10,
                _ => panic!("invalid fixture hex"),
            };
            (digit(pair[0]) << 4) | digit(pair[1])
        })
        .collect()
}
