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

checkpoint_enum!(Lifecycle {
    0 => Pristine;
    1 => Runnable;
    2 => Terminal(v0);
});

checkpoint_enum!(TypeInitializationState {
    0 => Uninitialized;
    1 => Initializing;
    2 => Initialized;
    3 => Failed;
});

checkpoint_struct!(Frame {
    function,
    base,
    byte_len,
    block,
    instruction,
    caller_block,
    caller_instruction,
    destination,
    initializer
});

checkpoint_struct!(FailureStack {
    frames,
    length,
    omitted
});

checkpoint_struct!(TaskFailure { exception, stack });

checkpoint_struct!(PendingException {
    failure,
    actual_type,
    next_handler,
    retires_instruction
});

checkpoint_struct!(PendingHostFailure {
    role,
    length,
    bytes
});

checkpoint_enum!(AllocationShape {
    0 => Object;
    1 => Exception;
    2 => Array { length };
});

checkpoint_struct!(AllocationRetry {
    request,
    destination,
    logical_bytes,
    shape,
    source
});

checkpoint_enum!(StringCollectionTarget {
    0 => Concat;
    1 => HostResponse;
    2 => RecordResponse;
    3 => ExceptionMessage;
});

checkpoint_struct!(PendingRaise {
    ty,
    units,
    length,
    text,
    message,
    exception,
    stack,
    retires_instruction
});

// This internal representation is not yet a disk format or a public admission
// boundary. The enclosing session must validate host waits, resources and every
// managed reference before accepting a persisted checkpoint.
impl Machine {
    pub(in crate::execution) fn write_checkpoint_state(&self, writer: &mut Writer) -> Result<()> {
        if self.trace_enabled {
            return Err(CheckpointError::Incompatible);
        }
        self.image.content_hash().write(writer)?;
        self.lifecycle.write(writer)?;
        self.frames.write(writer)?;
        self.task_frames.write(writer)?;
        self.task_frame_depths.write(writer)?;
        self.task_failures.write(writer)?;
        self.task_host_failures.write(writer)?;
        self.tasks.write(writer)?;
        self.channels.write(writer)?;
        self.frame_arena.write(writer)?;
        self.statics.write(writer)?;
        self.type_initialization.write(writer)?;
        self.heap.write(writer)?;
        self.external_roots.write(writer)?;
        self.collector.write(writer)?;
        self.allocation_retry.write(writer)?;
        self.pending_allocation.write(writer)?;
        self.pending_array_copy.write(writer)?;
        self.pending_exception.write(writer)?;
        self.pending_raise.write(writer)?;
        self.pending_text.write(writer)?;
        self.pending_concat.write(writer)?;
        self.pending_concat_source.write(writer)?;
        self.pending_host_string.write(writer)?;
        self.pending_record.write(writer)?;
        self.pending_record_source.write(writer)?;
        self.pending_host_string_source.write(writer)?;
        self.string_collection_pending.write(writer)?;
        self.task_string_response.write(writer)?;
        self.emergency_oom.write(writer)?;
        self.frame_depth.write(writer)?;
        self.failure_stack.write(writer)?;
        self.consumed_fixed_cost.write(writer)?;
        self.consumed_dynamic_cost.write(writer)?;
        self.consumed_maintenance_cost.write(writer)?;
        self.entered_blocks.write(writer)?;
        self.executed_instructions.write(writer)?;
        self.retired_instructions.write(writer)?;
        self.maximum_observed_frame_depth.write(writer)?;
        Ok(())
    }

    pub(in crate::execution) fn read_checkpoint_state(
        image: ExecutionImage,
        reader: &mut Reader<'_>,
    ) -> Result<Self> {
        let identity: [u8; 32] = Checkpoint::read(reader)?;
        if identity != image.content_hash() {
            return Err(CheckpointError::Incompatible);
        }
        let mut machine = Self {
            image,
            lifecycle: Checkpoint::read(reader)?,
            frames: Checkpoint::read(reader)?,
            task_frames: Checkpoint::read(reader)?,
            task_frame_depths: Checkpoint::read(reader)?,
            task_failures: Checkpoint::read(reader)?,
            task_host_failures: Checkpoint::read(reader)?,
            tasks: Checkpoint::read(reader)?,
            channels: Checkpoint::read(reader)?,
            frame_arena: Checkpoint::read(reader)?,
            statics: Checkpoint::read(reader)?,
            type_initialization: Checkpoint::read(reader)?,
            heap: Checkpoint::read(reader)?,
            external_roots: Checkpoint::read(reader)?,
            collector: Checkpoint::read(reader)?,
            allocation_retry: Checkpoint::read(reader)?,
            pending_allocation: Checkpoint::read(reader)?,
            pending_array_copy: Checkpoint::read(reader)?,
            pending_exception: Checkpoint::read(reader)?,
            pending_raise: Checkpoint::read(reader)?,
            pending_text: Checkpoint::read(reader)?,
            pending_concat: Checkpoint::read(reader)?,
            pending_concat_source: Checkpoint::read(reader)?,
            pending_host_string: Checkpoint::read(reader)?,
            pending_record: Checkpoint::read(reader)?,
            pending_record_source: Checkpoint::read(reader)?,
            pending_host_string_source: Checkpoint::read(reader)?,
            string_collection_pending: Checkpoint::read(reader)?,
            task_string_response: Checkpoint::read(reader)?,
            emergency_oom: Checkpoint::read(reader)?,
            frame_depth: Checkpoint::read(reader)?,
            failure_stack: Checkpoint::read(reader)?,
            consumed_fixed_cost: Checkpoint::read(reader)?,
            consumed_dynamic_cost: Checkpoint::read(reader)?,
            consumed_maintenance_cost: Checkpoint::read(reader)?,
            entered_blocks: Checkpoint::read(reader)?,
            executed_instructions: Checkpoint::read(reader)?,
            retired_instructions: Checkpoint::read(reader)?,
            maximum_observed_frame_depth: Checkpoint::read(reader)?,
            trace: Sha256::new(),
            trace_enabled: false,
        };
        machine.validate_checkpoint_storage()?;
        Ok(machine)
    }

