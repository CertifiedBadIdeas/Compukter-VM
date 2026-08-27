/*
 * The Compukters Developers
 *
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#[allow(dead_code)]
mod support;

use std::sync::Arc;

use compukter_vm::{
    verify_artifact, ArtifactLimits, ComputerError, ComputerMachine, ComputerTerminalEventKind,
    EntryArgumentLimits, ExecutableRevision, ExecutionProfile, HostVerifyError, TerminalKey,
    TerminalKeyAction, TerminalKeyEvent, TerminalModifiers,
};

#[test]
fn executable_revision_distinguishes_absent_and_present_files() {
    assert_ne!(ExecutableRevision::Absent, ExecutableRevision::Present(1));
    assert_eq!(ExecutableRevision::Present(7).generation(), Some(7));
    assert_eq!(ExecutableRevision::Absent.generation(), None);
}

#[test]
fn deployment_verification_rejects_malformed_artifacts_without_mutation() {
    let root = verify_artifact(Arc::from(terminal_artifact()), ArtifactLimits::default()).unwrap();
    let computer = ComputerMachine::start(root, profile(), &[], &[]).unwrap();
    let filesystem_generation = computer.filesystem_generation();
    let terminal_revision = computer.terminal().revision();

    assert!(matches!(
        computer.verify_for_deploy(Arc::from([0xff_u8].as_slice())),
        Err(HostVerifyError::Artifact(_)),
    ));
    assert_eq!(filesystem_generation, computer.filesystem_generation());
    assert_eq!(terminal_revision, computer.terminal().revision());
}

#[test]
fn deployment_verification_accepts_a_compatible_artifact() {
    let bytes: Arc<[u8]> = Arc::from(terminal_artifact());
    let root = verify_artifact(Arc::clone(&bytes), ArtifactLimits::default()).unwrap();
    let computer = ComputerMachine::start(root, profile(), &[], &[]).unwrap();

    let _candidate = computer.verify_for_deploy(bytes).unwrap();
}

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
        entry_argument_limits: EntryArgumentLimits {
            maximum_count: 64,
            maximum_code_units_per_argument: 4096,
            maximum_total_code_units: 16_384,
        },
    }
}

fn terminal_artifact() -> Vec<u8> {
    support::executable_minimal_vector()
}
