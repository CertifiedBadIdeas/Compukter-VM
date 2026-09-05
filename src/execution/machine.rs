use super::{
    error::{
        AdmissionError, AllocationDiagnostic, AllocationExhaustion, AllocationRequestKind,
        AllocationSource, EntryArgumentLimit, GuestTrap, Outcome, RunError, VmFault,
    },
    external_roots::ExternalRootTable,
    frame::{FrameArena, FrameReservation},
    gc::{Collector, RootSet},
    heap::{AllocationRequest, Heap},
    heap_ops::{load_value, store_value, PendingAllocation, PendingState},
    host::EntryArgumentLimits,
    image::{ExecutionImage, ResolvedFunction, ResolvedInstruction, ResolvedValueType},
    layout::{array_layout, RuntimeTypeLayout, ValueWidth},
    numeric, text,
    value::{EntryArgument, Ref32, ReferenceDomain, RegisterValue, RuntimeValue},
    TypeKey,
};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Lifecycle {
    Pristine,
    Runnable,
    Terminal(Outcome),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypeInitializationState {
    Uninitialized,
    Initializing,
    Initialized,
    Failed,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Frame {
    pub(super) function: usize,
    pub(super) base: u32,
    pub(super) byte_len: u32,
    pub(super) block: usize,
    pub(super) instruction: usize,
    pub(super) caller_block: usize,
    pub(super) caller_instruction: usize,
    pub(super) destination: u16,
    pub(super) initializer: Option<TypeKey>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CapabilitySuspension<'a> {
    pub capability: u32,
    pub operation: u32,
    pub arguments: &'a [u16],
}

impl Frame {
    const EMPTY: Self = Self {
        function: usize::MAX,
        base: 0,
        byte_len: 0,
        block: usize::MAX,
        instruction: 0,
        caller_block: usize::MAX,
        caller_instruction: 0,
        destination: u16::MAX,
        initializer: None,
    };

    #[cfg(test)]
    pub(super) const fn test_entry(function: usize) -> Self {
        Self {
            function,
            base: 0,
            byte_len: 0,
            block: 0,
            instruction: 0,
            caller_block: usize::MAX,
            caller_instruction: 0,
            destination: u16::MAX,
            initializer: None,
        }
    }
}

pub(super) fn read_frame_value(
    arena: &FrameArena,
    frame: Frame,
    function: &ResolvedFunction,
    register: u16,
) -> Result<RuntimeValue, VmFault> {
    let register = register as usize;
    let value_type = *function
        .registers
        .get(register)
        .ok_or(VmFault::InvalidStoragePlan)?;
    match value_type.kind {
        1 => arena
            .read_i32(frame.base, &function.frame_layout, register, 0)
            .map(RuntimeValue::I32),
        2 => arena
            .read_i64(frame.base, &function.frame_layout, register, 0)
            .map(RuntimeValue::I64),
        3 => arena
            .read_f32(frame.base, &function.frame_layout, register, 0)
            .map(RuntimeValue::F32),
        4 => arena
            .read_f64(frame.base, &function.frame_layout, register, 0)
            .map(RuntimeValue::F64),
        5 => arena
            .read_i32(frame.base, &function.frame_layout, register, 0)
            .and_then(|value| match value {
                0 => Ok(RuntimeValue::Bool(false)),
                1 => Ok(RuntimeValue::Bool(true)),
                _ => Err(VmFault::InvalidValueType),
            }),
        6 => arena
            .read_i32(frame.base, &function.frame_layout, register, 0)
            .and_then(|value| {
                u16::try_from(value)
                    .map(RuntimeValue::Char)
                    .map_err(|_| VmFault::InvalidValueType)
            }),
        7 => arena
            .read_ref32(frame.base, &function.frame_layout, register, 0)
            .map(|value| value.map_or(RuntimeValue::Null, RuntimeValue::Reference)),
        _ => Err(VmFault::InvalidValueType),
    }
}

pub(super) fn write_frame_value(
    arena: &mut FrameArena,
    frame: Frame,
    function: &ResolvedFunction,
    register: u16,
    value: RuntimeValue,
) -> Result<(), VmFault> {
    let register = register as usize;
    let value_type = *function
        .registers
        .get(register)
        .ok_or(VmFault::InvalidStoragePlan)?;
    match (value_type.kind, value) {
        (1, RuntimeValue::I32(value)) => {
            arena.write_i32(frame.base, &function.frame_layout, register, 0, value)
        }
        (2, RuntimeValue::I64(value)) => {
            arena.write_i64(frame.base, &function.frame_layout, register, 0, value)
        }
        (3, RuntimeValue::F32(value)) => {
            arena.write_f32(frame.base, &function.frame_layout, register, 0, value)
        }
        (4, RuntimeValue::F64(value)) => {
            arena.write_f64(frame.base, &function.frame_layout, register, 0, value)
        }
        (5, RuntimeValue::Bool(value)) => arena.write_i32(
            frame.base,
            &function.frame_layout,
            register,
            0,
            i32::from(value),
        ),
        (6, RuntimeValue::Char(value)) => arena.write_i32(
            frame.base,
            &function.frame_layout,
            register,
            0,
            i32::from(value),
        ),
        (7, RuntimeValue::Null) => {
            arena.write_ref32(frame.base, &function.frame_layout, register, 0, None)
        }
        (7, RuntimeValue::Reference(value)) => {
            arena.write_ref32(frame.base, &function.frame_layout, register, 0, Some(value))
        }
        _ => Err(VmFault::InvalidValueType),
    }
}

pub(super) struct Machine {
    image: ExecutionImage,
    lifecycle: Lifecycle,
    frames: Box<[Frame]>,
    frame_arena: FrameArena,
    static_slots: Box<[RuntimeValue]>,
    type_initialization: Box<[TypeInitializationState]>,
    heap: Heap,
    external_roots: ExternalRootTable,
    collector: Collector,
    allocation_retry: Option<AllocationRetry>,
    pending_allocation: Option<PendingAllocation>,
    pending_text: Option<text::PendingText>,
    pending_concat: Option<text::PendingConcat>,
    pending_concat_source: Option<AllocationSource>,
    pending_host_string: Option<text::PendingHostString>,
    pending_host_string_source: Option<AllocationSource>,
    string_collection_pending: Option<StringCollectionTarget>,
    emergency_oom: Option<super::value::Ref32>,
    frame_depth: usize,
    consumed_fixed_cost: u64,
    consumed_dynamic_cost: u64,
    consumed_maintenance_cost: u64,
    entered_blocks: u64,
    executed_instructions: u64,
    maximum_observed_frame_depth: usize,
    trace: Sha256,
}

#[derive(Clone, Copy, Debug)]
enum AllocationShape {
    Object,
    Array { length: u32 },
}

impl AllocationShape {
    const fn request_kind(self) -> AllocationRequestKind {
        match self {
            Self::Object => AllocationRequestKind::Object,
            Self::Array { .. } => AllocationRequestKind::Array,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct AllocationRetry {
    request: AllocationRequest,
    destination: u16,
    logical_bytes: u32,
    shape: AllocationShape,
    source: AllocationSource,
}

#[derive(Clone, Copy, Debug)]
enum StringCollectionTarget {
    Concat,
    HostResponse,
}

impl AllocationRetry {
    fn reserve(self, heap: &mut Heap) -> Result<Option<PendingAllocation>, VmFault> {
        let reservation = heap.reserve(self.request)?;
        let Some(reservation) = reservation else {
            return Ok(None);
        };
        let state = PendingState {
            request: self.request,
            reservation,
            destination: self.destination,
            logical_bytes: self.logical_bytes,
            initialized_bytes: 0,
            fixed_cost_paid: true,
            collection_attempted: true,
        };
        Ok(Some(match self.shape {
            AllocationShape::Object => PendingAllocation::Object(state),
            AllocationShape::Array { length } => PendingAllocation::Array { state, length },
        }))
    }
}

impl Machine {
    pub(super) fn new(image: ExecutionImage) -> Result<Self, AdmissionError> {
        let heap = Heap::new(&image.storage_plan())?;
        let external_roots = ExternalRootTable::new(image.external_root_capacity())?;
        let frame_count = image.maximum_call_depth();
        let largest_frame = image
            .functions()
            .iter()
            .map(|function| u64::from(function.frame_layout.byte_len).saturating_add(7) & !7)
            .max()
            .unwrap_or(0);
        let frame_arena_bytes = largest_frame
            .checked_mul(frame_count as u64)
            .and_then(|bytes| u32::try_from(bytes).ok())
            .ok_or(AdmissionError::StoragePlanOverflow)?;
        let frame_arena =
            FrameArena::new(frame_arena_bytes).map_err(|_| AdmissionError::AllocationFailed)?;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(frame_count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        frames.resize(frame_count, Frame::EMPTY);
        let static_count = usize::try_from(image.storage_plan().static_slot_count)
            .map_err(|_| AdmissionError::StoragePlanOverflow)?;
        let mut static_slots = Vec::new();
        static_slots
            .try_reserve_exact(static_count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        static_slots.resize(static_count, RuntimeValue::Null);
        for field in image.fields() {
            if let Some(slot) = field.static_slot {
                let value = zero_value(field.value_type)?;
                *static_slots
                    .get_mut(slot as usize)
                    .ok_or(AdmissionError::InvalidEntry)? = value;
            }
        }
        let mut type_initialization = Vec::new();
        type_initialization
            .try_reserve_exact(image.type_count())
            .map_err(|_| AdmissionError::AllocationFailed)?;
        for index in 0..image.type_count() {
            let key = image.type_key(index).ok_or(AdmissionError::InvalidEntry)?;
            type_initialization.push(if image.is_class(key) {
                TypeInitializationState::Uninitialized
            } else {
                TypeInitializationState::Initialized
            });
        }
        Ok(Self {
            image,
            lifecycle: Lifecycle::Pristine,
            frames: frames.into_boxed_slice(),
            frame_arena,
            static_slots: static_slots.into_boxed_slice(),
            type_initialization: type_initialization.into_boxed_slice(),
            heap,
            external_roots,
            collector: Collector::new(),
            allocation_retry: None,
            pending_allocation: None,
            pending_text: None,
            pending_concat: None,
            pending_concat_source: None,
            pending_host_string: None,
            pending_host_string_source: None,
            string_collection_pending: None,
            emergency_oom: Some(super::value::Ref32::reserved(0).unwrap()),
            frame_depth: 0,
            consumed_fixed_cost: 0,
            consumed_dynamic_cost: 0,
            consumed_maintenance_cost: 0,
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
        let reservation = self
            .frame_arena
            .push(&entry.frame_layout)
            .map_err(|_| RunError::NotRunnable)?;
        self.frames[0] = Frame {
            function: entry_index,
            base: reservation.base,
            byte_len: reservation.byte_len,
            block: entry.first_block,
            instruction: 0,
            caller_block: usize::MAX,
            caller_instruction: 0,
            destination: u16::MAX,
            initializer: None,
        };
        for (parameter, argument) in arguments.iter().enumerate() {
            if write_frame_value(
                &mut self.frame_arena,
                self.frames[0],
                entry,
                parameter as u16,
                argument.value,
            )
            .is_err()
            {
                self.frames[0] = Frame::EMPTY;
                let _ = self.frame_arena.pop(reservation);
                return Err(RunError::NotRunnable);
            }
        }
        self.frame_depth = 1;
        self.maximum_observed_frame_depth = 1;
        self.lifecycle = Lifecycle::Runnable;
        Ok(())
    }

    pub(super) fn materialize_entry_string_array(
        &mut self,
        arguments: &[Box<[u16]>],
        limits: EntryArgumentLimits,
    ) -> Result<EntryArgument, RunError> {
        if self.lifecycle != Lifecycle::Pristine {
            return Err(RunError::AlreadyStarted);
        }
        let count = u32::try_from(arguments.len())
            .map_err(|_| RunError::EntryArgumentLimit(EntryArgumentLimit::Count))?;
        if count > limits.maximum_count || count > i32::MAX as u32 {
            return Err(RunError::EntryArgumentLimit(EntryArgumentLimit::Count));
        }
        let mut total = 0_u32;
        for argument in arguments {
            let length = u32::try_from(argument.len())
                .map_err(|_| RunError::EntryArgumentLimit(EntryArgumentLimit::ArgumentCodeUnits))?;
            if length > limits.maximum_code_units_per_argument {
                return Err(RunError::EntryArgumentLimit(
                    EntryArgumentLimit::ArgumentCodeUnits,
                ));
            }
            total = total
                .checked_add(length)
                .ok_or(RunError::EntryArgumentLimit(
                    EntryArgumentLimit::TotalCodeUnits,
                ))?;
            if total > limits.maximum_total_code_units {
                return Err(RunError::EntryArgumentLimit(
                    EntryArgumentLimit::TotalCodeUnits,
                ));
            }
        }

        let entry = self
            .image
            .function(self.image.entry_index())
            .ok_or(RunError::NotRunnable)?;
        let array_type = entry
            .registers
            .first()
            .and_then(|value| value.nominal)
            .ok_or(RunError::NotRunnable)?;
        if !matches!(
            self.image.type_layout(array_type),
            Some(RuntimeTypeLayout::Array {
                element: ValueWidth::Ref
            })
        ) {
            return Err(RunError::NotRunnable);
        }
        let string_type = self.image.string_type().ok_or(RunError::NotRunnable)?;
        if self
            .image
            .array_element_type(array_type)
            .and_then(|value| value.nominal)
            != Some(string_type)
        {
            return Err(RunError::NotRunnable);
        }

        let mut strings = Vec::new();
        strings
            .try_reserve_exact(arguments.len())
            .map_err(|_| RunError::EntryAllocationFailed)?;
        for argument in arguments {
            let mut pending = text::PendingHostString::new(u16::MAX);
            let reference = loop {
                match pending.resume(&self.image, &mut self.heap, argument, u32::MAX) {
                    Ok((_, Some((_, RuntimeValue::Reference(reference))))) => break reference,
                    Ok((_, Some(_))) => {
                        self.rollback_entry_references(&strings);
                        return Err(RunError::NotRunnable);
                    }
                    Ok((_, None)) => continue,
                    Err(_) => {
                        let _ = pending.abort(&mut self.heap);
                        self.rollback_entry_references(&strings);
                        return Err(RunError::EntryAllocationFailed);
                    }
                }
            };
            strings.push(reference);
        }

        let layout = array_layout(ValueWidth::Ref, count as i32).map_err(|_| {
            self.rollback_entry_references(&strings);
            RunError::EntryAllocationFailed
        })?;
        let reservation = match self.heap.reserve(AllocationRequest {
            block_bytes: layout.block_bytes,
            type_id: self
                .image
                .type_id(array_type)
                .ok_or(RunError::NotRunnable)?,
        }) {
            Ok(Some(reservation)) => reservation,
            _ => {
                self.rollback_entry_references(&strings);
                return Err(RunError::EntryAllocationFailed);
            }
        };
        let initialize = (|| {
            self.heap
                .zero_reserved_payload(reservation, 0, layout.block_bytes - 24)?;
            self.heap.write_reserved_u32(reservation, 0, count)?;
            for (index, reference) in strings.iter().copied().enumerate() {
                let offset = 8_u32
                    .checked_add((index as u32).checked_mul(4).ok_or(VmFault::CorruptHeap)?)
                    .ok_or(VmFault::CorruptHeap)?;
                self.heap.write_reserved(
                    reservation,
                    offset,
                    &reference.to_bits().to_le_bytes(),
                )?;
            }
            self.heap.commit(reservation)
        })();
        let array = match initialize {
            Ok(reference) => reference,
            Err(_) => {
                let _ = self.heap.abort(reservation);
                self.rollback_entry_references(&strings);
                return Err(RunError::EntryAllocationFailed);
            }
        };
        Ok(EntryArgument::owned(
            self.image.content_hash(),
            RuntimeValue::Reference(array),
        ))
    }

    fn rollback_entry_references(&mut self, references: &[Ref32]) {
        for reference in references.iter().rev().copied() {
            let _ = self.heap.free(reference);
        }
    }

    pub(super) fn run_slice(
        &mut self,
        guest_budget: u32,
        maintenance_budget: u32,
    ) -> Result<Outcome, RunError> {
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
        if guest_budget == 0
            || guest_budget < minimum
            || guest_budget > self.image.maximum_slice_budget()
        {
            return Err(RunError::InvalidSliceBudget {
                minimum,
                maximum: self.image.maximum_slice_budget(),
                supplied: guest_budget,
            });
        }
        if maintenance_budget > self.image.maximum_slice_budget() {
            return Err(RunError::InvalidSliceBudget {
                minimum: 0,
                maximum: self.image.maximum_slice_budget(),
                supplied: maintenance_budget,
            });
        }
        if self.collector.is_active() {
            return self.run_maintenance(maintenance_budget);
        }
        let mut remaining = guest_budget;
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
            if self.pending_text.is_some() {
                if let Some(outcome) = self.resume_pending_text(frame_index, &mut remaining) {
                    return Ok(outcome);
                }
            }
            if self.pending_concat.is_some() {
                if let Some(outcome) = self.resume_pending_concat(frame_index, &mut remaining) {
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
                let active_type = match &self
                    .image
                    .block(block_index)
                    .ok_or(RunError::NotRunnable)?
                    .instructions[instruction_index]
                {
                    ResolvedInstruction::StaticGet { field, .. }
                    | ResolvedInstruction::StaticSet { field, .. } => Some(field.owner),
                    ResolvedInstruction::NewObject { ty, .. } => Some(*ty),
                    ResolvedInstruction::CallDirect { target, .. } => self
                        .image
                        .function(*target)
                        .and_then(|function| function.static_owner),
                    _ => None,
                };
                if let Some(ty) = active_type {
                    match self.ensure_type_initialized(ty, frame_index) {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(outcome) => return Ok(outcome),
                    }
                }
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

                        let continuation_block = self.frames[frame_index].caller_block;
                        let continuation_instruction = self.frames[frame_index].caller_instruction;
                        let destination = self.frames[frame_index].destination;
                        let initialized_type = self.frames[frame_index].initializer;
                        if (destination == u16::MAX) != returned.is_none() {
                            return Ok(self.fault(VmFault::InvalidValueType));
                        }
                        let callee = self.frames[frame_index];
                        if let Err(fault) = self.frame_arena.pop(FrameReservation {
                            base: callee.base,
                            byte_len: callee.byte_len,
                        }) {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index] = Frame::EMPTY;
                        self.frame_depth = frame_index;
                        let caller_index = frame_index - 1;
                        self.frames[caller_index].block = continuation_block;
                        self.frames[caller_index].instruction = continuation_instruction;
                        if let Some(ty) = initialized_type {
                            let Some(index) = self.image.type_index(ty) else {
                                return Ok(self.fault(VmFault::InvalidResolvedId));
                            };
                            self.type_initialization[index] = TypeInitializationState::Initialized;
                        }
                        if let Some(value) = returned {
                            if let Err(fault) =
                                self.write_register(caller_index, destination, value)
                            {
                                return Ok(self.fault(fault));
                            }
                        }
                        break;
                    }
                    ResolvedInstruction::CallDirect { .. }
                    | ResolvedInstruction::CallSuspend { .. } => {
                        let (dst, target, args, caller_block, caller_instruction) =
                            match instruction {
                                ResolvedInstruction::CallDirect { dst, target, args } => (
                                    *dst,
                                    *target,
                                    args.as_ref(),
                                    block_index,
                                    instruction_index + 1,
                                ),
                                ResolvedInstruction::CallSuspend {
                                    dst,
                                    target,
                                    args,
                                    resume_block,
                                } => (*dst, *target, args.as_ref(), *resume_block, 0),
                                _ => unreachable!(),
                            };
                        let Some(target_function) = self.image.function(target) else {
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
                        let reservation = match self.frame_arena.push(&target_function.frame_layout)
                        {
                            Ok(reservation) => reservation,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let callee = Frame {
                            function: target,
                            base: reservation.base,
                            byte_len: reservation.byte_len,
                            block: target_function.first_block,
                            instruction: 0,
                            caller_block,
                            caller_instruction,
                            destination: dst,
                            initializer: None,
                        };
                        for (parameter, source) in args.iter().enumerate() {
                            let value = match self.read_register(frame_index, *source) {
                                Ok(value) => value,
                                Err(fault) => {
                                    let _ = self.frame_arena.pop(reservation);
                                    return Ok(self.fault(fault));
                                }
                            };
                            if let Err(fault) = write_frame_value(
                                &mut self.frame_arena,
                                callee,
                                target_function,
                                parameter as u16,
                                value,
                            ) {
                                let _ = self.frame_arena.pop(reservation);
                                return Ok(self.fault(fault));
                            }
                        }
                        self.frames[callee_index] = callee;
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
                            type_id: self.image.type_id(*ty).ok_or(RunError::NotRunnable)?,
                        };
                        let logical_bytes = layout.payload_bytes;
                        let reservation = match self.heap.reserve(request) {
                            Ok(Some(reservation)) => reservation,
                            Ok(None) => {
                                let retry = AllocationRetry {
                                    request,
                                    destination: *dst,
                                    logical_bytes,
                                    shape: AllocationShape::Object,
                                    source: self.allocation_source(frame_index),
                                };
                                return self.start_collection(retry, maintenance_budget);
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
                    ResolvedInstruction::StringHash { dst, string } => {
                        let value = match self.read_register(frame_index, *string) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        self.pending_text =
                            match text::PendingText::hash(&self.image, &self.heap, value, *dst) {
                                Ok(pending) => Some(pending),
                                Err(error) => return Ok(self.text_outcome(error)),
                            };
                        if let Some(outcome) = self.resume_pending_text(frame_index, &mut remaining)
                        {
                            return Ok(outcome);
                        }
                    }
                    ResolvedInstruction::StringEquals { dst, lhs, rhs } => {
                        let lhs = match self.read_register(frame_index, *lhs) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let rhs = match self.read_register(frame_index, *rhs) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        self.pending_text = match text::PendingText::equals(
                            &self.image,
                            &self.heap,
                            lhs,
                            rhs,
                            *dst,
                        ) {
                            Ok(pending) => Some(pending),
                            Err(error) => return Ok(self.text_outcome(error)),
                        };
                        if let Some(outcome) = self.resume_pending_text(frame_index, &mut remaining)
                        {
                            return Ok(outcome);
                        }
                    }
                    ResolvedInstruction::StringCompare { dst, lhs, rhs } => {
                        let lhs = match self.read_register(frame_index, *lhs) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let rhs = match self.read_register(frame_index, *rhs) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        self.pending_text = match text::PendingText::compare(
                            &self.image,
                            &self.heap,
                            lhs,
                            rhs,
                            *dst,
                        ) {
                            Ok(pending) => Some(pending),
                            Err(error) => return Ok(self.text_outcome(error)),
                        };
                        if let Some(outcome) = self.resume_pending_text(frame_index, &mut remaining)
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
                            Err(_) => {
                                return Ok(self.allocation_exhausted(
                                    AllocationRequestKind::Array,
                                    u32::MAX,
                                    false,
                                    self.allocation_source(frame_index),
                                ));
                            }
                        };
                        let request = AllocationRequest {
                            block_bytes: layout.block_bytes,
                            type_id: self.image.type_id(*ty).ok_or(RunError::NotRunnable)?,
                        };
                        if request.block_bytes > self.image.storage_plan().heap_bytes {
                            return Ok(self.allocation_exhausted(
                                AllocationRequestKind::Array,
                                layout.payload_bytes,
                                false,
                                self.allocation_source(frame_index),
                            ));
                        }
                        let reservation = match self.heap.reserve(request) {
                            Ok(Some(reservation)) => reservation,
                            Ok(None) => {
                                let retry = AllocationRetry {
                                    request,
                                    destination: *dst,
                                    logical_bytes: layout.payload_bytes,
                                    shape: AllocationShape::Array {
                                        length: layout.length,
                                    },
                                    source: self.allocation_source(frame_index),
                                };
                                return self.start_collection(retry, maintenance_budget);
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
                    ResolvedInstruction::StringConcat { dst, lhs, rhs } => {
                        let lhs = match self.read_register(frame_index, *lhs) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let rhs = match self.read_register(frame_index, *rhs) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let pending =
                            match text::PendingConcat::new(&self.image, &self.heap, lhs, rhs, *dst)
                            {
                                Ok(pending) => pending,
                                Err(error) => return Ok(self.text_outcome(error)),
                            };
                        self.pending_concat_source = Some(self.allocation_source(frame_index));
                        self.pending_concat = Some(pending);
                        if let Some(outcome) =
                            self.resume_pending_concat(frame_index, &mut remaining)
                        {
                            return Ok(outcome);
                        }
                    }
                    ResolvedInstruction::StringValueOf { form, dst, source } => {
                        let value = match self.read_register(frame_index, *source) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let pending = match text::PendingConcat::scalar(value, *form, *dst) {
                            Ok(pending) => pending,
                            Err(error) => return Ok(self.text_outcome(error)),
                        };
                        self.pending_concat_source = Some(self.allocation_source(frame_index));
                        self.pending_concat = Some(pending);
                        if let Some(outcome) =
                            self.resume_pending_concat(frame_index, &mut remaining)
                        {
                            return Ok(outcome);
                        }
                    }
                    ResolvedInstruction::StringSubstring {
                        dst,
                        string,
                        start,
                        end,
                    } => {
                        let value = match self.read_register(frame_index, *string) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let start = match self.read_register(frame_index, *start) {
                            Ok(RuntimeValue::I32(value)) => value,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let end = match self.read_register(frame_index, *end) {
                            Ok(RuntimeValue::I32(value)) => value,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        match text::PendingConcat::substring(
                            &self.image,
                            &self.heap,
                            value,
                            start,
                            end,
                            *dst,
                        ) {
                            Ok(text::SubstringPlan::Identity(value)) => {
                                if let Err(fault) = self.write_register(frame_index, *dst, value) {
                                    return Ok(self.fault(fault));
                                }
                                self.frames[frame_index].instruction += 1;
                            }
                            Ok(text::SubstringPlan::Build(pending)) => {
                                self.pending_concat_source =
                                    Some(self.allocation_source(frame_index));
                                self.pending_concat = Some(pending);
                                if let Some(outcome) =
                                    self.resume_pending_concat(frame_index, &mut remaining)
                                {
                                    return Ok(outcome);
                                }
                            }
                            Ok(text::SubstringPlan::Empty) => {
                                let Some(value) = self.image.empty_string() else {
                                    return Ok(self.fault(VmFault::InvalidResolvedId));
                                };
                                if let Err(fault) = self.write_register(frame_index, *dst, value) {
                                    return Ok(self.fault(fault));
                                }
                                self.frames[frame_index].instruction += 1;
                            }
                            Err(error) => return Ok(self.text_outcome(error)),
                        }
                    }
                    ResolvedInstruction::StringFromCharArray {
                        dst,
                        array,
                        start,
                        end,
                    } => {
                        let array = match self.read_register(frame_index, *array) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let start = match self.read_register(frame_index, *start) {
                            Ok(RuntimeValue::I32(value)) => value,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let end = match self.read_register(frame_index, *end) {
                            Ok(RuntimeValue::I32(value)) => value,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let (reference, _, element, length) = match self.resolve_array(array) {
                            Ok(array) => array,
                            Err(InstructionFailure::Trap(trap)) => {
                                let outcome = Outcome::Crashed(trap);
                                self.lifecycle = Lifecycle::Terminal(outcome);
                                return Ok(outcome);
                            }
                            Err(InstructionFailure::Fault(fault)) => return Ok(self.fault(fault)),
                        };
                        if element != ValueWidth::Char {
                            return Ok(self.fault(VmFault::InvalidReference));
                        }
                        let pending = match text::PendingConcat::char_array(
                            reference, length, start, end, *dst,
                        ) {
                            Ok(pending) => pending,
                            Err(error) => return Ok(self.text_outcome(error)),
                        };
                        self.pending_concat_source = Some(self.allocation_source(frame_index));
                        self.pending_concat = Some(pending);
                        if let Some(outcome) =
                            self.resume_pending_concat(frame_index, &mut remaining)
                        {
                            return Ok(outcome);
                        }
                    }
                    ResolvedInstruction::StaticGet { dst, field } => {
                        let Some(static_slot) = field.static_slot else {
                            return Ok(self.fault(VmFault::InvalidResolvedId));
                        };
                        let Some(value) = self.static_slots.get(static_slot as usize).copied()
                        else {
                            return Ok(self.fault(VmFault::InvalidStoragePlan));
                        };
                        if value == RuntimeValue::Null && !field.value_type.nullable {
                            let outcome = Outcome::Crashed(GuestTrap::NullReference);
                            self.lifecycle = Lifecycle::Terminal(outcome);
                            return Ok(outcome);
                        }
                        if let Err(fault) = self.write_register(frame_index, *dst, value) {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::StaticSet { field, value } => {
                        let value = match self.read_register(frame_index, *value) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let Some(static_slot) = field.static_slot else {
                            return Ok(self.fault(VmFault::InvalidResolvedId));
                        };
                        let Some(slot) = self.static_slots.get_mut(static_slot as usize) else {
                            return Ok(self.fault(VmFault::InvalidStoragePlan));
                        };
                        *slot = value;
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::FieldGet {
                        dst,
                        receiver,
                        field,
                    } => {
                        let receiver = match self.read_register(frame_index, *receiver) {
                            Ok(RuntimeValue::Null) => {
                                let outcome = Outcome::Crashed(GuestTrap::NullReference);
                                self.lifecycle = Lifecycle::Terminal(outcome);
                                return Ok(outcome);
                            }
                            Ok(RuntimeValue::Reference(reference)) => reference,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let actual = match self.heap.managed_type(receiver) {
                            Ok(actual) => match self.image.type_key(actual as usize) {
                                Some(actual) => actual,
                                None => return Ok(self.fault(VmFault::InvalidResolvedId)),
                            },
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        if !self.image.is_assignable(actual, field.owner) {
                            return Ok(self.fault(VmFault::InvalidReference));
                        }
                        let Some(offset) = field.offset else {
                            return Ok(self.fault(VmFault::InvalidResolvedId));
                        };
                        let Some(width) = width_for_type(field.value_type) else {
                            return Ok(self.fault(VmFault::InvalidValueType));
                        };
                        let value = match load_value(&self.heap, receiver, offset, width) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        if value == RuntimeValue::Null && !field.value_type.nullable {
                            let outcome = Outcome::Crashed(GuestTrap::NullReference);
                            self.lifecycle = Lifecycle::Terminal(outcome);
                            return Ok(outcome);
                        }
                        if let Err(fault) = self.write_register(frame_index, *dst, value) {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::FieldSet {
                        receiver,
                        field,
                        value,
                    } => {
                        let receiver_value = match self.read_register(frame_index, *receiver) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let value = match self.read_register(frame_index, *value) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let receiver = match receiver_value {
                            RuntimeValue::Null => {
                                let outcome = Outcome::Crashed(GuestTrap::NullReference);
                                self.lifecycle = Lifecycle::Terminal(outcome);
                                return Ok(outcome);
                            }
                            RuntimeValue::Reference(reference) => reference,
                            _ => return Ok(self.fault(VmFault::InvalidValueType)),
                        };
                        let actual = match self.heap.managed_type(receiver) {
                            Ok(actual) => match self.image.type_key(actual as usize) {
                                Some(actual) => actual,
                                None => return Ok(self.fault(VmFault::InvalidResolvedId)),
                            },
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        if !self.image.is_assignable(actual, field.owner)
                            || !self.runtime_value_matches(value, field.value_type)
                        {
                            return Ok(self.fault(VmFault::InvalidReference));
                        }
                        let Some(offset) = field.offset else {
                            return Ok(self.fault(VmFault::InvalidResolvedId));
                        };
                        let Some(width) = width_for_type(field.value_type) else {
                            return Ok(self.fault(VmFault::InvalidValueType));
                        };
                        if let Err(fault) =
                            store_value(&mut self.heap, receiver, offset, width, value)
                        {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::IsType { dst, value, ty } => {
                        let value = match self.read_register(frame_index, *value) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let result = match value {
                            RuntimeValue::Null => false,
                            RuntimeValue::Reference(reference) => {
                                let actual = match self.reference_type(reference) {
                                    Ok(actual) => actual,
                                    Err(fault) => return Ok(self.fault(fault)),
                                };
                                self.image.is_assignable(actual, *ty)
                            }
                            _ => return Ok(self.fault(VmFault::InvalidValueType)),
                        };
                        if let Err(fault) =
                            self.write_register(frame_index, *dst, RuntimeValue::Bool(result))
                        {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::CheckedCast { dst, value, ty } => {
                        let value = match self.read_register(frame_index, *value) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let destination_type = self
                            .image
                            .function(self.frames[frame_index].function)
                            .and_then(|function| function.registers.get(*dst as usize))
                            .copied()
                            .ok_or(RunError::NotRunnable)?;
                        match value {
                            RuntimeValue::Null if !destination_type.nullable => {
                                let outcome = Outcome::Crashed(GuestTrap::NullReference);
                                self.lifecycle = Lifecycle::Terminal(outcome);
                                return Ok(outcome);
                            }
                            RuntimeValue::Null => {}
                            RuntimeValue::Reference(reference) => {
                                let actual = match self.reference_type(reference) {
                                    Ok(actual) => actual,
                                    Err(fault) => return Ok(self.fault(fault)),
                                };
                                if !self.image.is_assignable(actual, *ty) {
                                    let outcome = Outcome::Crashed(GuestTrap::ClassCast);
                                    self.lifecycle = Lifecycle::Terminal(outcome);
                                    return Ok(outcome);
                                }
                            }
                            _ => return Ok(self.fault(VmFault::InvalidValueType)),
                        }
                        if let Err(fault) = self.write_register(frame_index, *dst, value) {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::ArrayLength { dst, array } => {
                        let array = match self.read_register(frame_index, *array) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let (_, _, _, length) = match self.resolve_array(array) {
                            Ok(array) => array,
                            Err(InstructionFailure::Trap(trap)) => {
                                let outcome = Outcome::Crashed(trap);
                                self.lifecycle = Lifecycle::Terminal(outcome);
                                return Ok(outcome);
                            }
                            Err(InstructionFailure::Fault(fault)) => return Ok(self.fault(fault)),
                        };
                        if let Err(fault) =
                            self.write_register(frame_index, *dst, RuntimeValue::I32(length))
                        {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::ArrayLoad { dst, array, index } => {
                        let array = match self.read_register(frame_index, *array) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let index_value = match self.read_register(frame_index, *index) {
                            Ok(RuntimeValue::I32(value)) => value,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let (reference, _, element, length) = match self.resolve_array(array) {
                            Ok(array) => array,
                            Err(InstructionFailure::Trap(trap)) => {
                                let outcome = Outcome::Crashed(trap);
                                self.lifecycle = Lifecycle::Terminal(outcome);
                                return Ok(outcome);
                            }
                            Err(InstructionFailure::Fault(fault)) => return Ok(self.fault(fault)),
                        };
                        let offset = match array_element_offset(index_value, length, element) {
                            Ok(offset) => offset,
                            Err(trap) => {
                                let outcome = Outcome::Crashed(trap);
                                self.lifecycle = Lifecycle::Terminal(outcome);
                                return Ok(outcome);
                            }
                        };
                        let value = match load_value(&self.heap, reference, offset, element) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let destination_type = self
                            .image
                            .function(self.frames[frame_index].function)
                            .and_then(|function| function.registers.get(*dst as usize))
                            .copied()
                            .ok_or(RunError::NotRunnable)?;
                        if value == RuntimeValue::Null && !destination_type.nullable {
                            let outcome = Outcome::Crashed(GuestTrap::NullReference);
                            self.lifecycle = Lifecycle::Terminal(outcome);
                            return Ok(outcome);
                        }
                        if let Err(fault) = self.write_register(frame_index, *dst, value) {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::ArrayStore {
                        array,
                        index,
                        value,
                    } => {
                        let array = match self.read_register(frame_index, *array) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let index_value = match self.read_register(frame_index, *index) {
                            Ok(RuntimeValue::I32(value)) => value,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let value = match self.read_register(frame_index, *value) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let (reference, array_type, element, length) = match self
                            .resolve_array(array)
                        {
                            Ok(array) => array,
                            Err(InstructionFailure::Trap(trap)) => {
                                let outcome = Outcome::Crashed(trap);
                                self.lifecycle = Lifecycle::Terminal(outcome);
                                return Ok(outcome);
                            }
                            Err(InstructionFailure::Fault(fault)) => return Ok(self.fault(fault)),
                        };
                        let element_type = self
                            .image
                            .array_element_type(array_type)
                            .ok_or(RunError::NotRunnable)?;
                        if !self.runtime_value_matches(value, element_type) {
                            return Ok(self.fault(VmFault::InvalidReference));
                        }
                        let offset = match array_element_offset(index_value, length, element) {
                            Ok(offset) => offset,
                            Err(trap) => {
                                let outcome = Outcome::Crashed(trap);
                                self.lifecycle = Lifecycle::Terminal(outcome);
                                return Ok(outcome);
                            }
                        };
                        if let Err(fault) =
                            store_value(&mut self.heap, reference, offset, element, value)
                        {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::CapabilityCallSync { .. }
                    | ResolvedInstruction::CapabilityCallAsync { .. } => {
                        return Ok(Outcome::HostRequest);
                    }
                    ResolvedInstruction::Unreachable => {
                        return Ok(self.fault(VmFault::ReachedUnreachable));
                    }
                    _ => {
                        let frame = self.frames[frame_index];
                        let function = self
                            .image
                            .function(frame.function)
                            .ok_or(RunError::NotRunnable)?;
                        match execute_scalar(
                            instruction,
                            &mut self.frame_arena,
                            frame,
                            function,
                            &self.image,
                            &self.heap,
                        ) {
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
        let frame = *self
            .frames
            .get(frame)
            .filter(|frame| frame.function != usize::MAX)
            .ok_or(VmFault::InvalidStoragePlan)?;
        let function = self
            .image
            .function(frame.function)
            .ok_or(VmFault::InvalidResolvedId)?;
        read_frame_value(&self.frame_arena, frame, function, register)
    }

    fn write_register(
        &mut self,
        frame: usize,
        register: u16,
        value: RuntimeValue,
    ) -> Result<(), VmFault> {
        let frame = *self
            .frames
            .get(frame)
            .filter(|frame| frame.function != usize::MAX)
            .ok_or(VmFault::InvalidStoragePlan)?;
        let function = self
            .image
            .function(frame.function)
            .ok_or(VmFault::InvalidResolvedId)?;
        write_frame_value(&mut self.frame_arena, frame, function, register, value)
    }

    fn ensure_type_initialized(
        &mut self,
        ty: TypeKey,
        caller_index: usize,
    ) -> Result<bool, Outcome> {
        let Some(type_index) = self.image.type_index(ty) else {
            return Err(self.fault(VmFault::InvalidResolvedId));
        };
        match self.type_initialization[type_index] {
            TypeInitializationState::Initialized | TypeInitializationState::Initializing => {
                return Ok(false);
            }
            TypeInitializationState::Failed => {
                return Err(self.fault(VmFault::CorruptLifecycle));
            }
            TypeInitializationState::Uninitialized => {}
        }

        if let Some(superclass) = self.image.type_superclass(ty) {
            if self.ensure_type_initialized(superclass, caller_index)? {
                return Ok(true);
            }
        }

        let Some(initializer) = self.image.type_initializer(ty) else {
            self.type_initialization[type_index] = TypeInitializationState::Initialized;
            return Ok(false);
        };
        if self.frame_depth >= self.image.maximum_call_depth() {
            self.type_initialization[type_index] = TypeInitializationState::Failed;
            let outcome = Outcome::Crashed(GuestTrap::StackOverflow);
            self.lifecycle = Lifecycle::Terminal(outcome);
            return Err(outcome);
        }
        let Some(function) = self.image.function(initializer) else {
            return Err(self.fault(VmFault::InvalidResolvedId));
        };
        let callee_index = self.frame_depth;
        if callee_index >= self.frames.len() {
            return Err(self.fault(VmFault::InvalidStoragePlan));
        }
        let reservation = match self.frame_arena.push(&function.frame_layout) {
            Ok(reservation) => reservation,
            Err(fault) => return Err(self.fault(fault)),
        };
        let caller = self.frames[caller_index];
        self.frames[callee_index] = Frame {
            function: initializer,
            base: reservation.base,
            byte_len: reservation.byte_len,
            block: function.first_block,
            instruction: 0,
            caller_block: caller.block,
            caller_instruction: caller.instruction,
            destination: u16::MAX,
            initializer: Some(ty),
        };
        self.type_initialization[type_index] = TypeInitializationState::Initializing;
        self.frame_depth += 1;
        self.maximum_observed_frame_depth = self.maximum_observed_frame_depth.max(self.frame_depth);
        Ok(true)
    }

    fn allocation_source(&self, frame_index: usize) -> AllocationSource {
        let frame = self.frames[frame_index];
        let key = self
            .image
            .function(frame.function)
            .map(|function| function.key)
            .unwrap_or(super::FunctionKey {
                module: u32::MAX,
                function: u32::MAX,
            });
        AllocationSource {
            module: key.module,
            function: key.function,
            block: u32::try_from(frame.block).unwrap_or(u32::MAX),
            instruction: u32::try_from(frame.instruction).unwrap_or(u32::MAX),
        }
    }

    fn fault(&mut self, fault: VmFault) -> Outcome {
        self.cancel_pending_allocation();
        self.cancel_pending_concat();
        self.cancel_pending_host_string();
        for state in &mut self.type_initialization {
            if *state == TypeInitializationState::Initializing {
                *state = TypeInitializationState::Failed;
            }
        }
        let outcome = Outcome::Faulted(fault);
        self.lifecycle = Lifecycle::Terminal(outcome);
        outcome
    }

    fn allocation_exhausted(
        &mut self,
        request_kind: AllocationRequestKind,
        requested: u32,
        collection_attempted: bool,
        source: AllocationSource,
    ) -> Outcome {
        let Some(exception) = self.emergency_oom else {
            return self.fault(VmFault::InvalidStoragePlan);
        };
        let diagnostic = self.heap.diagnostic();
        let outcome = Outcome::AllocationExhausted(AllocationExhaustion {
            exception,
            diagnostic: AllocationDiagnostic {
                request_kind,
                requested,
                live: self
                    .image
                    .storage_plan()
                    .heap_bytes
                    .saturating_sub(diagnostic.total_free),
                total_free: diagnostic.total_free,
                largest_free_block: diagnostic.largest_free_block,
                source,
            },
            collection_attempted,
        });
        self.lifecycle = Lifecycle::Terminal(outcome);
        outcome
    }

    fn start_collection(
        &mut self,
        retry: AllocationRetry,
        maintenance_budget: u32,
    ) -> Result<Outcome, RunError> {
        if self.collector.is_active()
            || self.allocation_retry.is_some()
            || self.string_collection_pending.is_some()
        {
            return Ok(self.fault(VmFault::CorruptLifecycle));
        }
        self.allocation_retry = Some(retry);
        self.collector.start();
        self.run_maintenance(maintenance_budget)
    }

    fn run_maintenance(&mut self, budget: u32) -> Result<Outcome, RunError> {
        let mut remaining = budget;
        while remaining != 0 && self.collector.is_active() {
            let Some(consumed) = self.consumed_maintenance_cost.checked_add(1) else {
                return Ok(self.fault(VmFault::AccountingOverflow));
            };
            remaining -= 1;
            self.consumed_maintenance_cost = consumed;
            match self.collector.step(
                &mut self.heap,
                &self.image,
                RootSet {
                    static_slots: &self.static_slots,
                    frames: &self.frames,
                    frame_arena: &self.frame_arena,
                    frame_depth: self.frame_depth,
                    external: &self.external_roots,
                },
            ) {
                Ok(1) => {}
                Ok(_) => return Ok(self.fault(VmFault::CorruptLifecycle)),
                Err(fault) => return Ok(self.fault(fault)),
            }
        }
        if !self.collector.is_active() {
            if let Some(target) = self.string_collection_pending.take() {
                match target {
                    StringCollectionTarget::Concat => {
                        let Some(pending) = self.pending_concat.as_mut() else {
                            return Ok(self.fault(VmFault::CorruptLifecycle));
                        };
                        pending.mark_collection_attempted();
                    }
                    StringCollectionTarget::HostResponse => {
                        let Some(pending) = self.pending_host_string.as_mut() else {
                            return Ok(self.fault(VmFault::CorruptLifecycle));
                        };
                        pending.mark_collection_attempted();
                    }
                }
                return Ok(Outcome::SliceExhausted);
            }
            let Some(retry) = self.allocation_retry.take() else {
                return Ok(self.fault(VmFault::CorruptLifecycle));
            };
            match retry.reserve(&mut self.heap) {
                Ok(Some(pending)) => self.pending_allocation = Some(pending),
                Ok(None) => {
                    return Ok(self.allocation_exhausted(
                        retry.shape.request_kind(),
                        retry.logical_bytes,
                        true,
                        retry.source,
                    ));
                }
                Err(fault) => return Ok(self.fault(fault)),
            }
        }
        Ok(Outcome::SliceExhausted)
    }

    fn resume_pending_allocation(
        &mut self,
        frame_index: usize,
        remaining: &mut u32,
    ) -> Option<Outcome> {
        let mut pending = self.pending_allocation.take()?;
        let destination = pending.state().destination;
        let collection_attempted = pending.state().collection_attempted;
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
        if let Err(fault) =
            self.write_register(frame_index, destination, RuntimeValue::Reference(reference))
        {
            return Some(self.fault(fault));
        }
        self.frames[frame_index].instruction += 1;
        None
    }

    fn resume_pending_text(&mut self, frame_index: usize, remaining: &mut u32) -> Option<Outcome> {
        let mut pending = self.pending_text.take()?;
        let (used, result) = match pending.resume(&self.image, &self.heap, *remaining) {
            Ok(result) => result,
            Err(error) => return Some(self.text_outcome(error)),
        };
        let Some(consumed) = self.consumed_dynamic_cost.checked_add(u64::from(used)) else {
            return Some(self.fault(VmFault::AccountingOverflow));
        };
        self.consumed_dynamic_cost = consumed;
        *remaining -= used;
        let Some((destination, value)) = result else {
            self.pending_text = Some(pending);
            return Some(Outcome::SliceExhausted);
        };
        if let Err(fault) = self.write_register(frame_index, destination, value) {
            return Some(self.fault(fault));
        }
        self.frames[frame_index].instruction += 1;
        None
    }

    fn resume_pending_concat(
        &mut self,
        frame_index: usize,
        remaining: &mut u32,
    ) -> Option<Outcome> {
        let mut pending = self.pending_concat.take()?;
        let (used, result) = match pending.resume(&self.image, &mut self.heap, *remaining) {
            Ok(result) => result,
            Err(text::TextError::Exhausted {
                used,
                block_bytes,
                requested,
                collection_attempted,
            }) => {
                let Some(consumed) = self.consumed_dynamic_cost.checked_add(u64::from(used)) else {
                    let _ = pending.abort(&mut self.heap);
                    return Some(self.fault(VmFault::AccountingOverflow));
                };
                self.consumed_dynamic_cost = consumed;
                *remaining -= used;
                let Some(source) = self.pending_concat_source else {
                    let _ = pending.abort(&mut self.heap);
                    return Some(self.fault(VmFault::CorruptLifecycle));
                };
                if block_bytes > self.image.storage_plan().heap_bytes {
                    let _ = pending.abort(&mut self.heap);
                    self.pending_concat_source = None;
                    return Some(self.allocation_exhausted(
                        AllocationRequestKind::String,
                        requested,
                        false,
                        source,
                    ));
                }
                if collection_attempted {
                    let _ = pending.abort(&mut self.heap);
                    self.pending_concat_source = None;
                    return Some(self.allocation_exhausted(
                        AllocationRequestKind::String,
                        requested,
                        true,
                        source,
                    ));
                }
                if self.collector.is_active()
                    || self.allocation_retry.is_some()
                    || self.string_collection_pending.is_some()
                {
                    let _ = pending.abort(&mut self.heap);
                    return Some(self.fault(VmFault::CorruptLifecycle));
                }
                self.pending_concat = Some(pending);
                self.string_collection_pending = Some(StringCollectionTarget::Concat);
                self.collector.start();
                return Some(Outcome::SliceExhausted);
            }
            Err(error) => {
                let _ = pending.abort(&mut self.heap);
                self.pending_concat_source = None;
                return Some(self.text_outcome(error));
            }
        };
        let Some(consumed) = self.consumed_dynamic_cost.checked_add(u64::from(used)) else {
            let _ = pending.abort(&mut self.heap);
            return Some(self.fault(VmFault::AccountingOverflow));
        };
        self.consumed_dynamic_cost = consumed;
        *remaining -= used;
        let Some((destination, value)) = result else {
            self.pending_concat = Some(pending);
            return Some(Outcome::SliceExhausted);
        };
        if let Err(fault) = self.write_register(frame_index, destination, value) {
            let _ = pending.abort(&mut self.heap);
            return Some(self.fault(fault));
        }
        self.pending_concat_source = None;
        self.frames[frame_index].instruction += 1;
        None
    }

    fn text_outcome(&mut self, error: text::TextError) -> Outcome {
        match error {
            text::TextError::Trap(trap) => {
                self.cancel_pending_concat();
                let outcome = Outcome::Crashed(trap);
                self.lifecycle = Lifecycle::Terminal(outcome);
                outcome
            }
            text::TextError::Fault(fault) => self.fault(fault),
            text::TextError::Exhausted { .. } => self.fault(VmFault::CorruptLifecycle),
        }
    }

    fn cancel_pending_allocation(&mut self) {
        if let Some(pending) = self.pending_allocation.take() {
            let _ = pending.abort(&mut self.heap);
        }
    }

    fn cancel_pending_concat(&mut self) {
        if let Some(pending) = self.pending_concat.take() {
            let _ = pending.abort(&mut self.heap);
        }
        self.pending_concat_source = None;
        self.string_collection_pending = None;
    }

    fn cancel_pending_host_string(&mut self) {
        if let Some(pending) = self.pending_host_string.take() {
            let _ = pending.abort(&mut self.heap);
        }
        self.pending_host_string_source = None;
        if matches!(
            self.string_collection_pending,
            Some(StringCollectionTarget::HostResponse)
        ) {
            self.string_collection_pending = None;
        }
    }

    fn reference_type(&self, reference: super::value::Ref32) -> Result<super::TypeKey, VmFault> {
        match reference.domain() {
            super::value::ReferenceDomain::Managed => {
                self.heap.managed_type(reference).and_then(|type_id| {
                    self.image
                        .type_key(type_id as usize)
                        .ok_or(VmFault::InvalidResolvedId)
                })
            }
            super::value::ReferenceDomain::External => self
                .image
                .reference_type(reference)
                .ok_or(VmFault::InvalidReference),
            super::value::ReferenceDomain::Image => self
                .image
                .reference_type(reference)
                .ok_or(VmFault::InvalidReference),
            super::value::ReferenceDomain::Reserved => Err(VmFault::InvalidReference),
        }
    }

    fn runtime_value_matches(&self, value: RuntimeValue, expected: ResolvedValueType) -> bool {
        match value {
            RuntimeValue::I32(_) => expected.kind == 1,
            RuntimeValue::I64(_) => expected.kind == 2,
            RuntimeValue::F32(_) => expected.kind == 3,
            RuntimeValue::F64(_) => expected.kind == 4,
            RuntimeValue::Bool(_) => expected.kind == 5,
            RuntimeValue::Char(_) => expected.kind == 6,
            RuntimeValue::Null => expected.kind == 7 && expected.nullable,
            RuntimeValue::Reference(reference) if expected.kind == 7 => expected
                .nominal
                .and_then(|target| {
                    self.reference_type(reference)
                        .ok()
                        .map(|actual| (actual, target))
                })
                .is_some_and(|(actual, target)| self.image.is_assignable(actual, target)),
            RuntimeValue::Reference(_) => false,
        }
    }

    fn resolve_array(
        &self,
        value: RuntimeValue,
    ) -> Result<(super::value::Ref32, super::TypeKey, ValueWidth, i32), InstructionFailure> {
        let reference = match value {
            RuntimeValue::Null => return Err(InstructionFailure::Trap(GuestTrap::NullReference)),
            RuntimeValue::Reference(reference) => reference,
            _ => return Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
        };
        let ty = self
            .heap
            .managed_type(reference)
            .map_err(InstructionFailure::Fault)
            .and_then(|type_id| {
                self.image
                    .type_key(type_id as usize)
                    .ok_or(InstructionFailure::Fault(VmFault::InvalidResolvedId))
            })?;
        let element = match self.image.type_layout(ty) {
            Some(RuntimeTypeLayout::Array { element }) => *element,
            _ => return Err(InstructionFailure::Fault(VmFault::InvalidReference)),
        };
        let length = match load_value(&self.heap, reference, 0, ValueWidth::I32)
            .map_err(InstructionFailure::Fault)?
        {
            RuntimeValue::I32(length) if length >= 0 => length,
            _ => return Err(InstructionFailure::Fault(VmFault::CorruptHeap)),
        };
        Ok((reference, ty, element, length))
    }

    pub(super) fn consumed_fixed_cost(&self) -> u64 {
        self.consumed_fixed_cost
    }

    pub(super) fn capability_suspension(&self) -> Result<CapabilitySuspension<'_>, VmFault> {
        let frame_index = self
            .frame_depth
            .checked_sub(1)
            .ok_or(VmFault::CorruptLifecycle)?;
        let frame = self
            .frames
            .get(frame_index)
            .ok_or(VmFault::CorruptLifecycle)?;
        let instruction = self
            .image
            .block(frame.block)
            .and_then(|block| block.instructions.get(frame.instruction))
            .ok_or(VmFault::CorruptLifecycle)?;
        let (capability, operation, arguments) = match instruction {
            ResolvedInstruction::CapabilityCallSync {
                capability,
                operation,
                args,
                ..
            }
            | ResolvedInstruction::CapabilityCallAsync {
                capability,
                operation,
                args,
                ..
            } => (*capability, *operation, args.as_ref()),
            _ => return Err(VmFault::CorruptLifecycle),
        };
        Ok(CapabilitySuspension {
            capability,
            operation,
            arguments,
        })
    }

    pub(super) fn capability_argument(&self, register: u16) -> Result<RuntimeValue, VmFault> {
        let frame_index = self
            .frame_depth
            .checked_sub(1)
            .ok_or(VmFault::CorruptLifecycle)?;
        self.read_register(frame_index, register)
    }

    pub(super) fn capability_string_length(&self, register: u16) -> Result<u32, VmFault> {
        let value = self.capability_argument(register)?;
        text::length(&self.image, &self.heap, value)
            .map_err(text_fault)
            .and_then(|length| u32::try_from(length).map_err(|_| VmFault::InvalidReference))
    }

    pub(super) fn capability_string_code_unit(
        &self,
        register: u16,
        index: u32,
    ) -> Result<u16, VmFault> {
        let value = self.capability_argument(register)?;
        let index = i32::try_from(index).map_err(|_| VmFault::InvalidReference)?;
        text::get(&self.image, &self.heap, value, index).map_err(text_fault)
    }

    pub(super) fn charge_capability_dynamic(&mut self, units: u32) -> Result<(), VmFault> {
        self.consumed_dynamic_cost = self
            .consumed_dynamic_cost
            .checked_add(u64::from(units))
            .ok_or(VmFault::AccountingOverflow)?;
        Ok(())
    }

    pub(super) fn complete_capability(
        &mut self,
        value: Option<RuntimeValue>,
    ) -> Result<(), VmFault> {
        let frame_index = self
            .frame_depth
            .checked_sub(1)
            .ok_or(VmFault::CorruptLifecycle)?;
        let frame = *self
            .frames
            .get(frame_index)
            .ok_or(VmFault::CorruptLifecycle)?;
        let (destination, continuation) = match self
            .image
            .block(frame.block)
            .and_then(|block| block.instructions.get(frame.instruction))
        {
            Some(ResolvedInstruction::CapabilityCallAsync {
                dst, resume_block, ..
            }) => (*dst, Some(*resume_block)),
            Some(ResolvedInstruction::CapabilityCallSync { dst, .. }) => (*dst, None),
            _ => return Err(VmFault::CorruptLifecycle),
        };
        if (destination == u16::MAX) != value.is_none() {
            return Err(VmFault::InvalidValueType);
        }
        if let Some(value) = value {
            let function = self
                .image
                .function(frame.function)
                .ok_or(VmFault::InvalidResolvedId)?;
            let expected = *function
                .registers
                .get(destination as usize)
                .ok_or(VmFault::InvalidStoragePlan)?;
            if !self.runtime_value_matches(value, expected) {
                return Err(VmFault::InvalidValueType);
            }
            self.write_register(frame_index, destination, value)?;
        }
        let frame = self
            .frames
            .get_mut(frame_index)
            .ok_or(VmFault::CorruptLifecycle)?;
        if let Some(resume_block) = continuation {
            frame.block = resume_block;
            frame.instruction = 0;
        } else {
            frame.instruction += 1;
        }
        Ok(())
    }

    pub(super) fn begin_capability_string_response(&mut self, empty: bool) -> Result<(), VmFault> {
        let frame_index = self
            .frame_depth
            .checked_sub(1)
            .ok_or(VmFault::CorruptLifecycle)?;
        let frame = *self
            .frames
            .get(frame_index)
            .ok_or(VmFault::CorruptLifecycle)?;
        let destination = match self
            .image
            .block(frame.block)
            .and_then(|block| block.instructions.get(frame.instruction))
        {
            Some(ResolvedInstruction::CapabilityCallAsync { dst, .. })
            | Some(ResolvedInstruction::CapabilityCallSync { dst, .. })
                if *dst != u16::MAX =>
            {
                *dst
            }
            _ => return Err(VmFault::CorruptLifecycle),
        };
        if empty {
            let value = self
                .image
                .empty_string()
                .ok_or(VmFault::InvalidResolvedId)?;
            return self.complete_capability(Some(value));
        }
        if self.pending_host_string.is_some() || self.string_collection_pending.is_some() {
            return Err(VmFault::CorruptLifecycle);
        }
        self.pending_host_string = Some(text::PendingHostString::new(destination));
        self.pending_host_string_source = Some(self.allocation_source(frame_index));
        Ok(())
    }

    pub(super) fn run_capability_string_slice(
        &mut self,
        source: &[u16],
        guest_budget: u32,
        maintenance_budget: u32,
    ) -> Result<Outcome, RunError> {
        match self.lifecycle {
            Lifecycle::Terminal(outcome) => return Ok(outcome),
            Lifecycle::Pristine => return Err(RunError::NotStarted),
            Lifecycle::Runnable => {}
        }
        if guest_budget == 0 || guest_budget > self.image.maximum_slice_budget() {
            return Err(RunError::InvalidSliceBudget {
                minimum: 1,
                maximum: self.image.maximum_slice_budget(),
                supplied: guest_budget,
            });
        }
        if maintenance_budget > self.image.maximum_slice_budget() {
            return Err(RunError::InvalidSliceBudget {
                minimum: 0,
                maximum: self.image.maximum_slice_budget(),
                supplied: maintenance_budget,
            });
        }
        if self.collector.is_active() {
            return self.run_maintenance(maintenance_budget);
        }
        let Some(mut pending) = self.pending_host_string.take() else {
            return Ok(self.fault(VmFault::CorruptLifecycle));
        };
        let (used, result) = match pending.resume(&self.image, &mut self.heap, source, guest_budget)
        {
            Ok(result) => result,
            Err(text::TextError::Exhausted {
                used,
                block_bytes,
                requested,
                collection_attempted,
            }) => {
                let Some(consumed) = self.consumed_dynamic_cost.checked_add(u64::from(used)) else {
                    let _ = pending.abort(&mut self.heap);
                    return Ok(self.fault(VmFault::AccountingOverflow));
                };
                self.consumed_dynamic_cost = consumed;
                let Some(source) = self.pending_host_string_source else {
                    let _ = pending.abort(&mut self.heap);
                    return Ok(self.fault(VmFault::CorruptLifecycle));
                };
                if block_bytes > self.image.storage_plan().heap_bytes || collection_attempted {
                    let _ = pending.abort(&mut self.heap);
                    self.pending_host_string_source = None;
                    return Ok(self.allocation_exhausted(
                        AllocationRequestKind::String,
                        requested,
                        collection_attempted,
                        source,
                    ));
                }
                if self.collector.is_active()
                    || self.allocation_retry.is_some()
                    || self.string_collection_pending.is_some()
                {
                    let _ = pending.abort(&mut self.heap);
                    return Ok(self.fault(VmFault::CorruptLifecycle));
                }
                self.pending_host_string = Some(pending);
                self.string_collection_pending = Some(StringCollectionTarget::HostResponse);
                self.collector.start();
                return Ok(Outcome::SliceExhausted);
            }
            Err(error) => {
                let _ = pending.abort(&mut self.heap);
                self.pending_host_string_source = None;
                return Ok(self.text_outcome(error));
            }
        };
        let Some(consumed) = self.consumed_dynamic_cost.checked_add(u64::from(used)) else {
            let _ = pending.abort(&mut self.heap);
            return Ok(self.fault(VmFault::AccountingOverflow));
        };
        self.consumed_dynamic_cost = consumed;
        let Some((_destination, value)) = result else {
            self.pending_host_string = Some(pending);
            return Ok(Outcome::SliceExhausted);
        };
        self.pending_host_string_source = None;
        if let Err(fault) = self.complete_capability(Some(value)) {
            return Ok(self.fault(fault));
        }
        Ok(Outcome::SliceExhausted)
    }

    pub(super) fn capability_string_response_pending(&self) -> bool {
        self.pending_host_string.is_some()
            || matches!(
                self.string_collection_pending,
                Some(StringCollectionTarget::HostResponse)
            )
    }

    pub(super) fn consumed_dynamic_cost(&self) -> u64 {
        self.consumed_dynamic_cost
    }

    #[cfg(test)]
    pub(super) fn string_length(&self, reference: super::value::Ref32) -> i32 {
        text::length(&self.image, &self.heap, RuntimeValue::Reference(reference))
            .ok()
            .unwrap()
    }

    #[cfg(test)]
    pub(super) fn string_get(&self, reference: super::value::Ref32, index: i32) -> u16 {
        text::get(
            &self.image,
            &self.heap,
            RuntimeValue::Reference(reference),
            index,
        )
        .ok()
        .unwrap()
    }

    #[cfg(test)]
    pub(super) fn string_encoding(
        &self,
        reference: super::value::Ref32,
    ) -> Option<super::layout::StringEncoding> {
        text::encoding(&self.image, &self.heap, RuntimeValue::Reference(reference))
            .ok()
            .unwrap()
    }

    pub(super) fn trace_digest(&self) -> [u8; 32] {
        self.trace.clone().finalize().into()
    }

    pub(super) fn trace_host_field(&mut self, bytes: &[u8]) {
        trace_field(&mut self.trace, bytes);
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
        for active_frame in 0..self.frame_depth {
            let frame = self.frames[active_frame];
            let active_function = self
                .image
                .function(frame.function)
                .ok_or(RunError::NotRunnable)?;
            trace_field(
                &mut self.trace,
                &u32::try_from(active_function.register_count)
                    .map_err(|_| RunError::NotRunnable)?
                    .to_le_bytes(),
            );
            for register in 0..active_function.register_count {
                let value =
                    read_frame_value(&self.frame_arena, frame, active_function, register as u16)
                        .map_err(|_| RunError::NotRunnable)?;
                let reference_type = match value {
                    RuntimeValue::Reference(reference) => Some(
                        self.reference_type(reference)
                            .map_err(|_| RunError::NotRunnable)?,
                    ),
                    _ => None,
                };
                trace_register(
                    &mut self.trace,
                    RegisterValue::Initialized(value),
                    reference_type,
                )?;
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
                let expected_type = expected.nominal.ok_or(RunError::EntryType { parameter })?;
                match value.domain() {
                    ReferenceDomain::Managed => {
                        let actual = self
                            .heap
                            .managed_type(value)
                            .ok()
                            .and_then(|type_id| self.image.type_key(type_id as usize))
                            .ok_or(RunError::DeadReference { parameter })?;
                        if self.image.is_assignable(actual, expected_type) {
                            Ok(())
                        } else {
                            Err(RunError::EntryType { parameter })
                        }
                    }
                    ReferenceDomain::External => {
                        let admitted = self
                            .image
                            .host_reference(value)
                            .filter(|value| {
                                value.live
                                    && argument.external_handle
                                        == Some(super::external_roots::ExternalHandle {
                                            slot: value.value.payload(),
                                            generation: value.generation,
                                        })
                            })
                            .ok_or(RunError::DeadReference { parameter })?;
                        if admitted.assignable_to.contains(&expected_type) {
                            Ok(())
                        } else {
                            Err(RunError::EntryType { parameter })
                        }
                    }
                    ReferenceDomain::Image | ReferenceDomain::Reserved => {
                        Err(RunError::EntryType { parameter })
                    }
                }
            }
            _ => Err(RunError::EntryType { parameter }),
        }
    }

    pub(super) fn frame_depth(&self) -> usize {
        self.frame_depth
    }

    pub(super) fn consumed_maintenance_cost(&self) -> u64 {
        self.consumed_maintenance_cost
    }

    #[cfg(test)]
    pub(super) fn test_register(&self, register: usize) -> Option<RuntimeValue> {
        let width = self.image.registers_per_frame();
        let frame_index = register.checked_div(width)?;
        let local = register.checked_rem(width)?;
        let frame = *self.frames.get(frame_index)?;
        let function = self.image.function(frame.function)?;
        if local >= function.register_count
            || !self
                .frame_arena
                .is_value_initialized(frame.base, &function.frame_layout, local)
        {
            return None;
        }
        read_frame_value(&self.frame_arena, frame, function, local as u16).ok()
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
    pub(super) fn test_managed_payload(&self, reference: super::value::Ref32) -> Option<Box<[u8]>> {
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
    pub(super) fn test_collector_active(&self) -> bool {
        self.collector.is_active()
    }

    #[cfg(test)]
    pub(super) fn test_remove_emergency_oom(&mut self) {
        self.emergency_oom = None;
    }

    #[cfg(test)]
    pub(super) fn test_reserved_bytes(&self) -> usize {
        core::mem::size_of::<Self>()
            + self.frames.len() * core::mem::size_of::<Frame>()
            + self.frame_arena.reserved_bytes()
            + self.static_slots.len() * core::mem::size_of::<RuntimeValue>()
            + self.heap.test_reserved_bytes()
            + self.external_roots.reserved_bytes()
    }

    #[cfg(test)]
    pub(super) fn test_snapshot(&self) -> (u8, usize, Box<[RegisterValue]>) {
        let width = self.image.registers_per_frame();
        let mut registers = vec![RegisterValue::Uninitialized; self.frames.len() * width];
        for frame_index in 0..self.frame_depth {
            let frame = self.frames[frame_index];
            let Some(function) = self.image.function(frame.function) else {
                continue;
            };
            for register in 0..function.register_count {
                let flat = frame_index * width + register;
                if let Some(value) = self.test_register(flat) {
                    registers[flat] = RegisterValue::Initialized(value);
                }
            }
        }
        (
            match self.lifecycle {
                Lifecycle::Pristine => 0,
                Lifecycle::Runnable => 1,
                Lifecycle::Terminal(_) => 2,
            },
            self.frame_depth,
            registers.into_boxed_slice(),
        )
    }

    #[cfg(test)]
    pub(super) fn test_active_registers(&self) -> Box<[RegisterValue]> {
        let width = self.image.registers_per_frame();
        let base = self.frame_depth.saturating_sub(1) * width;
        (0..width)
            .map(|register| {
                self.test_register(base + register)
                    .map_or(RegisterValue::Uninitialized, RegisterValue::Initialized)
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) fn maximum_observed_frame_depth_for_test(&self) -> usize {
        self.maximum_observed_frame_depth
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        self.cancel_pending_allocation();
        self.cancel_pending_concat();
    }
}

fn zero_value(value_type: ResolvedValueType) -> Result<RuntimeValue, AdmissionError> {
    match value_type.kind {
        1 => Ok(RuntimeValue::I32(0)),
        2 => Ok(RuntimeValue::I64(0)),
        3 => Ok(RuntimeValue::F32(0)),
        4 => Ok(RuntimeValue::F64(0)),
        5 => Ok(RuntimeValue::Bool(false)),
        6 => Ok(RuntimeValue::Char(0)),
        7 => Ok(RuntimeValue::Null),
        _ => Err(AdmissionError::InvalidEntry),
    }
}

fn width_for_type(value_type: ResolvedValueType) -> Option<ValueWidth> {
    match value_type.kind {
        1 => Some(ValueWidth::I32),
        2 => Some(ValueWidth::I64),
        3 => Some(ValueWidth::F32),
        4 => Some(ValueWidth::F64),
        5 => Some(ValueWidth::Bool),
        6 => Some(ValueWidth::Char),
        7 => Some(ValueWidth::Ref),
        _ => None,
    }
}

fn array_element_offset(index: i32, length: i32, element: ValueWidth) -> Result<u32, GuestTrap> {
    if index < 0 || index >= length {
        return Err(GuestTrap::IndexOutOfBounds);
    }
    8_u32
        .checked_add(
            (index as u32)
                .checked_mul(element.bytes())
                .ok_or(GuestTrap::IndexOutOfBounds)?,
        )
        .ok_or(GuestTrap::IndexOutOfBounds)
}

fn trace_field(trace: &mut Sha256, bytes: &[u8]) {
    trace.update((bytes.len() as u32).to_le_bytes());
    trace.update(bytes);
}

fn text_fault(error: text::TextError) -> VmFault {
    match error {
        text::TextError::Fault(fault) => fault,
        text::TextError::Trap(_) | text::TextError::Exhausted { .. } => VmFault::InvalidReference,
    }
}

fn trace_register(
    trace: &mut Sha256,
    register: RegisterValue,
    reference_type: Option<super::TypeKey>,
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
                    let ty = reference_type.ok_or(RunError::NotRunnable)?;
                    trace.update(ty.module.to_le_bytes());
                    trace.update(ty.ty.to_le_bytes());
                    trace.update(value.payload().to_le_bytes());
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
    arena: &mut FrameArena,
    frame: Frame,
    function: &ResolvedFunction,
    image: &ExecutionImage,
    heap: &Heap,
) -> Result<(), InstructionFailure> {
    macro_rules! binary {
        ($dst:expr, $lhs:expr, $rhs:expr, $body:expr) => {{
            let lhs = read_frame_value(arena, frame, function, *$lhs)
                .map_err(InstructionFailure::Fault)?;
            let rhs = read_frame_value(arena, frame, function, *$rhs)
                .map_err(InstructionFailure::Fault)?;
            let value = ($body)(lhs, rhs)?;
            write_frame_value(arena, frame, function, *$dst, value)
                .map_err(InstructionFailure::Fault)
        }};
    }
    match instruction {
        ResolvedInstruction::Nop => Ok(()),
        ResolvedInstruction::Move { dst, src } => {
            let value = read_frame_value(arena, frame, function, *src)
                .map_err(InstructionFailure::Fault)?;
            write_frame_value(arena, frame, function, *dst, value)
                .map_err(InstructionFailure::Fault)
        }
        ResolvedInstruction::Const { dst, constant } => write_frame_value(
            arena,
            frame,
            function,
            *dst,
            image
                .constant(*constant)
                .ok_or(InstructionFailure::Fault(VmFault::InvalidResolvedId))?,
        )
        .map_err(InstructionFailure::Fault),
        ResolvedInstruction::Null { dst } => {
            write_frame_value(arena, frame, function, *dst, RuntimeValue::Null)
                .map_err(InstructionFailure::Fault)
        }
        ResolvedInstruction::Convert { dst, src } => {
            let destination = function
                .registers
                .get(*dst as usize)
                .ok_or(InstructionFailure::Fault(VmFault::InvalidValueType))?
                .kind;
            let source = read_frame_value(arena, frame, function, *src)
                .map_err(InstructionFailure::Fault)?;
            let value = convert(source, destination)?;
            write_frame_value(arena, frame, function, *dst, value)
                .map_err(InstructionFailure::Fault)
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
            let source = read_frame_value(arena, frame, function, *src)
                .map_err(InstructionFailure::Fault)?;
            let value = negate(*form, source)?;
            write_frame_value(arena, frame, function, *dst, value)
                .map_err(InstructionFailure::Fault)
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
        ResolvedInstruction::StringLength { dst, string } => {
            let string = read_frame_value(arena, frame, function, *string)
                .map_err(InstructionFailure::Fault)?;
            let value = text::length(image, heap, string).map_err(|error| match error {
                text::TextError::Trap(trap) => InstructionFailure::Trap(trap),
                text::TextError::Fault(fault) => InstructionFailure::Fault(fault),
                text::TextError::Exhausted { .. } => {
                    InstructionFailure::Fault(VmFault::CorruptLifecycle)
                }
            })?;
            write_frame_value(arena, frame, function, *dst, RuntimeValue::I32(value))
                .map_err(InstructionFailure::Fault)
        }
        ResolvedInstruction::StringGet { dst, string, index } => {
            let index_value = read_frame_value(arena, frame, function, *index)
                .map_err(InstructionFailure::Fault)?;
            let RuntimeValue::I32(index) = index_value else {
                return Err(InstructionFailure::Fault(VmFault::InvalidValueType));
            };
            let string = read_frame_value(arena, frame, function, *string)
                .map_err(InstructionFailure::Fault)?;
            let value = text::get(image, heap, string, index).map_err(|error| match error {
                text::TextError::Trap(trap) => InstructionFailure::Trap(trap),
                text::TextError::Fault(fault) => InstructionFailure::Fault(fault),
                text::TextError::Exhausted { .. } => {
                    InstructionFailure::Fault(VmFault::CorruptLifecycle)
                }
            })?;
            write_frame_value(arena, frame, function, *dst, RuntimeValue::Char(value))
                .map_err(InstructionFailure::Fault)
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
