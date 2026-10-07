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
use crate::execution::host::HostValueType;

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
        admitted
            .machine
            .validate_checkpoint_host_string(&admitted.inbound_utf16[..admitted.inbound_length])?;
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
            if operation.arguments.len() != request.arguments().len()
                || admitted.machine.checkpoint_host_operation(
                    request.identity().task(),
                    request.identity().request(),
                ) != Some((request.capability(), request.operation()))
                || operation
                    .arguments
                    .iter()
                    .zip(request.arguments())
                    .any(|(expected, slot)| checkpoint_slot_type(*slot) != Some(*expected))
            {
                return Err(CheckpointError::InvalidState);
            }
        }
        let mut replied = std::collections::BTreeSet::new();
        for reply in &admitted.replies {
            let (capability, operation) = admitted
                .machine
                .checkpoint_host_operation(reply.task, reply.request)
                .ok_or(CheckpointError::InvalidState)?;
            let schema = admitted
                .capabilities
                .get(capability as usize)
                .and_then(Option::as_ref)
                .and_then(|capability| capability.operations.get(operation as usize))
                .ok_or(CheckpointError::InvalidState)?;
            let valid_value = match &reply.value {
                CopiedReply::Scalar(None) => schema.result == HostValueType::Unit,
                CopiedReply::Scalar(Some(value)) => {
                    runtime_slot(*value).and_then(checkpoint_slot_type) == Some(schema.result)
                }
                CopiedReply::String(units) => {
                    schema.result == HostValueType::String && units.len() <= inbound_capacity
                }
                CopiedReply::Record(record) => schema
                    .result_record
                    .as_ref()
                    .is_some_and(|schema| schema.accepts(record, inbound_capacity)),
                CopiedReply::Failure(failure) => {
                    !failure.detail().is_empty()
                        && failure.detail().len() <= admitted.failure_detail.len()
                }
            };
            if !valid_value
                || reply.request.get() >= admitted.next_request_id
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
        if admitted.resuming_host_string != admitted.machine.capability_string_response_pending() {
            return Err(CheckpointError::InvalidState);
        }
        if let Some(preparing) = &admitted.preparing_request {
            let suspension = admitted
                .machine
                .capability_suspension()
                .map_err(|_| CheckpointError::InvalidState)?;
            let operation = admitted
                .capabilities
                .get(preparing.capability as usize)
                .and_then(Option::as_ref)
                .and_then(|capability| capability.operations.get(preparing.operation as usize))
                .ok_or(CheckpointError::InvalidState)?;
            if preparing.id.get() != admitted.next_request_id
                || admitted.machine.current_task().ok() != Some(preparing.task)
                || (suspension.capability, suspension.operation)
                    != (preparing.capability, preparing.operation)
                || suspension.arguments.len() != admitted.argument_count
                || operation.arguments.len() != admitted.argument_count
                || preparing.argument > admitted.argument_count
            {
                return Err(CheckpointError::InvalidState);
            }
            for (index, ((expected, slot), register)) in operation
                .arguments
                .iter()
                .zip(&admitted.argument_slots[..admitted.argument_count])
                .zip(suspension.arguments)
                .enumerate()
            {
                if checkpoint_slot_type(*slot) != Some(*expected) {
                    return Err(CheckpointError::InvalidState);
                }
                if let HostValueSlot::String { start, length } = slot {
                    if start
                        .checked_add(*length)
                        .is_none_or(|end| end as usize > outbound_capacity)
                        || admitted.machine.capability_string_length(*register).ok()
                            != Some(*length)
                        || index == preparing.argument && preparing.string_offset > *length
                    {
                        return Err(CheckpointError::InvalidState);
                    }
                } else if index == preparing.argument && preparing.string_offset != 0 {
                    return Err(CheckpointError::InvalidState);
                }
            }
            if preparing.argument == admitted.argument_count && preparing.string_offset != 0 {
                return Err(CheckpointError::InvalidState);
            }
        }
        Ok(admitted)
    }
}

