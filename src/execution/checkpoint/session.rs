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
    checkpoint_enum, checkpoint_struct, Checkpoint, CheckpointError, Reader, Result, Writer,
};

checkpoint_struct!(PreparingRequest {
    id,
    task,
    capability,
    operation,
    argument,
    string_offset
});

checkpoint_struct!(QueuedReply {
    task,
    request,
    value
});

checkpoint_enum!(CopiedReply {
    0 => Scalar(v0);
    1 => String(v0);
    2 => Record(v0);
    3 => Failure(v0);
});

checkpoint_enum!(SessionTerminal {
    0 => HostFailed(v0);
    1 => Faulted(v0);
    2 => QuotaExhausted(v0);
});

// Internal state transport only. Persisted admission additionally needs complete
// image-aware reference/continuation and external resource validation.
impl Session {
    pub(crate) fn checkpoint_has_host_request(&self, task: TaskId, request: RequestId) -> bool {
        self.pending_requests
            .get(HostRequestIdentity::new(task, request))
            .is_some()
            && self.machine.checkpoint_task_waits_for(task, request)
    }

    fn checkpoint_capability_identity(&self) -> Result<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let mut writer = Writer::new(16 * 1024 * 1024);
        self.capabilities.write(&mut writer)?;
        Ok(Sha256::digest(writer.finish()).into())
    }

    pub(crate) fn write_checkpoint_state(&self, writer: &mut Writer) -> Result<()> {
        self.checkpoint_capability_identity()?.write(writer)?;
        self.maximum_slice_budget.write(writer)?;
        self.maximum_replies.write(writer)?;
        self.entry_argument_limits.write(writer)?;
        self.machine.write_checkpoint_state(writer)?;
        self.entry_arguments.write(writer)?;
        self.outbound_utf16.write(writer)?;
        self.inbound_utf16.write(writer)?;
        self.failure_detail.write(writer)?;
        self.failure_detail_length.write(writer)?;
        self.argument_slots.write(writer)?;
        self.argument_count.write(writer)?;
        self.pending_requests.write(writer)?;
        self.terminal.write(writer)?;
        self.next_request_id.write(writer)?;
        self.preparing_request.write(writer)?;
        self.inbound_length.write(writer)?;
        self.resuming_host_string.write(writer)?;
        self.replies.write(writer)?;
        self.published_requests.write(writer)?;
        self.accepted_responses.write(writer)?;
        Ok(())
    }

    pub(crate) fn read_checkpoint_state(
        mut admitted: Self,
        reader: &mut Reader<'_>,
    ) -> Result<Self> {
        let identity: [u8; 32] = Checkpoint::read(reader)?;
        let maximum_slice_budget: u32 = Checkpoint::read(reader)?;
        let maximum_replies: usize = Checkpoint::read(reader)?;
        let entry_argument_limits: EntryArgumentLimits = Checkpoint::read(reader)?;
        if identity != admitted.checkpoint_capability_identity()?
            || maximum_slice_budget != admitted.maximum_slice_budget
            || maximum_replies != admitted.maximum_replies
            || entry_argument_limits != admitted.entry_argument_limits
        {
            return Err(CheckpointError::Incompatible);
        }
        let entry_capacity = admitted.entry_arguments.len();
        let outbound_capacity = admitted.outbound_utf16.len();
        let inbound_capacity = admitted.inbound_utf16.len();
        let argument_capacity = admitted.argument_slots.len();
        admitted.machine.restore_checkpoint_state(reader)?;
        admitted.entry_arguments = Checkpoint::read(reader)?;
        admitted.outbound_utf16 = Checkpoint::read(reader)?;
        admitted.inbound_utf16 = Checkpoint::read(reader)?;
        admitted.failure_detail = Checkpoint::read(reader)?;
        admitted.failure_detail_length = Checkpoint::read(reader)?;
        admitted.argument_slots = Checkpoint::read(reader)?;
        admitted.argument_count = Checkpoint::read(reader)?;
        let pending_requests: PendingRequestTable = Checkpoint::read(reader)?;
        pending_requests.validate_checkpoint(&admitted.pending_requests)?;
        admitted.pending_requests = pending_requests;
        admitted.terminal = Checkpoint::read(reader)?;
        admitted.next_request_id = Checkpoint::read(reader)?;
        admitted.preparing_request = Checkpoint::read(reader)?;
        admitted.inbound_length = Checkpoint::read(reader)?;
        admitted.resuming_host_string = Checkpoint::read(reader)?;
        admitted.replies = Checkpoint::read(reader)?;
        admitted.published_requests = Checkpoint::read(reader)?;
        admitted.accepted_responses = Checkpoint::read(reader)?;
        if admitted.entry_arguments.len() != entry_capacity
            || admitted.outbound_utf16.len() != outbound_capacity
            || admitted.inbound_utf16.len() != inbound_capacity
            || admitted.argument_slots.len() != argument_capacity
            || admitted.argument_count > argument_capacity
            || admitted.inbound_length > inbound_capacity
            || admitted.failure_detail_length > admitted.failure_detail.len()
            || core::str::from_utf8(&admitted.failure_detail[..admitted.failure_detail_length])
                .is_err()
            || admitted.replies.len() > maximum_replies
            || admitted.next_request_id == 0
        {
            return Err(CheckpointError::InvalidState);
        }
        for request in admitted.pending_requests.requests() {
            if request.identity().request().get() >= admitted.next_request_id
                || !admitted.machine.checkpoint_task_waits_for(
                    request.identity().task(),
                    request.identity().request(),
                )
            {
                return Err(CheckpointError::InvalidState);
            }
            let operation = admitted
                .capabilities
                .get(request.capability() as usize)
                .and_then(Option::as_ref)
                .and_then(|capability| capability.operations.get(request.operation() as usize))
                .ok_or(CheckpointError::InvalidState)?;
            if operation.arguments.len() != request.arguments().len() {
                return Err(CheckpointError::InvalidState);
            }
        }
        let mut replied = std::collections::BTreeSet::new();
        for reply in &admitted.replies {
            if reply.request.get() >= admitted.next_request_id
                || !admitted
                    .machine
                    .checkpoint_task_waits_for(reply.task, reply.request)
                || !replied.insert((reply.task, reply.request))
                || admitted
                    .pending_requests
                    .get(HostRequestIdentity::new(reply.task, reply.request))
                    .is_some()
                || matches!(&reply.value, CopiedReply::String(units) if units.len() > inbound_capacity)
            {
                return Err(CheckpointError::InvalidState);
            }
        }
        Ok(admitted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{
        fixtures,
        host::{HostValueType, OperationSchema},
    };

    fn profile() -> ExecutionProfile {
        let image = fixtures::profile();
        ExecutionProfile {
            heap_bytes: image.heap_bytes,
            frame_storage_bytes: image.frame_storage_bytes,
            maximum_call_depth: image.maximum_call_depth,
            maximum_coroutines: image.maximum_coroutines,
            maximum_channels: image.maximum_channels,
            maximum_channel_values: image.maximum_channel_values,
            maximum_host_requests: image.maximum_host_requests,
            maximum_events: image.maximum_events,
            maximum_slice_budget: image.maximum_slice_budget,
            compiler_abi: image.compiler_abi,
            platform_abi: image.platform_abi,
            maximum_host_arguments: 16,
            maximum_outbound_utf16_code_units: 4096,
            maximum_inbound_utf16_code_units: 4096,
            maximum_accepted_responses: 64,
            entry_argument_limits: EntryArgumentLimits {
                maximum_count: 64,
                maximum_code_units_per_argument: 4096,
                maximum_total_code_units: 16384,
            },
        }
    }

    fn round_trip(
        original: &Session,
        artifact: VerifiedArtifact,
        bindings: &[CapabilityBinding<'_>],
    ) -> Session {
        let mut writer = Writer::new(8 * 1024 * 1024);
        original.write_checkpoint_state(&mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 8 * 1024 * 1024, 16 * 1024 * 1024).unwrap();
        let admitted = Session::admit_untraced(artifact, profile(), bindings).unwrap();
        let restored = Session::read_checkpoint_state(admitted, &mut reader).unwrap();
        reader.finish().unwrap();
        restored
    }

    #[test]
    fn checkpoint_retains_host_wait_identities_and_accounting() {
        let operations = [
            OperationSchema::asynchronous(&[], HostValueType::I32),
            OperationSchema::asynchronous(&[], HostValueType::Unit),
        ];
        let bindings = [CapabilityBinding::new("app", "entry", 1, 0, &operations)];
        let artifact = fixtures::two_task_host_artifact();
        let mut original = Session::admit_untraced(artifact.clone(), profile(), &bindings).unwrap();
        original.start(&[]).unwrap();
        let identities = match original.advance(64, 0).unwrap() {
            AdvanceOutcome::HostRequestBatch(batch) => (0..batch.len())
                .map(|index| {
                    let request = batch.get(index).unwrap();
                    (request.task_id(), request.id(), request.operation())
                })
                .collect::<Vec<_>>(),
            other => panic!("unexpected outcome {other:?}"),
        };
        assert_eq!(2, identities.len());
        let mut restored = round_trip(&original, artifact, &bindings);
        assert_eq!(original.accounting(), restored.accounting());
        for session in [&mut original, &mut restored] {
            for &(task, request, operation) in identities.iter().rev() {
                let value = if operation == 0 {
                    HostValueInput::I32(13)
                } else {
                    HostValueInput::Unit
                };
                session
                    .resume_for(task, request, HostResponse::Success(value))
                    .unwrap();
            }
            assert_eq!(
                AdvanceOutcome::Halted(None),
                session.advance(64, 0).unwrap()
            );
        }
        assert_eq!(original.accounting(), restored.accounting());
    }

    #[test]
    fn checkpoint_resumes_partial_host_string_materialization_without_accepting_twice() {
        let operations = [OperationSchema::asynchronous(&[], HostValueType::String)];
        let bindings = [CapabilityBinding::new("app", "entry", 1, 0, &operations)];
        let artifact = fixtures::string_response_capability_artifact();
        let mut original = Session::admit_untraced(artifact.clone(), profile(), &bindings).unwrap();
        original.start(&[]).unwrap();
        let (task, request) = match original.advance(64, 0).unwrap() {
            AdvanceOutcome::HostRequestBatch(batch) => {
                let request = batch.get(0).unwrap();
                (request.task_id(), request.id())
            }
            other => panic!("unexpected outcome {other:?}"),
        };
        let units = vec![0xd800; 64];
        original
            .resume_for(
                task,
                request,
                HostResponse::Success(HostValueInput::String(&units)),
            )
            .unwrap();
        for _ in 0..256 {
            let mut restored = round_trip(&original, artifact.clone(), &bindings);
            let expected = original.advance_with_retirement_limit(8, 1, 1).unwrap();
            let actual = restored.advance_with_retirement_limit(8, 1, 1).unwrap();
            assert_eq!(expected, actual);
            assert_eq!(original.accounting(), restored.accounting());
            let mut first = Writer::new(8 * 1024 * 1024);
            let mut second = Writer::new(8 * 1024 * 1024);
            original.write_checkpoint_state(&mut first).unwrap();
            restored.write_checkpoint_state(&mut second).unwrap();
            assert_eq!(first.finish(), second.finish());
            if original.machine.terminal_outcome().is_some() {
                return;
            }
        }
        panic!("host string did not complete");
    }

    #[test]
    fn checkpoint_rejects_changed_host_bindings_and_response_limits() {
        let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let bindings = [CapabilityBinding::new("app", "entry", 1, 2, &operations)];
        let artifact = fixtures::capability_artifact(true, true, 1, 0);
        let original = Session::admit_untraced(artifact.clone(), profile(), &bindings).unwrap();
        let mut writer = Writer::new(8 * 1024 * 1024);
        original.write_checkpoint_state(&mut writer).unwrap();
        let bytes = writer.finish();
        let other_bindings = [CapabilityBinding::new("app", "entry", 1, 3, &operations)];
        let admitted =
            Session::admit_untraced(artifact.clone(), profile(), &other_bindings).unwrap();
        let mut reader = Reader::new(&bytes, 8 * 1024 * 1024, 16 * 1024 * 1024).unwrap();
        assert!(matches!(
            Session::read_checkpoint_state(admitted, &mut reader),
            Err(CheckpointError::Incompatible)
        ));
        let mut other_profile = profile();
        other_profile.maximum_host_requests = 32;
        let admitted = Session::admit_untraced(artifact, other_profile, &bindings).unwrap();
        let mut reader = Reader::new(&bytes, 8 * 1024 * 1024, 16 * 1024 * 1024).unwrap();
        assert!(matches!(
            Session::read_checkpoint_state(admitted, &mut reader),
            Err(CheckpointError::Incompatible)
        ));
    }
}
