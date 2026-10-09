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
    verify_artifact, ArtifactLimits, CanonicalLineSubmissionError, ComputerError, ComputerMachine,
    ComputerTerminalEventKind, EntryArgumentLimits, ExecutableRevision, ExecutionProfile,
    FileSystemLimits, HostVerifyError, TerminalKey, TerminalKeyAction, TerminalKeyEvent,
    TerminalModifiers, VirtualPath,
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
fn deployment_reports_the_exact_installed_revision() {
    let bytes: Arc<[u8]> = Arc::from(terminal_artifact());
    let root = verify_artifact(Arc::clone(&bytes), ArtifactLimits::default()).unwrap();
    let mut computer = ComputerMachine::start(root, profile(), &[], &[]).unwrap();
    let limits = FileSystemLimits::default();
    let path = VirtualPath::parse_utf8("/home/demo", &limits).unwrap();
    let expected = computer.executable_revision(&path).unwrap();
    assert_eq!(ExecutableRevision::Absent, expected);
    let candidate = computer.verify_for_deploy(bytes).unwrap();

    let installed = computer.deploy(&path, expected, candidate).unwrap();

    assert!(matches!(installed, ExecutableRevision::Present(_)));
    assert_eq!(installed, computer.executable_revision(&path).unwrap());
}