    pub(in crate::execution) fn restore_checkpoint_state(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<()> {
        *self = Self::read_checkpoint_state(self.image.clone(), reader)?;
        Ok(())
    }

    pub(in crate::execution) fn checkpoint_task_waits_for(
        &self,
        task: TaskId,
        request: RequestId,
    ) -> bool {
        self.tasks.state(task)
            == Some(super::super::task::TaskState::Waiting(
                super::super::task::TaskWait::Host(request),
            ))
    }

    fn validate_checkpoint_storage(&mut self) -> Result<()> {
        let invalid = || CheckpointError::InvalidState;
        let width = self.image.maximum_call_depth();
        let tasks = self.image.maximum_coroutines();
        if self.frames.len() != width
            || Some(self.task_frames.len()) != width.checked_mul(tasks)
            || self.task_frame_depths.len() != tasks
            || self.task_failures.len() != tasks
            || self.task_host_failures.len()
                != if self.image.runtime_exception_type(8).is_some() {
                    tasks
                } else {
                    0
                }
            || self.frame_depth > width
            || self.task_frame_depths.iter().any(|depth| *depth > width)
            || self.type_initialization.len() != self.image.type_count()
            || self.external_roots.len() != self.image.external_root_capacity() as usize
            || self.maximum_observed_frame_depth > width
        {
            return Err(invalid());
        }
        self.tasks.validate_checkpoint(tasks)?;
        self.channels.validate_checkpoint(
            self.image.maximum_channels(),
            self.image.maximum_channel_values(),
            tasks,
        )?;
        self.heap.validate_checkpoint(&self.image.storage_plan())?;
        self.statics
            .validate_checkpoint(self.image.static_layout())?;
        let mut reservations = Vec::new();
        for frames in core::iter::once(&self.frames[..self.frame_depth]).chain(
            self.task_frames
                .chunks(width)
                .zip(&self.task_frame_depths)
                .map(|(frames, depth)| &frames[..*depth]),
        ) {
            for (index, frame) in frames.iter().enumerate() {
                let function = self.image.function(frame.function).ok_or_else(invalid)?;
                let block = self.image.block(frame.block).ok_or_else(invalid)?;
                if block.function != frame.function
                    || frame.instruction > block.instructions.len()
                    || function.frame_layout.byte_len.checked_next_multiple_of(8)
                        != Some(frame.byte_len)
                    || frame
                        .initializer
                        .is_some_and(|key| self.image.type_index(key).is_none())
                {
                    return Err(invalid());
                }
                if index == 0 {
                    if frame.caller_block != usize::MAX || frame.destination != u16::MAX {
                        return Err(invalid());
                    }
                } else {
                    let caller = self
                        .image
                        .function(frames[index - 1].function)
                        .ok_or_else(invalid)?;
                    let continuation = self.image.block(frame.caller_block).ok_or_else(invalid)?;
                    if continuation.function != frames[index - 1].function
                        || frame.caller_instruction > continuation.instructions.len()
                        || frame.destination != u16::MAX
                            && frame.destination as usize >= caller.register_count
                    {
                        return Err(invalid());
                    }
                }
                reservations
                    .try_reserve(1)
                    .map_err(|_| CheckpointError::Allocation)?;
                reservations.push(FrameReservation {
                    base: frame.base,
                    byte_len: frame.byte_len,
                });
            }
        }
        self.frame_arena.validate_checkpoint(
            self.image.storage_plan().frame_arena_bytes as usize,
            &reservations,
        )?;
        if self
            .failure_stack
            .is_some_and(|stack| stack.length > MAXIMUM_FAILURE_FRAMES)
            || self
                .task_failures
                .iter()
                .flatten()
                .any(|failure| failure.stack.length > MAXIMUM_FAILURE_FRAMES)
            || self
                .task_host_failures
                .iter()
                .flatten()
                .any(|failure| failure.length > failure.bytes.len())
            || self.pending_raise.as_ref().is_some_and(|raise| {
                raise.length > raise.units.len() || raise.stack.length > MAXIMUM_FAILURE_FRAMES
            })
        {
            return Err(invalid());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::fixtures;

    fn round_trip(original: &Machine) -> Machine {
        let mut writer = Writer::new(8 * 1024 * 1024);
        original.write_checkpoint_state(&mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 8 * 1024 * 1024, 16 * 1024 * 1024).unwrap();
        let restored = Machine::read_checkpoint_state(original.image.clone(), &mut reader).unwrap();
        reader.finish().unwrap();
        restored
    }

    #[test]
    fn checkpoint_resumes_nested_calls_at_every_instruction_boundary() {
        let mut original = fixtures::started_zero_arg_untraced(fixtures::nested_call_artifact());
        for _ in 0..64 {
            let mut restored = round_trip(&original);
            let expected = original.run_slice_with_retirement_limit(32, 8, 1).unwrap();
            let actual = restored.run_slice_with_retirement_limit(32, 8, 1).unwrap();
            assert_eq!(expected, actual);
            let mut first = Writer::new(8 * 1024 * 1024);
            let mut second = Writer::new(8 * 1024 * 1024);
            original.write_checkpoint_state(&mut first).unwrap();
            restored.write_checkpoint_state(&mut second).unwrap();
            assert_eq!(first.finish(), second.finish());
            if expected != Outcome::SliceExhausted {
                return;
            }
        }
        panic!("nested program did not complete");
    }

    #[test]
    fn checkpoint_resumes_allocation_text_unwinding_tasks_and_channels() {
        for (name, artifact) in [
            ("allocation", fixtures::object_allocation_artifact(8)),
            ("text", fixtures::literal_string_concat_artifact()),
            ("exception", fixtures::exception_artifact(true, true, false)),
            ("task", fixtures::task_spawn_join_artifact()),
            ("channel", fixtures::channel_handoff_artifact()),
        ] {
            let mut original = fixtures::started_zero_arg_untraced(artifact);
            let mut completed = false;
            for _ in 0..256 {
                let mut restored = round_trip(&original);
                let expected = original.run_slice_with_retirement_limit(32, 1, 1).unwrap();
                let actual = restored.run_slice_with_retirement_limit(32, 1, 1).unwrap();
                assert_eq!(expected, actual, "{name}");
                let mut first = Writer::new(8 * 1024 * 1024);
                let mut second = Writer::new(8 * 1024 * 1024);
                original.write_checkpoint_state(&mut first).unwrap();
                restored.write_checkpoint_state(&mut second).unwrap();
                assert_eq!(first.finish(), second.finish(), "{name}");
                if !matches!(expected, Outcome::SliceExhausted) {
                    completed = true;
                    break;
                }
            }
            assert!(completed, "{name} did not complete");
        }
    }

    #[test]
    fn checkpoint_resumes_each_incremental_gc_phase() {
        let mut profile = fixtures::profile();
        profile.heap_bytes = 32;
        let image = ExecutionImage::admit(fixtures::gc_retry_artifact(), profile).unwrap();
        let mut original = Machine::new_untraced(image).unwrap();
        original.start(&[]).unwrap();
        let mut saw_collection = false;
        for _ in 0..256 {
            saw_collection |= original.collector.is_active();
            let mut restored = round_trip(&original);
            let expected = original.run_slice_with_retirement_limit(32, 1, 1).unwrap();
            let actual = restored.run_slice_with_retirement_limit(32, 1, 1).unwrap();
            assert_eq!(expected, actual);
            let mut first = Writer::new(8 * 1024 * 1024);
            let mut second = Writer::new(8 * 1024 * 1024);
            original.write_checkpoint_state(&mut first).unwrap();
            restored.write_checkpoint_state(&mut second).unwrap();
            assert_eq!(first.finish(), second.finish());
            if expected != Outcome::SliceExhausted {
                assert!(saw_collection);
                assert!(matches!(
                    expected,
                    Outcome::Halted(Some(RuntimeValue::Reference(_)))
                ));
                return;
            }
        }
        panic!("incremental collection did not complete");
    }

    #[test]
    fn checkpoint_rejects_other_executable_and_out_of_image_program_counter() {
        let mut original = fixtures::started_zero_arg_untraced(fixtures::nested_call_artifact());
        let mut writer = Writer::new(8 * 1024 * 1024);
        original.write_checkpoint_state(&mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 8 * 1024 * 1024, 16 * 1024 * 1024).unwrap();
        let other = fixtures::started_zero_arg_untraced(fixtures::two_block_artifact(3, 5));
        assert!(matches!(
            Machine::read_checkpoint_state(other.image.clone(), &mut reader),
            Err(CheckpointError::Incompatible)
        ));
        original.frames[0].instruction = usize::MAX;
        assert_eq!(
            Err(CheckpointError::InvalidState),
            original.validate_checkpoint_storage()
        );
    }
}
