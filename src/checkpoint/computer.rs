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

use super::*;
use crate::execution::checkpoint::{
    checkpoint_struct, Checkpoint, CheckpointError, Reader, Result, Writer,
};
use crate::filesystem::ComputerId;
#[path = "envelope.rs"]
mod envelope;

checkpoint_struct!(RetiredExecutionAccounting {
    fixed_guest_units,
    dynamic_guest_units,
    maintenance_units,
    entered_blocks,
    executed_instructions,
    retired_instructions,
    saturated
});
checkpoint_struct!(ExternalRequestRoute {
    scope_id,
    task_id,
    internal_id,
    external_id
});
checkpoint_struct!(CompilationTransaction {
    token,
    task,
    request,
    owner_depth,
    source,
    source_revision,
    output,
    output_revision
});

pub(crate) struct ComputerCheckpointContext {
    pub id: ComputerId,
    pub profile: ExecutionProfile,
    pub process_limits: ProcessLimits,
    pub addon_bindings: Box<[OwnedCapabilityBinding]>,
    pub filesystem: ComputerFileSystem,
    pub initial_file_capability: FileCapability,
}

impl ComputerMachine {
    pub(crate) fn write_checkpoint_envelope(
        &self,
        id: ComputerId,
        host: &[u8],
        limits: envelope::Limits,
    ) -> Result<Vec<u8>> {
        let mut writer = Writer::new(limits.execution_bytes);
        self.write_checkpoint_state(id, &mut writer)?;
        envelope::encode(
            id,
            self.filesystem.generation(),
            &writer.finish(),
            host,
            limits,
        )
    }

    pub(crate) fn read_checkpoint_envelope(
        context: ComputerCheckpointContext,
        bytes: &[u8],
        limits: envelope::Limits,
    ) -> Result<(Self, Vec<u8>)> {
        let decoded = envelope::decode(bytes, context.id, limits)?;
        let allocation = limits
            .allocation_bytes
            .checked_sub(decoded.host.len())
            .ok_or(CheckpointError::Limit)?;
        let mut reader = Reader::new(decoded.execution, limits.execution_bytes, allocation)?;
        let machine = Self::read_checkpoint_state(context, &mut reader)?;
        reader.finish()?;
        if machine.filesystem.generation() != decoded.generation {
            return Err(CheckpointError::InvalidState);
        }
        let mut host = Vec::new();
        host.try_reserve_exact(decoded.host.len())
            .map_err(|_| CheckpointError::Allocation)?;
        host.extend_from_slice(decoded.host);
        Ok((machine, host))
    }