#[test]
fn deployment_and_command_submission_are_separate_public_operations() {
    let bytes: Arc<[u8]> = Arc::from(terminal_artifact());
    let root = verify_artifact(Arc::clone(&bytes), ArtifactLimits::default()).unwrap();
    let mut computer = ComputerMachine::start(root, profile(), &[], &[]).unwrap();
    let path = VirtualPath::parse_utf8("/home/demo", &FileSystemLimits::default()).unwrap();
    let expected = computer.executable_revision(&path).unwrap();
    let candidate = computer.verify_for_deploy(bytes).unwrap();

    let installed = computer.deploy(&path, expected, candidate).unwrap();

    assert_eq!(installed, computer.executable_revision(&path).unwrap());
    assert_eq!(
        CanonicalLineSubmissionError::NoPendingRead,
        computer
            .submit_canonical_line(&"/home/demo".encode_utf16().collect::<Vec<_>>())
            .unwrap_err(),
    );
    assert_eq!(installed, computer.executable_revision(&path).unwrap());
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

#[test]
fn public_checkpoint_restores_without_starting_entry_or_consuming_input() {
    use compukter_vm::{
        ComputerAdvanceOutcome, ComputerCheckpointLimits, ComputerFileSystem, ComputerId,
        ComputerRestoreEnvironment, FileCapability, FileRights, ProcessLimits,
    };
    let id = ComputerId::from_bytes([4; 16]);
    let artifact =
        verify_artifact(Arc::from(terminal_artifact()), ArtifactLimits::default()).unwrap();
    let mut original = ComputerMachine::start(artifact, profile(), &[], &[]).unwrap();
    original.terminal_mut().push_text("preserve input").unwrap();
    let limits = ComputerCheckpointLimits::default();
    let bytes = original
        .checkpoint(id, b"paused host descriptors", limits)
        .unwrap();
    assert!(bytes.len() <= limits.maximum_encoded_bytes().unwrap());
    let filesystem_limits = FileSystemLimits::default();
    let environment = ComputerRestoreEnvironment {
        id,
        profile: profile(),
        process_limits: ProcessLimits::default(),
        addon_bindings: &[],
        filesystem: ComputerFileSystem::with_limits(filesystem_limits),
        initial_file_capability: FileCapability::new(
            VirtualPath::parse_utf8("/home", &filesystem_limits).unwrap(),
            FileRights::OWNER,
        ),
    };
    let (mut restored, host) =
        ComputerMachine::restore_checkpoint(environment, &bytes, limits).unwrap();
    assert_eq!(b"paused host descriptors", host.as_slice());
    assert_eq!(
        original.resource_snapshot().retired_instructions,
        restored.resource_snapshot().retired_instructions
    );
    assert_eq!(
        original.terminal().revision(),
        restored.terminal().revision()
    );
    assert_eq!(
        Some(ComputerTerminalEventKind::Text),
        restored.terminal_await_event().unwrap()
    );
    assert_eq!("preserve input", restored.terminal_event_text().unwrap());
    restored.terminal_finish_event().unwrap();
    for _ in 0..128 {
        let expected = original
            .advance_with_retirement_limit(64, 64, 64, 1)
            .unwrap();
        assert_eq!(
            expected,
            restored
                .advance_with_retirement_limit(64, 64, 64, 1)
                .unwrap()
        );
        assert_eq!(
            original.resource_snapshot().retired_instructions,
            restored.resource_snapshot().retired_instructions
        );
        if matches!(expected, ComputerAdvanceOutcome::Halted(_)) {
            return;
        }
    }
    panic!("restored program did not finish");
}

#[test]
fn checkpoint_continues_execution_in_a_fresh_process() {
    let root = std::env::temp_dir().join(format!(
        "compukters-checkpoint-process-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    let executable = std::env::current_exe().unwrap();
    for phase in ["save", "restore"] {
        let output = std::process::Command::new(&executable)
            .args([
                "--exact",
                "checkpoint_process_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("COMPUKTERS_CHECKPOINT_PROCESS_ROOT", &root)
            .env("COMPUKTERS_CHECKPOINT_PROCESS_PHASE", phase)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{phase}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "subprocess fixture"]
fn checkpoint_process_fixture() {
    if let Ok(expected_version) = std::env::var("COMPUKTERS_CHECKPOINT_PROCESS_VERSION") {
        assert_eq!(env!("CARGO_PKG_VERSION"), expected_version);
    }
    use compukter_vm::{
        ComputerAdvanceOutcome, ComputerCheckpointLimits, ComputerFileSystem, ComputerId,
        ComputerRestoreEnvironment, FileCapability, FileRights, ProcessLimits,
    };
    let root =
        std::path::PathBuf::from(std::env::var_os("COMPUKTERS_CHECKPOINT_PROCESS_ROOT").unwrap());
    let phase = std::env::var("COMPUKTERS_CHECKPOINT_PROCESS_PHASE").unwrap();
    let id = ComputerId::from_bytes([52; 16]);
    let limits = ComputerCheckpointLimits::default();
    if phase == "save" {
        let artifact = verify_artifact(
            Arc::from(support::executable_language_runtime_vector()),
            ArtifactLimits::default(),
        )
        .unwrap();
        let mut machine = ComputerMachine::start(artifact, profile(), &[], &[]).unwrap();
        machine
            .advance_with_retirement_limit(65536, 65536, 64, 6)
            .unwrap();
        assert_eq!(6, machine.resource_snapshot().retired_instructions);
        assert!(machine.resource_snapshot().heap_used_bytes > 0);
        machine
            .terminal_mut()
            .push_text("input from prior process")
            .unwrap();
        let bytes = machine
            .checkpoint(id, b"host from prior process", limits)
            .unwrap();
        std::fs::write(root.join("execution"), bytes).unwrap();
        let result = machine
            .advance_with_retirement_limit(65536, 65536, 64, 1000)
            .unwrap();
        assert!(matches!(result, ComputerAdvanceOutcome::Halted(_)));
        std::fs::write(
            root.join("expected"),
            format!(
                "{}:{result:?}",
                machine.resource_snapshot().retired_instructions
            ),
        )
        .unwrap();
    } else {
        assert_eq!("restore", phase);
        let fs_limits = FileSystemLimits::default();
        let environment = ComputerRestoreEnvironment {
            id,
            profile: profile(),
            process_limits: ProcessLimits::default(),
            addon_bindings: &[],
            filesystem: ComputerFileSystem::with_limits(fs_limits),
            initial_file_capability: FileCapability::new(
                VirtualPath::parse_utf8("/home", &fs_limits).unwrap(),
                FileRights::OWNER,
            ),
        };
        let (mut machine, host) = ComputerMachine::restore_checkpoint(
            environment,
            &std::fs::read(root.join("execution")).unwrap(),
            limits,
        )
        .unwrap();
        assert_eq!(b"host from prior process", host.as_slice());
        assert_eq!(6, machine.resource_snapshot().retired_instructions);
        assert!(machine.resource_snapshot().heap_used_bytes > 0);
        assert_eq!(
            Some(ComputerTerminalEventKind::Text),
            machine.terminal_await_event().unwrap()
        );
        assert_eq!(
            "input from prior process",
            machine.terminal_event_text().unwrap()
        );
        machine.terminal_finish_event().unwrap();
        let result = machine
            .advance_with_retirement_limit(65536, 65536, 64, 1000)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("expected")).unwrap(),
            format!(
                "{}:{result:?}",
                machine.resource_snapshot().retired_instructions
            )
        );
    }
}

#[test]
fn public_checkpoint_rejects_changed_identity_configuration_and_limits() {
    use compukter_vm::{
        ComputerCheckpointError, ComputerCheckpointLimits, ComputerFileSystem, ComputerId,
        ComputerRestoreEnvironment, FileCapability, FileRights, ProcessLimits,
    };
    let id = ComputerId::from_bytes([4; 16]);
    let artifact =
        verify_artifact(Arc::from(terminal_artifact()), ArtifactLimits::default()).unwrap();
    let original = ComputerMachine::start(artifact, profile(), &[], &[]).unwrap();
    let limits = ComputerCheckpointLimits::default();
    let bytes = original.checkpoint(id, b"", limits).unwrap();
    let environment = |id, profile| {
        let filesystem_limits = FileSystemLimits::default();
        ComputerRestoreEnvironment {
            id,
            profile,
            process_limits: ProcessLimits::default(),
            addon_bindings: &[],
            filesystem: ComputerFileSystem::with_limits(filesystem_limits),
            initial_file_capability: FileCapability::new(
                VirtualPath::parse_utf8("/home", &filesystem_limits).unwrap(),
                FileRights::OWNER,
            ),
        }
    };
    assert!(matches!(
        ComputerMachine::restore_checkpoint(
            environment(ComputerId::from_bytes([5; 16]), profile()),
            &bytes,
            limits
        ),
        Err(ComputerCheckpointError::Incompatible)
    ));
    let mut changed = profile();
    changed.maximum_events += 1;
    assert!(matches!(
        ComputerMachine::restore_checkpoint(environment(id, changed), &bytes, limits),
        Err(ComputerCheckpointError::Incompatible)
    ));
    assert!(matches!(
        ComputerMachine::restore_checkpoint(
            environment(id, profile()),
            &bytes,
            ComputerCheckpointLimits {
                maximum_decode_allocation_bytes: 0,
                ..limits
            }
        ),
        Err(ComputerCheckpointError::Limit)
    ));
    assert_eq!(
        Err(ComputerCheckpointError::Limit),
        ComputerCheckpointLimits {
            maximum_execution_bytes: usize::MAX,
            ..limits
        }
        .maximum_encoded_bytes()
    );
}

fn profile() -> ExecutionProfile {
    ExecutionProfile {
        heap_bytes: 1024 * 1024,
        frame_storage_bytes: 1024 * 1024,
        maximum_call_depth: 64,
        maximum_coroutines: 64,
        maximum_channels: 64,
        maximum_channel_values: 4096,
        maximum_host_requests: 64,
        maximum_events: 64,
        maximum_slice_budget: u32::MAX,
        compiler_abi: [0; 32],
        platform_abi: [0; 32],
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
