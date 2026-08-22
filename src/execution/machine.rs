use super::{
    error::{AdmissionError, AllocationExhaustion, GuestTrap, Outcome, RunError, VmFault},
    heap::{AllocationRequest, Heap},
    heap_ops::{PendingAllocation, PendingState},
    image::{ExecutionImage, ResolvedInstruction, ResolvedValueType},
    layout::{array_layout, RuntimeTypeLayout},
    numeric,
    value::{EntryArgument, RegisterValue, RuntimeValue},
};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Lifecycle {
    Pristine,
    Runnable,
    Terminal(Outcome),
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    function: usize,
    block: usize,
    instruction: usize,
    caller_instruction: usize,
    destination: u16,
}

impl Frame {
    const EMPTY: Self = Self {
        function: usize::MAX,
        block: usize::MAX,
        instruction: 0,
        caller_instruction: 0,
        destination: u16::MAX,
    };
}

pub(super) struct Machine {
    image: ExecutionImage,
    lifecycle: Lifecycle,
    frames: Box<[Frame]>,
    registers: Box<[RegisterValue]>,
    heap: Heap,
    pending_allocation: Option<PendingAllocation>,
    frame_depth: usize,
    consumed_fixed_cost: u64,
    consumed_dynamic_cost: u64,
    entered_blocks: u64,
    executed_instructions: u64,
    maximum_observed_frame_depth: usize,
    trace: Sha256,
}