fn checkpoint_slot_type(slot: HostValueSlot) -> Option<HostValueType> {
    Some(match slot {
        HostValueSlot::Empty => return None,
        HostValueSlot::I32(_) => HostValueType::I32,
        HostValueSlot::I64(_) => HostValueType::I64,
        HostValueSlot::F32(_) => HostValueType::F32,
        HostValueSlot::F64(_) => HostValueType::F64,
        HostValueSlot::Bool(_) => HostValueType::Bool,
        HostValueSlot::Char(_) => HostValueType::Char,
        HostValueSlot::String { .. } => HostValueType::String,
    })
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
    fn checkpoint_resumes_each_record_materialization_boundary() {
        use crate::execution::session_tests::{record_schema, record_value};
        let schema = record_schema();
        let operations = [OperationSchema::asynchronous_record(&[], &schema)];
        let bindings = [CapabilityBinding::new("app", "entry", 1, 0, &operations)];
        let artifact = fixtures::record_response_artifact(13, true);
        let mut original = Session::admit_untraced(artifact.clone(), profile(), &bindings).unwrap();
        original.start(&[]).unwrap();
        let id = match original.advance(64, 0).unwrap() {
            AdvanceOutcome::HostRequestBatch(batch) => batch.get(0).unwrap().id(),
            other => panic!("{other:?}"),
        };
        let value = record_value(0x7ff0000000000042, 128);
        original
            .resume(id, HostResponse::Success(HostValueInput::Record(&value)))
            .unwrap();
        for _ in 0..512 {
            let mut restored = round_trip(&original, artifact.clone(), &bindings);
            let expected = original.advance_with_retirement_limit(8, 1, 1).unwrap();
            assert_eq!(
                expected,
                restored.advance_with_retirement_limit(8, 1, 1).unwrap()
            );
            let completed = matches!(expected, AdvanceOutcome::Halted(_));
            let mut first = Writer::new(8 * 1024 * 1024);
            let mut second = Writer::new(8 * 1024 * 1024);
            original.write_checkpoint_state(&mut first).unwrap();
            restored.write_checkpoint_state(&mut second).unwrap();
            assert_eq!(first.finish(), second.finish());
            if completed {
                return;
            }
        }
        panic!("record response did not complete");
    }

    fn assert_rejected(
        original: &Session,
        artifact: VerifiedArtifact,
        bindings: &[CapabilityBinding<'_>],
    ) {
        let mut writer = Writer::new(8 * 1024 * 1024);
        original.write_checkpoint_state(&mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 8 * 1024 * 1024, 16 * 1024 * 1024).unwrap();
        let admitted = Session::admit_untraced(artifact, profile(), bindings).unwrap();
        assert!(matches!(
            Session::read_checkpoint_state(admitted, &mut reader),
            Err(CheckpointError::InvalidState)
        ));
    }

    #[test]
    fn checkpoint_rejects_queued_reply_that_disagrees_with_suspended_operation() {
        let operations = [
            OperationSchema::asynchronous(&[], HostValueType::I32),
            OperationSchema::asynchronous(&[], HostValueType::Unit),
        ];
        let bindings = [CapabilityBinding::new("app", "entry", 1, 0, &operations)];
        let artifact = fixtures::two_task_host_artifact();
        let mut original = Session::admit_untraced(artifact.clone(), profile(), &bindings).unwrap();
        original.start(&[]).unwrap();
        original.advance(64, 0).unwrap();
        let request = original
            .pending_requests
            .requests()
            .iter()
            .find(|request| request.operation() == 0)
            .unwrap()
            .identity();
        original.pending_requests.take(request).unwrap();
        original.replies.push_back(QueuedReply {
            task: request.task(),
            request: request.request(),
            value: CopiedReply::Scalar(Some(RuntimeValue::I64(13))),
        });
        assert_rejected(&original, artifact, &bindings);
    }

    #[test]
    fn checkpoint_resumes_outbound_string_copy_and_rejects_foreign_copy_cursor() {
        let operations = [OperationSchema::asynchronous(
            &[HostValueType::String],
            HostValueType::Unit,
        )];
        let bindings = [CapabilityBinding::new("app", "entry", 1, 0, &operations)];
        let artifact = fixtures::string_capability_artifact(&[0x61; 64], false, false);
        let mut original = Session::admit_untraced(artifact.clone(), profile(), &bindings).unwrap();
        original.start(&[]).unwrap();
        for _ in 0..128 {
            original.advance(8, 0).unwrap();
            if original.preparing_request.is_some() {
                break;
            }
        }
        assert!(original.preparing_request.is_some());
        let mut restored = round_trip(&original, artifact.clone(), &bindings);
        for _ in 0..128 {
            let expected = original.advance(8, 0).unwrap();
            assert_eq!(expected, restored.advance(8, 0).unwrap());
            if matches!(expected, AdvanceOutcome::HostRequestBatch(_)) {
                break;
            }
        }
        assert!(original.preparing_request.is_none());
        assert_eq!(
            original.pending_requests.requests(),
            restored.pending_requests.requests()
        );
        let mut corrupt = Session::admit_untraced(artifact.clone(), profile(), &bindings).unwrap();
        corrupt.start(&[]).unwrap();
        for _ in 0..128 {
            corrupt.advance(8, 0).unwrap();
            if let Some(preparing) = &mut corrupt.preparing_request {
                preparing.string_offset = 65;
                break;
            }
        }
        assert_rejected(&corrupt, artifact, &bindings);
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