    fn checkpoint_binding_identity(bindings: &[OwnedCapabilityBinding]) -> Result<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let mut writer = Writer::new(16 * 1024 * 1024);
        bindings.len().write(&mut writer)?;
        for binding in bindings {
            binding.write(&mut writer)?;
        }
        Ok(Sha256::digest(writer.finish()).into())
    }

    pub(crate) fn write_checkpoint_state(&self, id: ComputerId, writer: &mut Writer) -> Result<()> {
        self.root_artifact.write(writer)?;
        self.profile.write(writer)?;
        self.process_limits.write(writer)?;
        Self::checkpoint_binding_identity(&self.addon_bindings)?.write(writer)?;
        self.initial_file_capability.write(writer)?;
        self.inspection_file_capability.write(writer)?;
        self.sessions.len().write(writer)?;
        for frame in &self.sessions {
            frame.scope_id.write(writer)?;
            frame.executable.is_some().write(writer)?;
            if let Some((path, artifact)) = &frame.executable {
                path.write(writer)?;
                artifact.write(writer)?;
            }
            frame.session.write_checkpoint_state(writer)?;
            frame.process_diagnostics.write(writer)?;
            frame.compiler_diagnostics.write(writer)?;
            frame.pending_terminal_event.write(writer)?;
            frame.pending_stdio_read.write(writer)?;
            frame.pending_process.write(writer)?;
        }
        self.terminal.write(writer)?;
        self.standard_streams.write(writer)?;
        self.redstone.write(writer)?;
        self.active_terminal_event.write(writer)?;
        self.active_terminal_event_owner.write(writer)?;
        self.filesystem.write_checkpoint_state(id, writer)?;
        self.process_starts.write(writer)?;
        self.retired_execution.write(writer)?;
        self.reserved_heap_bytes.write(writer)?;
        self.reserved_frame_storage_bytes.write(writer)?;
        self.maximum_text_code_units.write(writer)?;
        self.pending_compilation.write(writer)?;
        self.pending_termination.write(writer)?;
        self.next_compilation_token.write(writer)?;
        self.next_external_request.write(writer)?;
        self.external_requests.write(writer)?;
        Ok(())
    }

    /// Internal composition; the disk admission boundary must additionally
    /// verify the envelope and host resource descriptions before publication.
    pub(crate) fn read_checkpoint_state(
        context: ComputerCheckpointContext,
        reader: &mut Reader<'_>,
    ) -> Result<Self> {
        let root_artifact = VerifiedArtifact::read(reader)?;
        let profile = ExecutionProfile::read(reader)?;
        let process_limits = ProcessLimits::read(reader)?;
        let bindings: [u8; 32] = Checkpoint::read(reader)?;
        let initial_file_capability = FileCapability::read(reader)?;
        let inspection_file_capability = FileCapability::read(reader)?;
        let inspection = FileCapability::new(
            VirtualPath::root(),
            FileRights::INSPECT | FileRights::LIST | FileRights::READ,
        );
        if profile != context.profile
            || process_limits != context.process_limits
            || bindings != Self::checkpoint_binding_identity(&context.addon_bindings)?
            || initial_file_capability != context.initial_file_capability
            || inspection_file_capability != inspection
        {
            return Err(CheckpointError::Incompatible);
        }
        let count = usize::read(reader)?;
        if count == 0 || count > process_limits.maximum_depth as usize {
            return Err(CheckpointError::Limit);
        }
        reader.allocate::<ProcessFrame>(count)?;
        let mut sessions = Vec::new();
        sessions
            .try_reserve_exact(count)
            .map_err(|_| CheckpointError::Allocation)?;
        for index in 0..count {
            let scope_id = u64::read(reader)?;
            let executable = if bool::read(reader)? {
                Some((VirtualPath::read(reader)?, VerifiedArtifact::read(reader)?))
            } else {
                None
            };
            if (index == 0) != executable.is_none() || (index == 0) != (scope_id == 0) {
                return Err(CheckpointError::InvalidState);
            }
            let artifact = executable
                .as_ref()
                .map_or(&root_artifact, |(_, artifact)| artifact);
            let admitted =
                admit_session(artifact.clone(), profile.clone(), &context.addon_bindings)
                    .map_err(|_| CheckpointError::Incompatible)?;
            let session = Session::read_checkpoint_state(admitted, reader)?;
            sessions.push(ProcessFrame {
                scope_id,
                session,
                executable,
                process_diagnostics: Checkpoint::read(reader)?,
                compiler_diagnostics: Checkpoint::read(reader)?,
                pending_terminal_event: Checkpoint::read(reader)?,
                pending_stdio_read: Checkpoint::read(reader)?,
                pending_process: Checkpoint::read(reader)?,
            });
        }
        let terminal = TerminalDevice::read(reader)?;
        terminal.validate_checkpoint(&TerminalDevice::default())?;
        let standard_streams = StandardStreams::read(reader)?;
        standard_streams.validate_checkpoint(
            &StandardStreams::new(
                profile.maximum_inbound_utf16_code_units as usize,
                profile.maximum_outbound_utf16_code_units as usize,
            )
            .map_err(|_| CheckpointError::Incompatible)?,
        )?;
        let redstone = RedstoneDevice::read(reader)?;
        redstone
            .validate_checkpoint(&RedstoneDevice::new(profile.maximum_host_requests as usize))?;
        let active_terminal_event = Checkpoint::read(reader)?;
        let active_terminal_event_owner = Checkpoint::read(reader)?;
        let filesystem =
            ComputerFileSystem::read_checkpoint_state(context.filesystem, context.id, reader)?;
        let computer = Self {
            root_artifact,
            machine_identity: Arc::new(()),
            sessions,
            terminal,
            standard_streams,
            redstone,
            active_terminal_event,
            active_terminal_event_owner,
            filesystem,
            initial_file_capability,
            inspection_file_capability,
            profile,
            addon_bindings: context.addon_bindings,
            process_limits,
            process_starts: Checkpoint::read(reader)?,
            retired_execution: Checkpoint::read(reader)?,
            reserved_heap_bytes: Checkpoint::read(reader)?,
            reserved_frame_storage_bytes: Checkpoint::read(reader)?,
            maximum_text_code_units: Checkpoint::read(reader)?,
            pending_compilation: Checkpoint::read(reader)?,
            pending_termination: Checkpoint::read(reader)?,
            next_compilation_token: Checkpoint::read(reader)?,
            next_external_request: Checkpoint::read(reader)?,
            external_requests: Checkpoint::read(reader)?,
        };
        computer.validate_checkpoint_state()?;
        Ok(computer)
    }

    fn validate_checkpoint_state(&self) -> Result<()> {
        let invalid = || CheckpointError::InvalidState;
        let count = self.sessions.len() as u64;
        if self.process_starts == 0
            || self.process_starts > self.process_limits.maximum_starts
            || self.process_starts > i64::MAX as u64
            || count.checked_mul(u64::from(self.profile.heap_bytes))
                != Some(self.reserved_heap_bytes)
            || count.checked_mul(self.profile.frame_storage_bytes)
                != Some(self.reserved_frame_storage_bytes)
            || self.reserved_heap_bytes > self.process_limits.maximum_aggregate_heap_bytes
            || self.reserved_frame_storage_bytes
                > self.process_limits.maximum_aggregate_frame_storage_bytes
            || self.maximum_text_code_units
                != self.profile.maximum_inbound_utf16_code_units as usize
            || self.next_compilation_token == 0
            || self.next_external_request == 0
            || self.next_external_request > i64::MAX as u64
            || self.active_terminal_event.is_some() != self.active_terminal_event_owner.is_some()
            || self.active_terminal_event_owner.is_some_and(|owner| {
                owner.frame() == 0 || owner.frame() > self.sessions.len() || owner.task().get() == 0
            })
        {
            return Err(invalid());
        }
        let mut scopes = std::collections::BTreeSet::new();
        for frame in &self.sessions {
            if frame.scope_id >= self.process_starts
                || !scopes.insert(frame.scope_id)
                || frame.compiler_diagnostics.len()
                    > self.process_limits.maximum_diagnostic_utf16_code_units
                || frame.process_diagnostics.len() > self.profile.maximum_coroutines as usize
                || frame.process_diagnostics.iter().any(|(task, text)| {
                    task.get() == 0
                        || text.len() > self.process_limits.maximum_diagnostic_utf16_code_units
                })
                || frame.executable.as_ref().is_some_and(|(path, _)| {
                    VirtualPath::parse_utf8(&path.to_string(), self.filesystem.limits()).is_err()
                })
            {
                return Err(invalid());
            }
            for (task, request) in [
                frame.pending_terminal_event,
                frame.pending_stdio_read,
                frame.pending_process,
            ]
            .into_iter()
            .flatten()
            {
                if !frame.session.checkpoint_has_host_request(task, request) {
                    return Err(invalid());
                }
            }
        }
        let maximum = (self.profile.maximum_host_requests as usize)
            .checked_mul(self.process_limits.maximum_depth as usize)
            .ok_or_else(invalid)?;
        if self.external_requests.len() > maximum {
            return Err(invalid());
        }
        let mut external = std::collections::BTreeSet::new();
        let mut internal = std::collections::BTreeSet::new();
        for route in &self.external_requests {
            let frame = self
                .sessions
                .iter()
                .find(|frame| frame.scope_id == route.scope_id)
                .ok_or_else(invalid)?;
            let task = TaskId::new(route.task_id).ok_or_else(invalid)?;
            let request = RequestId::new(route.internal_id).ok_or_else(invalid)?;
            if route.external_id == 0
                || route.external_id >= self.next_external_request
                || !external.insert(route.external_id)
                || !internal.insert((route.scope_id, route.task_id, route.internal_id))
                || !frame.session.checkpoint_has_host_request(task, request)
            {
                return Err(invalid());
            }
        }
        if self
            .pending_termination
            .is_some_and(|scope| !scopes.contains(&scope))
        {
            return Err(invalid());
        }
        if let Some(compilation) = &self.pending_compilation {
            let frame = self
                .sessions
                .get(compilation.owner_depth.checked_sub(1).ok_or_else(invalid)?)
                .ok_or_else(invalid)?;
            if compilation.token == 0
                || compilation.token >= self.next_compilation_token
                || !frame
                    .session
                    .checkpoint_has_host_request(compilation.task, compilation.request)
                || VirtualPath::parse_utf8(
                    &compilation.source.to_string(),
                    self.filesystem.limits(),
                )
                .is_err()
                || VirtualPath::parse_utf8(
                    &compilation.output.to_string(),
                    self.filesystem.limits(),
                )
                .is_err()
                || compilation.source_revision > self.filesystem.generation()
                || compilation
                    .output_revision
                    .generation()
                    .is_some_and(|generation| generation > self.filesystem.generation())
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer::tests::profile;
    use crate::execution::fixtures;
    const ID: ComputerId = ComputerId::from_bytes([9; 16]);

    fn round_trip(original: &ComputerMachine) -> ComputerMachine {
        let mut writer = Writer::new(32 * 1024 * 1024);
        original.write_checkpoint_state(ID, &mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 32 * 1024 * 1024, 64 * 1024 * 1024).unwrap();
        let context = ComputerCheckpointContext {
            id: ID,
            profile: original.profile.clone(),
            process_limits: original.process_limits,
            addon_bindings: original.addon_bindings.clone(),
            filesystem: ComputerFileSystem::with_limits(*original.filesystem.limits()),
            initial_file_capability: original.initial_file_capability.clone(),
        };
        let restored = ComputerMachine::read_checkpoint_state(context, &mut reader).unwrap();
        reader.finish().unwrap();
        restored
    }

    fn assert_same_state(first: &ComputerMachine, second: &ComputerMachine) {
        let mut first_bytes = Writer::new(32 * 1024 * 1024);
        let mut second_bytes = Writer::new(32 * 1024 * 1024);
        first.write_checkpoint_state(ID, &mut first_bytes).unwrap();
        second
            .write_checkpoint_state(ID, &mut second_bytes)
            .unwrap();
        assert_eq!(first_bytes.finish(), second_bytes.finish());
        // Spare vector capacity may shrink when rebuilding logical state.
        // Resident storage measures that physical allocation, not guest quota.
        let mut first_resources = first.resource_snapshot();
        let mut second_resources = second.resource_snapshot();
        first_resources.mutable_execution_resident_bytes = 0;
        second_resources.mutable_execution_resident_bytes = 0;
        assert_eq!(first_resources, second_resources);
    }

    #[test]
    fn checkpoint_envelope_restores_computer_and_binds_filesystem_generation() {
        use sha2::{Digest, Sha256};
        let mut original =
            ComputerMachine::start(fixtures::nested_call_artifact(), profile(), &[], &[]).unwrap();
        original
            .advance_with_retirement_limit(32, 1, 64, 1)
            .unwrap();
        let limits = envelope::Limits {
            execution_bytes: 32 * 1024 * 1024,
            host_bytes: 4096,
            allocation_bytes: 64 * 1024 * 1024,
        };
        let context = || ComputerCheckpointContext {
            id: ID,
            profile: original.profile.clone(),
            process_limits: original.process_limits,
            addon_bindings: original.addon_bindings.clone(),
            filesystem: ComputerFileSystem::with_limits(*original.filesystem.limits()),
            initial_file_capability: original.initial_file_capability.clone(),
        };
        let bytes = original
            .write_checkpoint_envelope(ID, b"host resource descriptors", limits)
            .unwrap();
        let (restored, host) =
            ComputerMachine::read_checkpoint_envelope(context(), &bytes, limits).unwrap();
        assert_eq!(b"host resource descriptors", host.as_slice());
        assert_same_state(&original, &restored);
        let mut inconsistent = bytes.clone();
        inconsistent[60..68].copy_from_slice(&17_u64.to_le_bytes());
        let end = inconsistent.len() - 32;
        let digest = Sha256::digest(&inconsistent[..end]);
        inconsistent[end..].copy_from_slice(&digest);
        assert!(matches!(
            ComputerMachine::read_checkpoint_envelope(context(), &inconsistent, limits),
            Err(CheckpointError::InvalidState)
        ));
        assert!(matches!(
            ComputerMachine::read_checkpoint_envelope(
                context(),
                &bytes,
                envelope::Limits {
                    allocation_bytes: 0,
                    ..limits
                }
            ),
            Err(CheckpointError::Limit)
        ));
    }

    #[test]
    fn checkpoint_store_reopens_complete_computer_and_discards_consumed_snapshot() {
        use crate::{RomImage, StoreError, WorldFileSystemStore};
        use sha2::{Digest, Sha256};
        let filesystem_limits = FileSystemLimits::testing();
        let mut rom = b"CPKTROM\0".to_vec();
        rom.extend_from_slice(&1_u16.to_le_bytes());
        rom.extend_from_slice(&0_u16.to_le_bytes());
        rom.extend_from_slice(&0_u32.to_le_bytes());
        rom.extend_from_slice(&Sha256::digest(&rom));
        let rom = Arc::new(RomImage::admit(rom.into(), &filesystem_limits).unwrap());
        let root = std::env::temp_dir().join(format!(
            "compukters-computer-checkpoint-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = WorldFileSystemStore::open(&root, filesystem_limits).unwrap();
        let owner = FileCapability::new(
            VirtualPath::parse_utf8("/home", &filesystem_limits).unwrap(),
            FileRights::OWNER,
        );
        let mut filesystem = store.open_computer(ID, Arc::clone(&rom)).unwrap();
        let file = VirtualPath::parse_utf8("/home/data", &filesystem_limits).unwrap();
        filesystem
            .write_file(&owner, &file, b"before hibernation", false)
            .unwrap();
        let mut original = ComputerMachine::start_in_filesystem(
            fixtures::nested_call_artifact(),
            profile(),
            &[],
            &[],
            filesystem,
            owner.clone(),
        )
        .unwrap();
        original
            .advance_with_retirement_limit(32, 1, 64, 1)
            .unwrap();
        original.terminal_mut().push_text("queued input").unwrap();
        let limits = envelope::Limits {
            execution_bytes: 32 * 1024 * 1024,
            host_bytes: 4096,
            allocation_bytes: 64 * 1024 * 1024,
        };
        let generation = original.filesystem_generation();
        let bytes = original
            .write_checkpoint_envelope(ID, b"paused timers", limits)
            .unwrap();
        assert_eq!(
            Err(StoreError::Busy),
            store.save_execution_checkpoint(ID, generation, &bytes, 1)
        );
        store
            .save_execution_checkpoint(ID, generation, &bytes, bytes.len())
            .unwrap();
        assert_eq!(
            Err(StoreError::StorageFaulted),
            store.read_execution_checkpoint(ID, 1)
        );
        store.close().unwrap();
        drop(store);
        let store = WorldFileSystemStore::open(&root, filesystem_limits).unwrap();
        let filesystem = store.open_computer(ID, Arc::clone(&rom)).unwrap();
        let persisted = store
            .read_execution_checkpoint(ID, bytes.len())
            .unwrap()
            .unwrap();
        let (mut restored, host) = ComputerMachine::read_checkpoint_envelope(
            ComputerCheckpointContext {
                id: ID,
                profile: profile(),
                process_limits: original.process_limits,
                addon_bindings: original.addon_bindings.clone(),
                filesystem,
                initial_file_capability: owner,
            },
            &persisted,
            limits,
        )
        .unwrap();
        assert_eq!(b"paused timers", host.as_slice());
        assert_same_state(&original, &restored);
        store.discard_execution_checkpoint(ID).unwrap();
        store.discard_execution_checkpoint(ID).unwrap();
        assert!(store
            .read_execution_checkpoint(ID, bytes.len())
            .unwrap()
            .is_none());
        for _ in 0..64 {
            let expected = original
                .advance_with_retirement_limit(32, 1, 64, 1)
                .unwrap();
            assert_eq!(
                expected,
                restored
                    .advance_with_retirement_limit(32, 1, 64, 1)
                    .unwrap()
            );
            assert_same_state(&original, &restored);
            if matches!(expected, ComputerAdvanceOutcome::Halted(_)) {
                break;
            }
        }
        assert_eq!(
            b"before hibernation",
            restored
                .filesystem
                .read_file_for_test(&file)
                .unwrap()
                .as_slice()
        );
        store.close().unwrap();
        assert_eq!(
            Err(StoreError::Closed),
            store.read_execution_checkpoint(ID, bytes.len())
        );
        drop(restored);
        drop(original);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn checkpoint_resumes_complete_computer_at_each_boundary_and_preserves_halted_state() {
        let mut original =
            ComputerMachine::start(fixtures::nested_call_artifact(), profile(), &[], &[]).unwrap();
        original.terminal_mut().write_utf16(&[0x41, 0x42]).unwrap();
        original.terminal_mut().push_text("queued input").unwrap();
        for _ in 0..64 {
            let mut restored = round_trip(&original);
            assert_same_state(&original, &restored);
            let expected = original
                .advance_with_retirement_limit(32, 1, 64, 1)
                .unwrap();
            let actual = restored
                .advance_with_retirement_limit(32, 1, 64, 1)
                .unwrap();
            assert_eq!(expected, actual);
            assert_same_state(&original, &restored);
            if matches!(expected, ComputerAdvanceOutcome::Halted(_)) {
                let mut restored = round_trip(&original);
                assert_eq!(expected, restored.advance(32, 1, 64).unwrap());
                assert_same_state(&original, &restored);
                return;
            }
        }
        panic!("computer did not complete");
    }

    #[test]
    fn checkpoint_keeps_external_request_routes_and_rejects_duplicate_completion() {
        let operations = [
            OperationSchema::asynchronous(&[], HostValueType::I32),
            OperationSchema::asynchronous(&[], HostValueType::Unit),
        ];
        let bindings = [CapabilityBinding::new("app", "entry", 1, 0, &operations)];
        let mut original = ComputerMachine::start(
            fixtures::two_task_host_artifact(),
            profile(),
            &bindings,
            &[],
        )
        .unwrap();
        let batch = match original.advance(64, 1, 64).unwrap() {
            ComputerAdvanceOutcome::HostRequestBatch(batch) => batch,
            other => panic!("unexpected outcome {other:?}"),
        };
        let mut restored = round_trip(&original);
        assert_eq!(
            ComputerAdvanceOutcome::HostRequestBatch(batch.clone()),
            restored.advance(64, 1, 64).unwrap()
        );
        for request in batch.requests.iter().rev() {
            for computer in [&mut original, &mut restored] {
                let value = if request.operation == 0 {
                    HostValueInput::I32(13)
                } else {
                    HostValueInput::Unit
                };
                computer
                    .resume_host_request_for(
                        request.task_id,
                        request.id,
                        HostResponse::Success(value),
                    )
                    .unwrap();
                assert_eq!(
                    Err(ComputerError::InvalidRequestId),
                    computer.resume_host_request_for(
                        request.task_id,
                        request.id,
                        HostResponse::Success(value)
                    )
                );
            }
            assert_same_state(&original, &restored);
        }
        assert_eq!(
            ComputerAdvanceOutcome::Halted(None),
            restored.advance(64, 1, 64).unwrap()
        );
        assert_eq!(
            ComputerAdvanceOutcome::Halted(None),
            original.advance(64, 1, 64).unwrap()
        );
        assert_same_state(&original, &restored);
    }

    #[test]
    fn checkpoint_preserves_partial_canonical_line_and_pending_read() {
        let mut original = ComputerMachine::start(
            fixtures::stdio_conformance_artifact(&[]),
            profile(),
            &[],
            &[],
        )
        .unwrap();
        for _ in 0..128 {
            if original.advance(64, 64, u32::MAX).unwrap()
                == ComputerAdvanceOutcome::WaitingForTerminalEvent
            {
                break;
            }
        }
        assert!(original.active_frame().pending_stdio_read.is_some());
        original.terminal_mut().push_text("partial").unwrap();
        original.advance(64, 64, u32::MAX).unwrap();
        let mut restored = round_trip(&original);
        assert_same_state(&original, &restored);
        for computer in [&mut original, &mut restored] {
            computer
                .terminal_mut()
                .push_key(TerminalKeyEvent::new(
                    TerminalKey::Backspace,
                    TerminalKeyAction::Press,
                    TerminalModifiers::default(),
                ))
                .unwrap();
            computer.terminal_mut().push_text("!").unwrap();
            computer
                .terminal_mut()
                .push_key(TerminalKeyEvent::new(
                    TerminalKey::Enter,
                    TerminalKeyAction::Press,
                    TerminalModifiers::default(),
                ))
                .unwrap();
        }
        for _ in 0..128 {
            let expected = original.advance(64, 64, u32::MAX).unwrap();
            assert_eq!(expected, restored.advance(64, 64, u32::MAX).unwrap());
            assert_same_state(&original, &restored);
            if matches!(expected, ComputerAdvanceOutcome::Halted(_)) {
                return;
            }
        }
        panic!("canonical read did not complete");
    }

    #[test]
    fn checkpoint_preserves_pending_compilation_and_publishes_only_once() {
        use crate::computer::tests::{compiler_computer, next_compilation_request};
        let (mut original, owner, _, output) = compiler_computer(b"fun main() = 42", None);
        let request = next_compilation_request(&mut original);
        let mut restored = round_trip(&original);
        assert_same_state(&original, &restored);
        let compiled = fixtures::two_block_artifact(1, 1);
        for computer in [&mut original, &mut restored] {
            computer
                .complete_compilation_success(request.token, &compiled.decoded().bytes)
                .unwrap();
            assert_eq!(
                Err(ComputerError::NoActiveCompilation),
                computer.complete_compilation_success(request.token, &compiled.decoded().bytes)
            );
            assert!(
                computer
                    .filesystem
                    .stat(&owner, &output)
                    .unwrap()
                    .executable
            );
        }
        assert_same_state(&original, &restored);
        for _ in 0..128 {
            let expected = original.advance(64, 64, u32::MAX).unwrap();
            assert_eq!(expected, restored.advance(64, 64, u32::MAX).unwrap());
            assert_same_state(&original, &restored);
            if matches!(expected, ComputerAdvanceOutcome::Halted(_)) {
                return;
            }
        }
        panic!("compilation did not complete");
    }

    #[test]
    fn checkpoint_pins_running_child_image_after_executable_replacement() {
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(
            VirtualPath::parse_utf8("/home", &limits).unwrap(),
            FileRights::OWNER,
        );
        let path = VirtualPath::parse_utf8("/home/child", &limits).unwrap();
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        let child = fixtures::two_block_artifact(1, 1);
        filesystem
            .write_file(&owner, &path, &child.decoded().bytes, true)
            .unwrap();
        let parent = fixtures::process_v2_run_artifact(
            &"/home/child".encode_utf16().collect::<Vec<_>>(),
            &[0, 0],
        );
        let mut original = ComputerMachine::start_in_filesystem(
            parent,
            profile(),
            &[],
            &[],
            filesystem,
            owner.clone(),
        )
        .unwrap();
        loop {
            match original.advance(64, 1, 64).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::ProcessEntered(_) => break,
                other => panic!("unexpected outcome {other:?}"),
            }
        }
        original
            .filesystem
            .write_file(&owner, &path, b"replacement", false)
            .unwrap();
        for _ in 0..128 {
            let mut restored = round_trip(&original);
            let expected = original
                .advance_with_retirement_limit(64, 1, 64, 1)
                .unwrap();
            let actual = restored
                .advance_with_retirement_limit(64, 1, 64, 1)
                .unwrap();
            assert_eq!(expected, actual);
            assert_same_state(&original, &restored);
            if matches!(expected, ComputerAdvanceOutcome::Halted(_)) {
                assert_eq!(
                    ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(0))),
                    expected
                );
                assert_eq!(
                    b"replacement",
                    restored
                        .filesystem
                        .read_file_for_test(&path)
                        .unwrap()
                        .as_slice()
                );
                return;
            }
        }
        panic!("child did not return to its parent");
    }
}