impl Machine {
    pub(super) fn new(image: ExecutionImage) -> Result<Self, AdmissionError> {
        let heap = Heap::new(&image.storage_plan())?;
        let frame_count = image.maximum_call_depth();
        let register_count = frame_count
            .checked_mul(image.registers_per_frame())
            .ok_or(AdmissionError::StoragePlanOverflow)?;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(frame_count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        frames.resize(frame_count, Frame::EMPTY);
        let mut registers = Vec::new();
        registers
            .try_reserve_exact(register_count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        registers.resize(register_count, RegisterValue::Uninitialized);
        Ok(Self {
            image,
            lifecycle: Lifecycle::Pristine,
            frames: frames.into_boxed_slice(),
            registers: registers.into_boxed_slice(),
            heap,
            pending_allocation: None,
            frame_depth: 0,
            consumed_fixed_cost: 0,
            consumed_dynamic_cost: 0,
            entered_blocks: 0,
            executed_instructions: 0,
            maximum_observed_frame_depth: 0,
            trace: Sha256::new(),
        })
    }

    pub(super) fn start(&mut self, arguments: &[EntryArgument]) -> Result<(), RunError> {
        if self.lifecycle != Lifecycle::Pristine {
            return Err(RunError::AlreadyStarted);
        }
        let entry_index = self.image.entry_index();
        let entry = self
            .image
            .function(entry_index)
            .ok_or(RunError::NotRunnable)?;
        let supplied = u16::try_from(arguments.len()).unwrap_or(u16::MAX);
        if arguments.len() != entry.parameter_count {
            return Err(RunError::EntryArity {
                expected: entry.parameter_count as u16,
                supplied,
            });
        }
        for (parameter, (argument, expected)) in arguments
            .iter()
            .zip(&entry.registers[..entry.parameter_count])
            .enumerate()
        {
            self.validate_argument(parameter as u16, *argument, *expected)?;
        }

        let entry = self
            .image
            .function(entry_index)
            .ok_or(RunError::NotRunnable)?;
        self.frames[0] = Frame {
            function: entry_index,
            block: entry.first_block,
            instruction: 0,
            caller_instruction: 0,
            destination: u16::MAX,
        };
        let width = self.image.registers_per_frame();
        self.registers[..width].fill(RegisterValue::Uninitialized);
        for (slot, argument) in self.registers.iter_mut().zip(arguments) {
            *slot = RegisterValue::Initialized(argument.value);
        }
        self.frame_depth = 1;
        self.maximum_observed_frame_depth = 1;
        self.lifecycle = Lifecycle::Runnable;
        Ok(())
    }

    pub(super) fn run_slice(&mut self, budget: u32) -> Result<Outcome, RunError> {
        match self.lifecycle {
            Lifecycle::Terminal(outcome) => return Ok(outcome),
            Lifecycle::Pristine => return Err(RunError::NotStarted),
            Lifecycle::Runnable => {}
        }
        let minimum = if self.pending_allocation.is_some() {
            1
        } else {
            self.image.minimum_slice_cost()
        };
        if budget == 0 || budget < minimum || budget > self.image.maximum_slice_budget() {
            return Err(RunError::InvalidSliceBudget {
                minimum,
                maximum: self.image.maximum_slice_budget(),
                supplied: budget,
            });
        }
        let mut remaining = budget;
        loop {
            let frame_index = self
                .frame_depth
                .checked_sub(1)
                .ok_or(RunError::NotRunnable)?;
            if self.pending_allocation.is_some() {
                if let Some(outcome) = self.resume_pending_allocation(frame_index, &mut remaining) {
                    return Ok(outcome);
                }
            }
            let block_index = self.frames[frame_index].block;
            let block_cost = self
                .image
                .block(block_index)
                .ok_or(RunError::NotRunnable)?
                .fixed_cost;
            if self.frames[frame_index].instruction == 0 {
                if block_cost > remaining {
                    return Ok(Outcome::SliceExhausted);
                }
                remaining -= block_cost;
                let Some(consumed) = self.consumed_fixed_cost.checked_add(u64::from(block_cost))
                else {
                    return Ok(self.fault(VmFault::AccountingOverflow));
                };
                let Some(entered_blocks) = self.entered_blocks.checked_add(1) else {
                    return Ok(self.fault(VmFault::AccountingOverflow));
                };
                self.consumed_fixed_cost = consumed;
                self.entered_blocks = entered_blocks;
                self.trace_block_entry(frame_index, block_index, remaining)?;
            }
            let block_len = self
                .image
                .block(block_index)
                .ok_or(RunError::NotRunnable)?
                .instructions
                .len();

            while self.frames[frame_index].instruction < block_len {
                let instruction_index = self.frames[frame_index].instruction;
                let instruction = &self
                    .image
                    .block(block_index)
                    .ok_or(RunError::NotRunnable)?
                    .instructions[instruction_index];
                let Some(executed_instructions) = self.executed_instructions.checked_add(1) else {
                    return Ok(self.fault(VmFault::AccountingOverflow));
                };
                self.executed_instructions = executed_instructions;
                match instruction {
                    ResolvedInstruction::Return { value } => {
                        let returned = if *value == u16::MAX {
                            None
                        } else {
                            match self.read_register(frame_index, *value) {
                                Ok(value) => Some(value),
                                Err(fault) => return Ok(self.fault(fault)),
                            }
                        };
                        let function = self
                            .image
                            .function(self.frames[frame_index].function)
                            .ok_or(RunError::NotRunnable)?;
                        if (function.result.kind == 0) != returned.is_none() {
                            return Ok(self.fault(VmFault::InvalidValueType));
                        }
                        if frame_index == 0 {
                            let outcome = Outcome::Halted(returned);
                            self.lifecycle = Lifecycle::Terminal(outcome);
                            self.frame_depth = 0;
                            return Ok(outcome);
                        }

                        let continuation = self.frames[frame_index].caller_instruction;
                        let destination = self.frames[frame_index].destination;
                        if (destination == u16::MAX) != returned.is_none() {
                            return Ok(self.fault(VmFault::InvalidValueType));
                        }
                        let width = self.image.registers_per_frame();
                        let callee_base = frame_index
                            .checked_mul(width)
                            .ok_or(RunError::NotRunnable)?;
                        self.registers[callee_base..callee_base + width]
                            .fill(RegisterValue::Uninitialized);
                        self.frames[frame_index] = Frame::EMPTY;
                        self.frame_depth = frame_index;
                        let caller_index = frame_index - 1;
                        self.frames[caller_index].instruction = continuation;
                        if let Some(value) = returned {
                            let destination_index = caller_index
                                .checked_mul(width)
                                .and_then(|base| base.checked_add(destination as usize))
                                .ok_or(RunError::NotRunnable)?;
                            let Some(slot) = self.registers.get_mut(destination_index) else {
                                return Ok(self.fault(VmFault::InvalidStoragePlan));
                            };
                            *slot = RegisterValue::Initialized(value);
                        }
                        break;
                    }
                    ResolvedInstruction::CallDirect { dst, target, args } => {
                        let Some(target_function) = self.image.function(*target) else {
                            return Ok(self.fault(VmFault::InvalidResolvedId));
                        };
                        if target_function.parameter_count != args.len() {
                            return Ok(self.fault(VmFault::InvalidValueType));
                        }
                        for source in args.iter() {
                            if let Err(fault) = self.read_register(frame_index, *source) {
                                return Ok(self.fault(fault));
                            }
                        }
                        if self.frame_depth >= self.image.maximum_call_depth() {
                            let outcome = Outcome::Crashed(GuestTrap::StackOverflow);
                            self.lifecycle = Lifecycle::Terminal(outcome);
                            return Ok(outcome);
                        }
                        let callee_index = self.frame_depth;
                        if callee_index >= self.frames.len() {
                            return Ok(self.fault(VmFault::InvalidStoragePlan));
                        }
                        let width = self.image.registers_per_frame();
                        let callee_base = callee_index
                            .checked_mul(width)
                            .ok_or(RunError::NotRunnable)?;
                        let Some(callee_registers) = self
                            .registers
                            .get_mut(callee_base..callee_base.saturating_add(width))
                        else {
                            return Ok(self.fault(VmFault::InvalidStoragePlan));
                        };
                        callee_registers.fill(RegisterValue::Uninitialized);
                        for (parameter, source) in args.iter().enumerate() {
                            let value = match self.read_register(frame_index, *source) {
                                Ok(value) => value,
                                Err(fault) => return Ok(self.fault(fault)),
                            };
                            self.registers[callee_base + parameter] =
                                RegisterValue::Initialized(value);
                        }
                        self.frames[callee_index] = Frame {
                            function: *target,
                            block: target_function.first_block,
                            instruction: 0,
                            caller_instruction: instruction_index + 1,
                            destination: *dst,
                        };
                        self.frame_depth += 1;
                        self.maximum_observed_frame_depth =
                            self.maximum_observed_frame_depth.max(self.frame_depth);
                        break;
                    }
                    ResolvedInstruction::Jump { target } => {
                        self.frames[frame_index].block = *target;
                        self.frames[frame_index].instruction = 0;
                        break;
                    }
                    ResolvedInstruction::Branch {
                        condition,
                        true_block,
                        false_block,
                    } => {
                        let target = match self.read_register(frame_index, *condition) {
                            Ok(RuntimeValue::Bool(true)) => *true_block,
                            Ok(RuntimeValue::Bool(false)) => *false_block,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        self.frames[frame_index].block = target;
                        self.frames[frame_index].instruction = 0;
                        break;
                    }
                    ResolvedInstruction::SwitchI32 {
                        key,
                        default_block,
                        cases,
                    } => {
                        let key = match self.read_register(frame_index, *key) {
                            Ok(RuntimeValue::I32(value)) => value,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let target = cases
                            .binary_search_by_key(&key, |case| case.value)
                            .map(|index| cases[index].target)
                            .unwrap_or(*default_block);
                        self.frames[frame_index].block = target;
                        self.frames[frame_index].instruction = 0;
                        break;
                    }
                    ResolvedInstruction::NewObject { dst, ty } => {
                        let RuntimeTypeLayout::Object(layout) =
                            self.image.type_layout(*ty).ok_or(RunError::NotRunnable)?
                        else {
                            return Ok(self.fault(VmFault::InvalidResolvedId));
                        };
                        let request = AllocationRequest {
                            block_bytes: layout.block_bytes,
                            ty: *ty,
                        };
                        let logical_bytes = layout.payload_bytes;
                        let reservation = match self.heap.reserve(request) {
                            Ok(Some(reservation)) => reservation,
                            Ok(None) => {
                                return Ok(self.allocation_exhausted(request.block_bytes, false))
                            }
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        self.pending_allocation = Some(PendingAllocation::Object(PendingState {
                            request,
                            reservation,
                            destination: *dst,
                            logical_bytes,
                            initialized_bytes: 0,
                            fixed_cost_paid: true,
                            collection_attempted: false,
                        }));
                        if let Some(outcome) =
                            self.resume_pending_allocation(frame_index, &mut remaining)
                        {
                            return Ok(outcome);
                        }
                    }
                    ResolvedInstruction::NewArray { dst, ty, length } => {
                        let length = match self.read_register(frame_index, *length) {
                            Ok(RuntimeValue::I32(length)) => length,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        if length < 0 {
                            let outcome = Outcome::Crashed(GuestTrap::NegativeArraySize);
                            self.lifecycle = Lifecycle::Terminal(outcome);
                            return Ok(outcome);
                        }
                        let RuntimeTypeLayout::Array { element } =
                            self.image.type_layout(*ty).ok_or(RunError::NotRunnable)?
                        else {
                            return Ok(self.fault(VmFault::InvalidResolvedId));
                        };
                        let layout = match array_layout(*element, length) {
                            Ok(layout) => layout,
                            Err(_) => return Ok(self.allocation_exhausted(u32::MAX, false)),
                        };
                        let request = AllocationRequest {
                            block_bytes: layout.block_bytes,
                            ty: *ty,
                        };
                        if request.block_bytes > self.image.storage_plan().heap_bytes {
                            return Ok(self.allocation_exhausted(request.block_bytes, false));
                        }
                        let reservation = match self.heap.reserve(request) {
                            Ok(Some(reservation)) => reservation,
                            Ok(None) => {
                                return Ok(self.allocation_exhausted(request.block_bytes, false))
                            }
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        self.pending_allocation = Some(PendingAllocation::Array {
                            state: PendingState {
                                request,
                                reservation,
                                destination: *dst,
                                logical_bytes: layout.payload_bytes,
                                initialized_bytes: 0,
                                fixed_cost_paid: true,
                                collection_attempted: false,
                            },
                            length: layout.length,
                        });
                        if let Some(outcome) =
                            self.resume_pending_allocation(frame_index, &mut remaining)
                        {
                            return Ok(outcome);
                        }
                    }
                    ResolvedInstruction::Unreachable => {
                        return Ok(self.fault(VmFault::ReachedUnreachable));
                    }
                    _ => {
                        let width = self.image.registers_per_frame();
                        let base = frame_index * width;
                        let registers = &mut self.registers[base..base + width];
                        let register_types = &self
                            .image
                            .function(self.frames[frame_index].function)
                            .ok_or(RunError::NotRunnable)?
                            .registers;
                        match execute_scalar(instruction, registers, register_types, &self.image) {
                            Ok(()) => self.frames[frame_index].instruction += 1,
                            Err(InstructionFailure::Trap(trap)) => {
                                let outcome = Outcome::Crashed(trap);
                                self.lifecycle = Lifecycle::Terminal(outcome);
                                return Ok(outcome);
                            }
                            Err(InstructionFailure::Fault(fault)) => {
                                return Ok(self.fault(fault));
                            }
                        }
                    }
                }
            }
            if self.frames[frame_index].instruction == block_len {
                return Ok(self.fault(VmFault::CorruptLifecycle));
            }
        }
    }

    fn read_register(&self, frame: usize, register: u16) -> Result<RuntimeValue, VmFault> {
        let index = frame
            .checked_mul(self.image.registers_per_frame())
            .and_then(|base| base.checked_add(register as usize))
            .ok_or(VmFault::InvalidStoragePlan)?;
        match self.registers.get(index) {
            Some(RegisterValue::Initialized(value)) => Ok(*value),
            _ => Err(VmFault::InvalidValueType),
        }
    }

    fn fault(&mut self, fault: VmFault) -> Outcome {
        self.cancel_pending_allocation();
        let outcome = Outcome::Faulted(fault);
        self.lifecycle = Lifecycle::Terminal(outcome);
        outcome
    }

    fn allocation_exhausted(
        &mut self,
        requested_block_bytes: u32,
        collection_attempted: bool,
    ) -> Outcome {
        let diagnostic = self.heap.diagnostic();
        let outcome = Outcome::AllocationExhausted(AllocationExhaustion {
            requested_block_bytes,
            total_free: diagnostic.total_free,
            largest_free_block: diagnostic.largest_free_block,
            collection_attempted,
        });
        self.lifecycle = Lifecycle::Terminal(outcome);
        outcome
    }

    fn resume_pending_allocation(
        &mut self,
        frame_index: usize,
        remaining: &mut u32,
    ) -> Option<Outcome> {
        let mut pending = self.pending_allocation.take()?;
        let destination = pending.state().destination;
        let collection_attempted = pending.state().collection_attempted;
        let index = match frame_index
            .checked_mul(self.image.registers_per_frame())
            .and_then(|base| base.checked_add(destination as usize))
            .filter(|index| *index < self.registers.len())
        {
            Some(index) => index,
            None => {
                let _ = pending.abort(&mut self.heap);
                return Some(self.fault(VmFault::InvalidStoragePlan));
            }
        };
        let expected_units = pending.units_for_budget(*remaining);
        let Some(consumed_dynamic_cost) = self
            .consumed_dynamic_cost
            .checked_add(u64::from(expected_units))
        else {
            let _ = pending.abort(&mut self.heap);
            return Some(self.fault(VmFault::AccountingOverflow));
        };
        let (used, published) = match pending.advance(&mut self.heap, *remaining) {
            Ok(result) => result,
            Err(fault) => {
                let _ = pending.abort(&mut self.heap);
                return Some(self.fault(fault));
            }
        };
        debug_assert_eq!(expected_units, used);
        *remaining -= used;
        self.consumed_dynamic_cost = consumed_dynamic_cost;
        let Some(reference) = published else {
            debug_assert!(!collection_attempted);
            self.pending_allocation = Some(pending);
            return Some(Outcome::SliceExhausted);
        };
        self.registers[index] = RegisterValue::Initialized(RuntimeValue::Reference(reference));
        self.frames[frame_index].instruction += 1;
        None
    }

    fn cancel_pending_allocation(&mut self) {
        if let Some(pending) = self.pending_allocation.take() {
            let _ = pending.abort(&mut self.heap);
        }
    }

    pub(super) fn consumed_fixed_cost(&self) -> u64 {
        self.consumed_fixed_cost
    }

    pub(super) fn consumed_dynamic_cost(&self) -> u64 {
        self.consumed_dynamic_cost
    }

    pub(super) fn trace_digest(&self) -> [u8; 32] {
        self.trace.clone().finalize().into()
    }

    pub(super) fn entered_blocks(&self) -> u64 {
        self.entered_blocks
    }

    pub(super) fn executed_instructions(&self) -> u64 {
        self.executed_instructions
    }

    fn trace_block_entry(
        &mut self,
        frame_index: usize,
        block_index: usize,
        remaining: u32,
    ) -> Result<(), RunError> {
        let function = self
            .image
            .function(self.frames[frame_index].function)
            .ok_or(RunError::NotRunnable)?;
        let local_block = block_index
            .checked_sub(function.first_block)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(RunError::NotRunnable)?;
        trace_field(&mut self.trace, &[1]);
        trace_field(&mut self.trace, &self.image.content_hash());
        trace_field(&mut self.trace, &function.key.module.to_le_bytes());
        trace_field(&mut self.trace, &function.key.function.to_le_bytes());
        trace_field(&mut self.trace, &local_block.to_le_bytes());
        trace_field(
            &mut self.trace,
            &u32::try_from(self.frame_depth)
                .map_err(|_| RunError::NotRunnable)?
                .to_le_bytes(),
        );
        trace_field(&mut self.trace, &remaining.to_le_bytes());
        trace_field(&mut self.trace, &self.consumed_fixed_cost.to_le_bytes());
        trace_field(&mut self.trace, &self.consumed_dynamic_cost.to_le_bytes());
        let width = self.image.registers_per_frame();
        for active_frame in 0..self.frame_depth {
            let active_function = self
                .image
                .function(self.frames[active_frame].function)
                .ok_or(RunError::NotRunnable)?;
            trace_field(
                &mut self.trace,
                &u32::try_from(active_function.register_count)
                    .map_err(|_| RunError::NotRunnable)?
                    .to_le_bytes(),
            );
            let base = active_frame
                .checked_mul(width)
                .ok_or(RunError::NotRunnable)?;
            for register in &self.registers[base..base + active_function.register_count] {
                trace_register(&mut self.trace, *register, &self.image)?;
            }
        }
        Ok(())
    }

    fn validate_argument(
        &self,
        parameter: u16,
        argument: EntryArgument,
        expected: ResolvedValueType,
    ) -> Result<(), RunError> {
        let value = argument.value;
        let primitive_matches = matches!(
            (expected.kind, value),
            (1, RuntimeValue::I32(_))
                | (2, RuntimeValue::I64(_))
                | (3, RuntimeValue::F32(_))
                | (4, RuntimeValue::F64(_))
                | (5, RuntimeValue::Bool(_))
                | (6, RuntimeValue::Char(_))
        );
        if primitive_matches {
            return Ok(());
        }
        match value {
            RuntimeValue::Null if expected.kind == 7 && expected.nullable => Ok(()),
            RuntimeValue::Reference(value) if expected.kind == 7 => {
                if argument.owner != Some(self.image.content_hash()) {
                    return Err(RunError::ForeignReference { parameter });
                }
                let admitted = self
                    .image
                    .host_reference(value)
                    .ok_or(RunError::DeadReference { parameter })?;
                if !admitted.live {
                    return Err(RunError::DeadReference { parameter });
                }
                let expected_type = expected.nominal.ok_or(RunError::EntryType { parameter })?;
                if admitted.assignable_to.contains(&expected_type) {
                    Ok(())
                } else {
                    Err(RunError::EntryType { parameter })
                }
            }
            _ => Err(RunError::EntryType { parameter }),
        }
    }

    pub(super) fn frame_depth(&self) -> usize {
        self.frame_depth
    }

    #[cfg(test)]
    pub(super) fn test_register(&self, register: usize) -> Option<RuntimeValue> {
        match self.registers.get(register) {
            Some(RegisterValue::Initialized(value)) => Some(*value),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn test_pending_initialized_bytes(&self) -> u32 {
        self.pending_allocation
            .map_or(0, PendingAllocation::initialized_bytes)
    }

    #[cfg(test)]
    pub(super) fn test_heap_diagnostic(&self) -> super::heap::HeapDiagnostic {
        self.heap.diagnostic()
    }

    #[cfg(test)]
    pub(super) fn test_managed_payload(
        &self,
        reference: super::value::ReferenceValue,
    ) -> Option<Box<[u8]>> {
        self.heap.test_managed_payload(reference)
    }

    #[cfg(test)]
    pub(super) fn test_cancel_pending(&mut self) -> Result<(), VmFault> {
        let Some(pending) = self.pending_allocation.take() else {
            return Ok(());
        };
        pending.abort(&mut self.heap)
    }

    #[cfg(test)]
    pub(super) fn test_snapshot(&self) -> (u8, usize, Box<[RegisterValue]>) {
        (
            match self.lifecycle {
                Lifecycle::Pristine => 0,
                Lifecycle::Runnable => 1,
                Lifecycle::Terminal(_) => 2,
            },
            self.frame_depth,
            self.registers.clone(),
        )
    }

    #[cfg(test)]
    pub(super) fn test_active_registers(&self) -> Box<[RegisterValue]> {
        let width = self.image.registers_per_frame();
        let base = self.frame_depth.saturating_sub(1) * width;
        self.registers[base..base + width].into()
    }

    #[cfg(test)]
    pub(super) fn maximum_observed_frame_depth_for_test(&self) -> usize {
        self.maximum_observed_frame_depth
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        self.cancel_pending_allocation();
    }
}

fn trace_field(trace: &mut Sha256, bytes: &[u8]) {
    trace.update((bytes.len() as u32).to_le_bytes());
    trace.update(bytes);
}

fn trace_register(
    trace: &mut Sha256,
    register: RegisterValue,
    image: &ExecutionImage,
) -> Result<(), RunError> {
    match register {
        RegisterValue::Uninitialized => trace_field(trace, &[0]),
        RegisterValue::Initialized(value) => {
            trace.update((2 + value.trace_payload_len()).to_le_bytes());
            trace.update([1, value.trace_tag()]);
            match value {
                RuntimeValue::I32(value) => trace.update(value.to_le_bytes()),
                RuntimeValue::I64(value) => trace.update(value.to_le_bytes()),
                RuntimeValue::F32(bits) => trace.update(bits.to_le_bytes()),
                RuntimeValue::F64(bits) => trace.update(bits.to_le_bytes()),
                RuntimeValue::Bool(value) => trace.update([u8::from(value)]),
                RuntimeValue::Char(value) => trace.update(value.to_le_bytes()),
                RuntimeValue::Null => {}
                RuntimeValue::Reference(value) => {
                    let ty = image.reference_type(value).ok_or(RunError::NotRunnable)?;
                    trace.update(ty.module.to_le_bytes());
                    trace.update(ty.ty.to_le_bytes());
                    trace.update(value.slot().to_le_bytes());
                    trace.update(value.generation().to_le_bytes());
                }
            }
        }
    }
    Ok(())
}

enum InstructionFailure {
    Trap(GuestTrap),
    Fault(VmFault),
}

fn execute_scalar(
    instruction: &ResolvedInstruction,
    registers: &mut [RegisterValue],
    register_types: &[ResolvedValueType],
    image: &ExecutionImage,
) -> Result<(), InstructionFailure> {
    let read = |register: u16| match registers.get(register as usize) {
        Some(RegisterValue::Initialized(value)) => Ok(*value),
        _ => Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
    };
    let write = |registers: &mut [RegisterValue], register: u16, value| {
        let slot = registers
            .get_mut(register as usize)
            .ok_or(InstructionFailure::Fault(VmFault::InvalidStoragePlan))?;
        *slot = RegisterValue::Initialized(value);
        Ok(())
    };
    macro_rules! binary {
        ($dst:expr, $lhs:expr, $rhs:expr, $body:expr) => {{
            let lhs = read(*$lhs)?;
            let rhs = read(*$rhs)?;
            let value = ($body)(lhs, rhs)?;
            write(registers, *$dst, value)
        }};
    }
    match instruction {
        ResolvedInstruction::Nop => Ok(()),
        ResolvedInstruction::Move { dst, src } => {
            let value = read(*src)?;
            write(registers, *dst, value)
        }
        ResolvedInstruction::Const { dst, constant } => write(
            registers,
            *dst,
            image
                .constant(*constant)
                .ok_or(InstructionFailure::Fault(VmFault::InvalidResolvedId))?,
        ),
        ResolvedInstruction::Null { dst } => write(registers, *dst, RuntimeValue::Null),
        ResolvedInstruction::Convert { dst, src } => {
            let destination = register_types
                .get(*dst as usize)
                .ok_or(InstructionFailure::Fault(VmFault::InvalidValueType))?
                .kind;
            let value = convert(read(*src)?, destination)?;
            write(registers, *dst, value)
        }
        ResolvedInstruction::Add {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| arithmetic(
            *form,
            a,
            b,
            Arithmetic::Add
        )),
        ResolvedInstruction::Sub {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| arithmetic(
            *form,
            a,
            b,
            Arithmetic::Sub
        )),
        ResolvedInstruction::Mul {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| arithmetic(
            *form,
            a,
            b,
            Arithmetic::Mul
        )),
        ResolvedInstruction::Div {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| arithmetic(
            *form,
            a,
            b,
            Arithmetic::Div
        )),
        ResolvedInstruction::Rem {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| arithmetic(
            *form,
            a,
            b,
            Arithmetic::Rem
        )),
        ResolvedInstruction::Neg { form, dst, src } => {
            let value = negate(*form, read(*src)?)?;
            write(registers, *dst, value)
        }
        ResolvedInstruction::BitAnd {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| integer_binary(*form, a, b, |x, y| x
            & y)),
        ResolvedInstruction::BitOr {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| integer_binary(*form, a, b, |x, y| x
            | y)),
        ResolvedInstruction::BitXor {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| integer_binary(*form, a, b, |x, y| x
            ^ y)),
        ResolvedInstruction::ShiftLeft {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| shift(*form, a, b, Shift::Left)),
        ResolvedInstruction::ShiftRight {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| shift(*form, a, b, Shift::Right)),
        ResolvedInstruction::ShiftUnsigned {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| shift(*form, a, b, Shift::Unsigned)),
        ResolvedInstruction::Equal {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| compare(
            *form,
            a,
            b,
            Comparison::Equal
        )),
        ResolvedInstruction::NotEqual {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| compare(
            *form,
            a,
            b,
            Comparison::NotEqual
        )),
        ResolvedInstruction::Less {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| compare(*form, a, b, Comparison::Less)),
        ResolvedInstruction::LessEqual {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| compare(
            *form,
            a,
            b,
            Comparison::LessEqual
        )),
        ResolvedInstruction::Greater {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| compare(
            *form,
            a,
            b,
            Comparison::Greater
        )),
        ResolvedInstruction::GreaterEqual {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(dst, lhs, rhs, |a, b| compare(
            *form,
            a,
            b,
            Comparison::GreaterEqual
        )),
        ResolvedInstruction::RefEqual { dst, lhs, rhs } => {
            binary!(dst, lhs, rhs, |a, b| Ok(RuntimeValue::Bool(a == b)))
        }
        ResolvedInstruction::RefNotEqual { dst, lhs, rhs } => {
            binary!(dst, lhs, rhs, |a, b| Ok(RuntimeValue::Bool(a != b)))
        }
        _ => Err(InstructionFailure::Fault(VmFault::UnsupportedInstruction)),
    }
}

#[derive(Clone, Copy)]
enum Arithmetic {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}
#[derive(Clone, Copy)]
enum Shift {
    Left,
    Right,
    Unsigned,
}
#[derive(Clone, Copy)]
enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

fn arithmetic(
    form: u8,
    lhs: RuntimeValue,
    rhs: RuntimeValue,
    operation: Arithmetic,
) -> Result<RuntimeValue, InstructionFailure> {
    match (form, lhs, rhs) {
        (1, RuntimeValue::I32(a), RuntimeValue::I32(b)) => Ok(RuntimeValue::I32(match operation {
            Arithmetic::Add => numeric::add_i32(a, b),
            Arithmetic::Sub => numeric::sub_i32(a, b),
            Arithmetic::Mul => numeric::mul_i32(a, b),
            Arithmetic::Div => numeric::div_i32(a, b).map_err(InstructionFailure::Trap)?,
            Arithmetic::Rem => numeric::rem_i32(a, b).map_err(InstructionFailure::Trap)?,
        })),
        (2, RuntimeValue::I64(a), RuntimeValue::I64(b)) => Ok(RuntimeValue::I64(match operation {
            Arithmetic::Add => numeric::add_i64(a, b),
            Arithmetic::Sub => numeric::sub_i64(a, b),
            Arithmetic::Mul => numeric::mul_i64(a, b),
            Arithmetic::Div => numeric::div_i64(a, b).map_err(InstructionFailure::Trap)?,
            Arithmetic::Rem => numeric::rem_i64(a, b).map_err(InstructionFailure::Trap)?,
        })),
        (3, RuntimeValue::F32(a), RuntimeValue::F32(b)) => {
            let (a, b) = (f32::from_bits(a), f32::from_bits(b));
            Ok(RuntimeValue::F32(
                match operation {
                    Arithmetic::Add => numeric::add_f32(a, b),
                    Arithmetic::Sub => numeric::sub_f32(a, b),
                    Arithmetic::Mul => numeric::mul_f32(a, b),
                    Arithmetic::Div => numeric::div_f32(a, b),
                    Arithmetic::Rem => numeric::rem_f32(a, b),
                }
                .to_bits(),
            ))
        }
        (4, RuntimeValue::F64(a), RuntimeValue::F64(b)) => {
            let (a, b) = (f64::from_bits(a), f64::from_bits(b));
            Ok(RuntimeValue::F64(
                match operation {
                    Arithmetic::Add => numeric::add_f64(a, b),
                    Arithmetic::Sub => numeric::sub_f64(a, b),
                    Arithmetic::Mul => numeric::mul_f64(a, b),
                    Arithmetic::Div => numeric::div_f64(a, b),
                    Arithmetic::Rem => numeric::rem_f64(a, b),
                }
                .to_bits(),
            ))
        }
        _ => Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
    }
}

fn negate(form: u8, value: RuntimeValue) -> Result<RuntimeValue, InstructionFailure> {
    match (form, value) {
        (1, RuntimeValue::I32(v)) => Ok(RuntimeValue::I32(numeric::neg_i32(v))),
        (2, RuntimeValue::I64(v)) => Ok(RuntimeValue::I64(numeric::neg_i64(v))),
        (3, RuntimeValue::F32(v)) => Ok(RuntimeValue::F32(
            numeric::neg_f32(f32::from_bits(v)).to_bits(),
        )),
        (4, RuntimeValue::F64(v)) => Ok(RuntimeValue::F64(
            numeric::neg_f64(f64::from_bits(v)).to_bits(),
        )),
        _ => Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
    }
}

fn integer_binary(
    form: u8,
    lhs: RuntimeValue,
    rhs: RuntimeValue,
    op: impl Fn(i64, i64) -> i64,
) -> Result<RuntimeValue, InstructionFailure> {
    match (form, lhs, rhs) {
        (1, RuntimeValue::I32(a), RuntimeValue::I32(b)) => {
            Ok(RuntimeValue::I32(op(a as i64, b as i64) as i32))
        }
        (2, RuntimeValue::I64(a), RuntimeValue::I64(b)) => Ok(RuntimeValue::I64(op(a, b))),
        _ => Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
    }
}
fn shift(
    form: u8,
    lhs: RuntimeValue,
    rhs: RuntimeValue,
    op: Shift,
) -> Result<RuntimeValue, InstructionFailure> {
    match (form, lhs, rhs) {
        (1, RuntimeValue::I32(a), RuntimeValue::I32(b)) => Ok(RuntimeValue::I32(match op {
            Shift::Left => numeric::shl_i32(a, b),
            Shift::Right => numeric::shr_i32(a, b),
            Shift::Unsigned => numeric::ushr_i32(a, b),
        })),
        (2, RuntimeValue::I64(a), RuntimeValue::I32(b)) => Ok(RuntimeValue::I64(match op {
            Shift::Left => numeric::shl_i64(a, b),
            Shift::Right => numeric::shr_i64(a, b),
            Shift::Unsigned => numeric::ushr_i64(a, b),
        })),
        _ => Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
    }
}

fn compare(
    form: u8,
    lhs: RuntimeValue,
    rhs: RuntimeValue,
    op: Comparison,
) -> Result<RuntimeValue, InstructionFailure> {
    let result = match (form, lhs, rhs) {
        (1, RuntimeValue::I32(a), RuntimeValue::I32(b)) => ordered(a, b, op),
        (2, RuntimeValue::I64(a), RuntimeValue::I64(b)) => ordered(a, b, op),
        (3, RuntimeValue::F32(a), RuntimeValue::F32(b)) => {
            float_compare_f32(f32::from_bits(a), f32::from_bits(b), op)
        }
        (4, RuntimeValue::F64(a), RuntimeValue::F64(b)) => {
            float_compare_f64(f64::from_bits(a), f64::from_bits(b), op)
        }
        (5, RuntimeValue::Bool(a), RuntimeValue::Bool(b)) => match op {
            Comparison::Equal => a == b,
            Comparison::NotEqual => a != b,
            _ => return Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
        },
        (6, RuntimeValue::Char(a), RuntimeValue::Char(b)) => ordered(a, b, op),
        _ => return Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
    };
    Ok(RuntimeValue::Bool(result))
}
fn ordered<T: Ord>(a: T, b: T, op: Comparison) -> bool {
    match op {
        Comparison::Equal => a == b,
        Comparison::NotEqual => a != b,
        Comparison::Less => a < b,
        Comparison::LessEqual => a <= b,
        Comparison::Greater => a > b,
        Comparison::GreaterEqual => a >= b,
    }
}
fn float_compare_f32(a: f32, b: f32, op: Comparison) -> bool {
    match op {
        Comparison::Equal => numeric::eq_f32(a, b),
        Comparison::NotEqual => numeric::ne_f32(a, b),
        Comparison::Less => numeric::lt_f32(a, b),
        Comparison::LessEqual => numeric::le_f32(a, b),
        Comparison::Greater => numeric::gt_f32(a, b),
        Comparison::GreaterEqual => numeric::ge_f32(a, b),
    }
}
fn float_compare_f64(a: f64, b: f64, op: Comparison) -> bool {
    match op {
        Comparison::Equal => numeric::eq_f64(a, b),
        Comparison::NotEqual => numeric::ne_f64(a, b),
        Comparison::Less => numeric::lt_f64(a, b),
        Comparison::LessEqual => numeric::le_f64(a, b),
        Comparison::Greater => numeric::gt_f64(a, b),
        Comparison::GreaterEqual => numeric::ge_f64(a, b),
    }
}

fn convert(value: RuntimeValue, destination: u8) -> Result<RuntimeValue, InstructionFailure> {
    match (value, destination) {
        (RuntimeValue::I32(v), 1) => Ok(RuntimeValue::I32(v)),
        (RuntimeValue::I32(v), 2) => Ok(RuntimeValue::I64(numeric::i32_to_i64(v))),
        (RuntimeValue::I32(v), 3) => Ok(RuntimeValue::F32(numeric::i32_to_f32(v).to_bits())),
        (RuntimeValue::I32(v), 4) => Ok(RuntimeValue::F64(numeric::i32_to_f64(v).to_bits())),
        (RuntimeValue::I32(v), 6) => Ok(RuntimeValue::Char(numeric::i32_to_char(v))),
        (RuntimeValue::I64(v), 1) => Ok(RuntimeValue::I32(numeric::i64_to_i32(v))),
        (RuntimeValue::I64(v), 2) => Ok(RuntimeValue::I64(v)),
        (RuntimeValue::I64(v), 3) => Ok(RuntimeValue::F32(numeric::i64_to_f32(v).to_bits())),
        (RuntimeValue::I64(v), 4) => Ok(RuntimeValue::F64(numeric::i64_to_f64(v).to_bits())),
        (RuntimeValue::F32(v), 1) => Ok(RuntimeValue::I32(numeric::f32_to_i32(f32::from_bits(v)))),
        (RuntimeValue::F32(v), 2) => Ok(RuntimeValue::I64(numeric::f32_to_i64(f32::from_bits(v)))),
        (RuntimeValue::F32(v), 3) => Ok(RuntimeValue::F32(v)),
        (RuntimeValue::F32(v), 4) => Ok(RuntimeValue::F64(
            numeric::f32_to_f64(f32::from_bits(v)).to_bits(),
        )),
        (RuntimeValue::F64(v), 1) => Ok(RuntimeValue::I32(numeric::f64_to_i32(f64::from_bits(v)))),
        (RuntimeValue::F64(v), 2) => Ok(RuntimeValue::I64(numeric::f64_to_i64(f64::from_bits(v)))),
        (RuntimeValue::F64(v), 3) => Ok(RuntimeValue::F32(
            numeric::f64_to_f32(f64::from_bits(v)).to_bits(),
        )),
        (RuntimeValue::F64(v), 4) => Ok(RuntimeValue::F64(v)),
        (RuntimeValue::Char(v), 1) => Ok(RuntimeValue::I32(numeric::char_to_i32(v))),
        _ => Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
    }
}
