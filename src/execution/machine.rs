use super::{
    array_copy::PendingArrayCopy,
    channel::{ChannelArena, ChannelError, ReceiveResult, SendResult},
    error::{
        AdmissionError, AllocationDiagnostic, AllocationExhaustion, AllocationRequestKind,
        AllocationSource, EntryArgumentLimit, GuestTrap, Outcome, ResidentStorageComponent,
        RunError, VmFault,
    },
    external_roots::ExternalRootTable,
    frame::{FrameArena, FrameReservation, StaticArena},
    gc::{Collector, RootSet},
    heap::{AllocationRequest, Heap},
    heap_ops::{load_value, store_value, PendingAllocation, PendingState},
    host::{EntryArgumentLimits, RequestId, TaskId},
    image::{ExecutionImage, ResolvedFunction, ResolvedInstruction, ResolvedValueType},
    layout::{array_layout, RuntimeTypeLayout, ValueWidth},
    numeric,
    task::{TaskError, TaskScheduler},
    text,
    value::{EntryArgument, Ref32, ReferenceDomain, RuntimeValue},
    TypeKey,
};
use crate::VerifiedArtifact;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Lifecycle {
    Pristine,
    Runnable,
    Terminal(Outcome),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TypeInitializationState {
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

#[inline(always)]
pub(super) fn read_frame_value(
    arena: &FrameArena,
    frame: Frame,
    function: &ResolvedFunction,
    register: u16,
) -> Result<RuntimeValue, VmFault> {
    let register = register as usize;
    let access = *function
        .register_accesses
        .get(register)
        .ok_or(VmFault::InvalidStoragePlan)?;
    arena.read_access(frame.base, access)
}

#[inline(always)]
pub(super) fn write_frame_value(
    arena: &mut FrameArena,
    frame: Frame,
    function: &ResolvedFunction,
    register: u16,
    value: RuntimeValue,
) -> Result<(), VmFault> {
    let register = register as usize;
    let access = *function
        .register_accesses
        .get(register)
        .ok_or(VmFault::InvalidStoragePlan)?;
    arena.write_access(frame.base, access, value)
}

pub(super) struct Machine {
    image: ExecutionImage,
    lifecycle: Lifecycle,
    frames: Box<[Frame]>,
    task_frames: Box<[Frame]>,
    task_frame_depths: Box<[usize]>,
    task_failures: Box<[Option<TaskFailure>]>,
    task_host_failures: Box<[Option<PendingHostFailure>]>,
    tasks: TaskScheduler,
    channels: ChannelArena,
    frame_arena: FrameArena,
    statics: StaticArena,
    type_initialization: Box<[TypeInitializationState]>,
    heap: Heap,
    external_roots: ExternalRootTable,
    collector: Collector,
    allocation_retry: Option<AllocationRetry>,
    pending_allocation: Option<PendingAllocation>,
    pending_array_copy: Option<PendingArrayCopy>,
    pending_exception: Option<PendingException>,
    pending_raise: Option<PendingRaise>,
    pending_text: Option<text::PendingText>,
    pending_concat: Option<text::PendingConcat>,
    pending_concat_source: Option<AllocationSource>,
    pending_host_string: Option<text::PendingHostString>,
    pending_record: Option<super::record::PendingRecord>,
    pending_record_source: Option<AllocationSource>,
    pending_host_string_source: Option<AllocationSource>,
    string_collection_pending: Option<StringCollectionTarget>,
    task_string_response: Option<(TaskId, RequestId, Option<TaskId>)>,
    emergency_oom: Option<super::value::Ref32>,
    frame_depth: usize,
    failure_stack: Option<FailureStack>,
    consumed_fixed_cost: u64,
    consumed_dynamic_cost: u64,
    consumed_maintenance_cost: u64,
    entered_blocks: u64,
    executed_instructions: u64,
    retired_instructions: u64,
    maximum_observed_frame_depth: usize,
    trace: Sha256,
    trace_enabled: bool,
}

pub(crate) const MAXIMUM_FAILURE_FRAMES: usize = 32;

/// A host-owned, allocation-free snapshot; never reads possibly damaged guest storage.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FailureStack {
    pub frames: [AllocationSource; MAXIMUM_FAILURE_FRAMES],
    pub length: usize,
    pub omitted: usize,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TaskFailure {
    pub exception: Ref32,
    pub stack: FailureStack,
}

#[derive(Clone, Copy, Debug)]
struct PendingException {
    failure: TaskFailure,
    actual_type: TypeKey,
    next_handler: usize,
    retires_instruction: bool,
}

#[derive(Clone, Copy, Debug)]
struct PendingHostFailure {
    role: u8,
    length: usize,
    bytes: [u8; super::host::MAXIMUM_HOST_FAILURE_DETAIL_BYTES],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MachineResourceSnapshot {
    pub heap_capacity_bytes: u64,
    pub heap_used_bytes: u64,
    pub live_objects: u64,
    pub mutable_resident_bytes: u64,
    pub task_capacity: u64,
    pub live_tasks: u64,
    pub runnable_tasks: u64,
    pub suspended_tasks: u64,
    pub completed_tasks: u64,
}

#[derive(Clone, Copy, Debug)]
enum AllocationShape {
    Object,
    Exception,
    Array { length: u32 },
}

impl AllocationShape {
    const fn request_kind(self) -> AllocationRequestKind {
        match self {
            Self::Object | Self::Exception => AllocationRequestKind::Object,
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
    RecordResponse,
    ExceptionMessage,
}

#[derive(Clone, Copy, Debug)]
struct PendingRaise {
    ty: TypeKey,
    units: [u16; super::host::MAXIMUM_HOST_FAILURE_DETAIL_BYTES],
    length: usize,
    text: text::PendingHostString,
    message: Option<Ref32>,
    exception: Option<Ref32>,
    stack: FailureStack,
    retires_instruction: bool,
}

fn task_fault(error: TaskError) -> VmFault {
    match error {
        TaskError::NoCapacity | TaskError::IdExhausted => VmFault::HandleExhausted,
        TaskError::UnknownTask | TaskError::SelfJoin | TaskError::JoinCycle => {
            VmFault::InvalidReference
        }
        TaskError::AlreadyStarted
        | TaskError::NotStarted
        | TaskError::NoRunningTask
        | TaskError::WrongWait
        | TaskError::CorruptQueue => VmFault::CorruptLifecycle,
    }
}

fn channel_failure(error: ChannelError) -> InstructionFailure {
    match error {
        ChannelError::InvalidCapacity | ChannelError::InvalidHandle => {
            InstructionFailure::Trap(GuestTrap::InvalidArgument)
        }
        ChannelError::NoCapacity => InstructionFailure::Fault(VmFault::HandleExhausted),
        ChannelError::CorruptQueue => InstructionFailure::Fault(VmFault::CorruptLifecycle),
    }
}

impl AllocationRetry {
    fn reserve(
        self,
        heap: &mut Heap,
        collection_attempted: bool,
    ) -> Result<Option<PendingAllocation>, VmFault> {
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
            collection_attempted,
        };
        Ok(Some(match self.shape {
            AllocationShape::Object => PendingAllocation::Object(state),
            AllocationShape::Exception => PendingAllocation::Exception(state),
            AllocationShape::Array { length } => PendingAllocation::Array { state, length },
        }))
    }
}

impl Machine {
    pub(super) const fn task_failure_bytes(capacity: u64, host_failures: bool) -> Option<u64> {
        let per_task = core::mem::size_of::<Option<TaskFailure>>()
            + if host_failures {
                core::mem::size_of::<Option<PendingHostFailure>>()
            } else {
                0
            };
        (per_task as u64).checked_mul(capacity)
    }

    pub(super) const fn pending_state_bytes() -> u64 {
        (core::mem::size_of::<Option<AllocationRetry>>()
            + core::mem::size_of::<Option<PendingArrayCopy>>()
            + core::mem::size_of::<Option<PendingException>>()
            + core::mem::size_of::<Option<PendingRaise>>()
            + core::mem::size_of::<Option<PendingAllocation>>()
            + core::mem::size_of::<Option<text::PendingText>>()
            + core::mem::size_of::<Option<text::PendingConcat>>()
            + core::mem::size_of::<Option<AllocationSource>>() * 2
            + core::mem::size_of::<Option<super::record::PendingRecord>>()
            + core::mem::size_of::<Option<AllocationSource>>()
            + core::mem::size_of::<Option<text::PendingHostString>>()
            + core::mem::size_of::<Option<StringCollectionTarget>>()) as u64
            + core::mem::size_of::<Option<(TaskId, RequestId, Option<TaskId>)>>() as u64
    }

    pub(super) const fn fixed_state_bytes() -> u64 {
        core::mem::size_of::<Self>() as u64 - Self::pending_state_bytes()
    }

    pub(super) fn new(image: ExecutionImage) -> Result<Self, AdmissionError> {
        Self::new_with_trace(image, true)
    }

    pub(super) fn new_untraced(image: ExecutionImage) -> Result<Self, AdmissionError> {
        Self::new_with_trace(image, false)
    }

    fn new_with_trace(image: ExecutionImage, trace_enabled: bool) -> Result<Self, AdmissionError> {
        let heap = Heap::new(&image.storage_plan())?;
        let external_roots = ExternalRootTable::new(image.external_root_capacity())?;
        let frame_count = image.maximum_call_depth();
        let task_count = image.maximum_coroutines();
        let frame_arena_bytes =
            u32::try_from(image.storage_plan().frame_arena_bytes).map_err(|_| {
                AdmissionError::ResidentStorageOverflow {
                    component: ResidentStorageComponent::FrameArena,
                }
            })?;
        let frame_arena =
            FrameArena::new(frame_arena_bytes).map_err(|_| AdmissionError::AllocationFailed)?;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(frame_count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        frames.resize(frame_count, Frame::EMPTY);
        let saved_frame_count = frame_count
            .checked_mul(task_count)
            .ok_or(AdmissionError::StoragePlanOverflow)?;
        let mut task_frames = Vec::new();
        task_frames
            .try_reserve_exact(saved_frame_count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        task_frames.resize(saved_frame_count, Frame::EMPTY);
        let mut task_frame_depths = Vec::new();
        task_frame_depths
            .try_reserve_exact(task_count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        task_frame_depths.resize(task_count, 0);
        let mut task_failures = Vec::new();
        task_failures
            .try_reserve_exact(task_count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        task_failures.resize(task_count, None);
        let host_failure_count = if image.runtime_exception_type(8).is_some() {
            task_count
        } else {
            0
        };
        let mut task_host_failures = Vec::new();
        task_host_failures
            .try_reserve_exact(host_failure_count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        task_host_failures.resize(host_failure_count, None);
        let tasks = TaskScheduler::new(task_count).map_err(|_| AdmissionError::AllocationFailed)?;
        let channels = ChannelArena::new(
            image.maximum_channels(),
            image.maximum_channel_values(),
            task_count,
        )
        .map_err(|_| AdmissionError::AllocationFailed)?;
        let statics = StaticArena::new(image.static_layout().clone())
            .map_err(|_| AdmissionError::AllocationFailed)?;
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
            task_frames: task_frames.into_boxed_slice(),
            task_frame_depths: task_frame_depths.into_boxed_slice(),
            task_failures: task_failures.into_boxed_slice(),
            task_host_failures: task_host_failures.into_boxed_slice(),
            tasks,
            channels,
            frame_arena,
            statics,
            type_initialization: type_initialization.into_boxed_slice(),
            heap,
            external_roots,
            collector: Collector::new(),
            allocation_retry: None,
            pending_allocation: None,
            pending_array_copy: None,
            pending_exception: None,
            pending_raise: None,
            pending_text: None,
            pending_concat: None,
            pending_concat_source: None,
            pending_host_string: None,
            pending_host_string_source: None,
            string_collection_pending: None,
            task_string_response: None,
            pending_record: None,
            pending_record_source: None,
            emergency_oom: Some(super::value::Ref32::reserved(0).unwrap()),
            frame_depth: 0,
            failure_stack: None,
            consumed_fixed_cost: 0,
            consumed_dynamic_cost: 0,
            consumed_maintenance_cost: 0,
            entered_blocks: 0,
            executed_instructions: 0,
            retired_instructions: 0,
            maximum_observed_frame_depth: 0,
            trace: Sha256::new(),
            trace_enabled,
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
        self.tasks.start_root().map_err(|_| RunError::NotRunnable)?;
        self.lifecycle = Lifecycle::Runnable;
        Ok(())
    }

    fn save_current_task(&mut self) -> Result<(), VmFault> {
        let task = self.tasks.current().map_err(task_fault)?;
        self.save_task(task)
    }

    fn save_task(&mut self, task: TaskId) -> Result<(), VmFault> {
        let slot = self.tasks.slot_of(task).ok_or(VmFault::CorruptLifecycle)?;
        let width = self.image.maximum_call_depth();
        let start = slot.checked_mul(width).ok_or(VmFault::InvalidStoragePlan)?;
        let end = start
            .checked_add(width)
            .ok_or(VmFault::InvalidStoragePlan)?;
        let saved = self
            .task_frames
            .get_mut(start..end)
            .ok_or(VmFault::InvalidStoragePlan)?;
        saved.fill(Frame::EMPTY);
        saved[..self.frame_depth].copy_from_slice(&self.frames[..self.frame_depth]);
        self.task_frame_depths[slot] = self.frame_depth;
        self.frames[..self.frame_depth].fill(Frame::EMPTY);
        self.frame_depth = 0;
        Ok(())
    }

    fn restore_task(&mut self, task: TaskId) -> Result<(), VmFault> {
        let slot = self.tasks.slot_of(task).ok_or(VmFault::CorruptLifecycle)?;
        let depth = *self
            .task_frame_depths
            .get(slot)
            .ok_or(VmFault::InvalidStoragePlan)?;
        let width = self.image.maximum_call_depth();
        let start = slot.checked_mul(width).ok_or(VmFault::InvalidStoragePlan)?;
        let end = start
            .checked_add(width)
            .ok_or(VmFault::InvalidStoragePlan)?;
        let saved = self
            .task_frames
            .get_mut(start..end)
            .ok_or(VmFault::InvalidStoragePlan)?;
        self.frames[..depth].copy_from_slice(&saved[..depth]);
        saved[..depth].fill(Frame::EMPTY);
        self.task_frame_depths[slot] = 0;
        self.frame_depth = depth;
        Ok(())
    }

    fn resume_saved_channel_task(
        &mut self,
        task: TaskId,
        channel: u32,
        resume_block: usize,
        result: Option<(u16, i32)>,
    ) -> Result<(), VmFault> {
        let slot = self.tasks.slot_of(task).ok_or(VmFault::CorruptLifecycle)?;
        let depth = *self
            .task_frame_depths
            .get(slot)
            .filter(|depth| **depth != 0)
            .ok_or(VmFault::CorruptLifecycle)?;
        let start = slot
            .checked_mul(self.image.maximum_call_depth())
            .ok_or(VmFault::InvalidStoragePlan)?;
        let frame_index = start
            .checked_add(depth - 1)
            .ok_or(VmFault::InvalidStoragePlan)?;
        let frame = *self
            .task_frames
            .get(frame_index)
            .filter(|frame| frame.function != usize::MAX)
            .ok_or(VmFault::CorruptLifecycle)?;
        let function = self
            .image
            .function(frame.function)
            .ok_or(VmFault::InvalidResolvedId)?;
        if let Some((destination, value)) = result {
            write_frame_value(
                &mut self.frame_arena,
                frame,
                function,
                destination,
                RuntimeValue::I32(value),
            )?;
        }
        self.task_frames[frame_index].block = resume_block;
        self.task_frames[frame_index].instruction = 0;
        self.tasks
            .complete_channel(task, channel)
            .map_err(task_fault)
    }

    fn activate_next_task(&mut self) -> Result<bool, VmFault> {
        let Some(task) = self.tasks.activate_next().map_err(task_fault)? else {
            return Ok(false);
        };
        self.restore_task(task)?;
        let slot = self.tasks.slot_of(task).ok_or(VmFault::CorruptLifecycle)?;
        if let Some(failure) = self.task_host_failures.get_mut(slot).and_then(Option::take) {
            let detail = core::str::from_utf8(&failure.bytes[..failure.length])
                .map_err(|_| VmFault::CorruptLifecycle)?;
            self.begin_runtime_exception(failure.role, detail)?;
            self.pending_raise
                .as_mut()
                .ok_or(VmFault::CorruptLifecycle)?
                .retires_instruction = false;
        }
        Ok(true)
    }

    pub(super) fn complete_task_host_failure(
        &mut self,
        task: TaskId,
        request: RequestId,
        role: u8,
        detail: &str,
    ) -> Result<(), VmFault> {
        if detail.len() > super::host::MAXIMUM_HOST_FAILURE_DETAIL_BYTES
            || self.tasks.state(task)
                != Some(super::task::TaskState::Waiting(
                    super::task::TaskWait::Host(request),
                ))
        {
            return Err(VmFault::CorruptLifecycle);
        }
        let slot = self.tasks.slot_of(task).ok_or(VmFault::CorruptLifecycle)?;
        let target = self
            .task_host_failures
            .get_mut(slot)
            .ok_or(VmFault::InvalidStoragePlan)?;
        if target.is_some() {
            return Err(VmFault::CorruptLifecycle);
        }
        let mut bytes = [0; super::host::MAXIMUM_HOST_FAILURE_DETAIL_BYTES];
        bytes[..detail.len()].copy_from_slice(detail.as_bytes());
        *target = Some(PendingHostFailure {
            role,
            length: detail.len(),
            bytes,
        });
        self.tasks
            .complete_host(task, request)
            .map_err(task_fault)?;
        if self.tasks.current().is_err() && self.task_string_response.is_none() {
            self.activate_next_task()?;
        }
        Ok(())
    }

    pub(super) fn current_task(&self) -> Result<TaskId, VmFault> {
        self.tasks.current().map_err(task_fault)
    }

    pub(super) fn terminal_outcome(&self) -> Option<Outcome> {
        match self.lifecycle {
            Lifecycle::Terminal(outcome) => Some(outcome),
            _ => None,
        }
    }

    pub(super) fn has_active_task(&self) -> bool {
        self.tasks.current().is_ok()
    }

    pub(super) fn minimum_run_budget(&self) -> u32 {
        if self.pending_allocation.is_some()
            || self.pending_exception.is_some()
            || self.pending_raise.is_some()
        {
            1
        } else {
            self.image.minimum_slice_cost()
        }
    }

    pub(super) fn suspend_capability_task(
        &mut self,
        request: super::host::RequestId,
    ) -> Result<bool, VmFault> {
        let task = self.current_task()?;
        self.tasks.suspend_host(request).map_err(task_fault)?;
        self.save_task(task)?;
        self.activate_next_task()
    }

    pub(super) fn complete_task_capability(
        &mut self,
        task: TaskId,
        request: super::host::RequestId,
        value: Option<RuntimeValue>,
    ) -> Result<(), VmFault> {
        let active = self.tasks.current().ok();
        if let Some(active) = active {
            self.save_task(active)?;
        }
        self.restore_task(task)?;
        let completion = self.complete_capability(value);
        let save = self.save_task(task);
        if let Some(active) = active {
            self.restore_task(active)?;
        }
        completion?;
        save?;
        self.tasks
            .complete_host(task, request)
            .map_err(task_fault)?;
        if active.is_none() {
            self.activate_next_task()?;
        }
        Ok(())
    }

    pub(super) fn begin_task_record_response(
        &mut self,
        task: TaskId,
        request: RequestId,
        value: super::host::HostRecordValue,
    ) -> Result<(), VmFault> {
        if self.task_string_response.is_some() || self.pending_record.is_some() {
            return Err(VmFault::CorruptLifecycle);
        }
        let active = self.tasks.current().ok();
        if let Some(active) = active {
            self.save_task(active)?;
        }
        self.restore_task(task)?;
        let frame_index = self
            .frame_depth
            .checked_sub(1)
            .ok_or(VmFault::CorruptLifecycle)?;
        let frame = self
            .frames
            .get(frame_index)
            .ok_or(VmFault::CorruptLifecycle)?;
        let destination = match self
            .image
            .block(frame.block)
            .and_then(|block| block.instructions.get(frame.instruction))
        {
            Some(ResolvedInstruction::CapabilityCallAsync { dst, .. }) if *dst != u16::MAX => *dst,
            _ => return Err(VmFault::CorruptLifecycle),
        };
        let ty = self
            .image
            .function(frame.function)
            .and_then(|function| function.registers.get(destination as usize))
            .and_then(|value| value.nominal)
            .ok_or(VmFault::InvalidValueType)?;
        let layout = self
            .image
            .record_layout(ty)
            .ok_or(VmFault::InvalidResolvedId)?;
        self.pending_record = Some(super::record::PendingRecord::new(layout, value)?);
        self.pending_record_source = Some(self.allocation_source(frame_index));
        self.task_string_response = Some((task, request, active));
        Ok(())
    }

    pub(super) fn record_response_pending(&self) -> bool {
        self.pending_record.is_some()
    }

    pub(super) fn run_record_slice(
        &mut self,
        guest_budget: u32,
        maintenance_budget: u32,
    ) -> Result<Outcome, RunError> {
        match self.lifecycle {
            Lifecycle::Terminal(outcome) => return Ok(outcome),
            Lifecycle::Pristine => return Err(RunError::NotStarted),
            Lifecycle::Runnable => {}
        }
        if guest_budget == 0
            || guest_budget > self.image.maximum_slice_budget()
            || maintenance_budget > self.image.maximum_slice_budget()
        {
            return Err(RunError::InvalidSliceBudget {
                minimum: 1,
                maximum: self.image.maximum_slice_budget(),
                supplied: guest_budget,
            });
        }
        if self.collector.is_active() {
            return self.run_maintenance(maintenance_budget);
        }
        let Some(mut pending) = self.pending_record.take() else {
            return Ok(self.fault(VmFault::CorruptLifecycle));
        };
        let result = pending.resume(&self.image, &mut self.heap, guest_budget);
        let used = match &result {
            Ok((used, _)) | Err(text::TextError::Exhausted { used, .. }) => *used,
            _ => 0,
        };
        let Some(consumed) = self.consumed_dynamic_cost.checked_add(u64::from(used)) else {
            let _ = pending.abort(&mut self.heap);
            return Ok(self.fault(VmFault::AccountingOverflow));
        };
        self.consumed_dynamic_cost = consumed;
        match result {
            Ok((_, Some(value))) => {
                self.pending_record_source = None;
                if let Err(fault) = self.complete_capability(Some(value)) {
                    return Ok(self.fault(fault));
                }
            }
            Ok((_, None)) => self.pending_record = Some(pending),
            Err(text::TextError::Exhausted {
                block_bytes,
                requested,
                collection_attempted,
                ..
            }) => {
                let Some(source) = self.pending_record_source else {
                    let _ = pending.abort(&mut self.heap);
                    return Ok(self.fault(VmFault::CorruptLifecycle));
                };
                if u64::from(block_bytes) > self.image.storage_plan().heap_arena_bytes
                    || collection_attempted
                {
                    let kind = pending.allocation_kind();
                    let _ = pending.abort(&mut self.heap);
                    self.pending_record_source = None;
                    return Ok(self.allocation_exhausted(
                        kind,
                        requested,
                        collection_attempted,
                        source,
                    ));
                }
                if self.allocation_retry.is_some() || self.string_collection_pending.is_some() {
                    let _ = pending.abort(&mut self.heap);
                    return Ok(self.fault(VmFault::CorruptLifecycle));
                }
                self.pending_record = Some(pending);
                self.string_collection_pending = Some(StringCollectionTarget::RecordResponse);
                self.collector.start();
            }
            Err(error) => {
                let _ = pending.abort(&mut self.heap);
                self.pending_record_source = None;
                return Ok(self.text_outcome(error));
            }
        }
        Ok(Outcome::SliceExhausted)
    }

    pub(super) fn begin_task_string_response(
        &mut self,
        task: TaskId,
        request: RequestId,
        empty: bool,
    ) -> Result<bool, VmFault> {
        if self.task_string_response.is_some() {
            return Err(VmFault::CorruptLifecycle);
        }
        let active = self.tasks.current().ok();
        if let Some(active) = active {
            self.save_task(active)?;
        }
        self.restore_task(task)?;
        if let Err(fault) = self.begin_capability_string_response(empty) {
            self.save_task(task)?;
            if let Some(active) = active {
                self.restore_task(active)?;
            }
            return Err(fault);
        }
        if empty {
            self.save_task(task)?;
            if let Some(active) = active {
                self.restore_task(active)?;
            }
            self.tasks
                .complete_host(task, request)
                .map_err(task_fault)?;
            if active.is_none() {
                self.activate_next_task()?;
            }
            return Ok(false);
        }
        self.task_string_response = Some((task, request, active));
        Ok(true)
    }

    pub(super) fn finish_task_string_response(&mut self) -> Result<(), VmFault> {
        if self.capability_string_response_pending() {
            return Err(VmFault::CorruptLifecycle);
        }
        let (task, request, active) = self
            .task_string_response
            .take()
            .ok_or(VmFault::CorruptLifecycle)?;
        self.save_task(task)?;
        if let Some(active) = active {
            self.restore_task(active)?;
        }
        self.tasks
            .complete_host(task, request)
            .map_err(task_fault)?;
        if active.is_none() {
            self.activate_next_task()?;
        }
        Ok(())
    }

    pub(super) fn has_task_string_response(&self) -> bool {
        self.task_string_response.is_some()
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

        let layout = array_layout(ValueWidth::Ref, count as i32, self.heap.header_format())
            .map_err(|_| {
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
            self.heap.zero_reserved_payload(
                reservation,
                0,
                layout.block_bytes - self.heap.payload_offset(),
            )?;
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
        self.run_slice_with_retirement_limit(guest_budget, maintenance_budget, u32::MAX)
    }

    pub(super) fn run_slice_with_retirement_limit(
        &mut self,
        guest_budget: u32,
        maintenance_budget: u32,
        retirement_limit: u32,
    ) -> Result<Outcome, RunError> {
        let attempts_before = self.executed_instructions;
        let pending_before = u64::from(self.has_pending_guest_instruction());
        let outcome = self.run_slice_inner(guest_budget, maintenance_budget, retirement_limit)?;
        if matches!(
            outcome,
            Outcome::AllocationExhausted(_) | Outcome::Crashed(_) | Outcome::Faulted(_)
        ) {
            self.capture_failure_stack();
        }
        let attempts = self.executed_instructions - attempts_before;
        let pending_after = u64::from(self.has_pending_guest_instruction());
        let completed = attempts + pending_before - pending_after;
        let faulting_instruction = matches!(
            outcome,
            Outcome::AllocationExhausted(_) | Outcome::Crashed(_) | Outcome::Faulted(_)
        ) && completed != 0;
        let retired = completed - u64::from(faulting_instruction);
        self.retired_instructions = self
            .retired_instructions
            .checked_add(retired)
            .expect("retired instruction count cannot exceed attempted count");
        Ok(outcome)
    }

    fn has_pending_guest_instruction(&self) -> bool {
        if let Some(pending) = self.pending_raise {
            return pending.retires_instruction;
        }
        if let Some(pending) = self.pending_exception {
            return pending.retires_instruction;
        }
        self.pending_allocation.is_some()
            || self.pending_array_copy.is_some()
            || self.pending_text.is_some()
            || self.pending_concat.is_some()
            || self.allocation_retry.is_some()
            || matches!(
                self.string_collection_pending,
                Some(StringCollectionTarget::Concat)
            )
    }

    fn run_slice_inner(
        &mut self,
        guest_budget: u32,
        maintenance_budget: u32,
        retirement_limit: u32,
    ) -> Result<Outcome, RunError> {
        match self.lifecycle {
            Lifecycle::Terminal(outcome) => return Ok(outcome),
            Lifecycle::Pristine => return Err(RunError::NotStarted),
            Lifecycle::Runnable => {}
        }
        let minimum = if self.pending_allocation.is_some()
            || self.pending_exception.is_some()
            || self.pending_raise.is_some()
        {
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
        if retirement_limit == 0 {
            return Ok(Outcome::SliceExhausted);
        }
        let attempts_before = self.executed_instructions;
        let attempt_limit = u64::from(retirement_limit)
            .saturating_sub(u64::from(self.has_pending_guest_instruction()));
        let mut remaining = guest_budget;
        'run: loop {
            if self.pending_raise.is_some() {
                if let Some(outcome) = self.resume_runtime_exception(&mut remaining) {
                    return Ok(outcome);
                }
            }
            if self.pending_exception.is_some() {
                match self.resume_exception(&mut remaining) {
                    Ok(Some(outcome)) => return Ok(outcome),
                    Ok(None) => {}
                    Err(fault) => return Ok(self.fault(fault)),
                }
            }
            let frame_index = self
                .frame_depth
                .checked_sub(1)
                .ok_or(RunError::NotRunnable)?;
            if self.pending_allocation.is_some() {
                if let Some(outcome) = self.resume_pending_allocation(frame_index, &mut remaining) {
                    return Ok(outcome);
                }
            }
            if self.pending_array_copy.is_some() {
                if let Some(outcome) = self.resume_pending_array_copy(frame_index, &mut remaining) {
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
            if self.executed_instructions - attempts_before >= attempt_limit {
                return Ok(Outcome::SliceExhausted);
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
                if self.executed_instructions - attempts_before >= attempt_limit {
                    return Ok(Outcome::SliceExhausted);
                }
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
                        let returning_function = self
                            .image
                            .function(self.frames[frame_index].function)
                            .ok_or(RunError::NotRunnable)?;
                        if returning_function.result.kind == 8 {
                            if frame_index == 0 || *value == u16::MAX {
                                return Ok(self.fault(VmFault::InvalidValueType));
                            }
                            let callee = self.frames[frame_index];
                            let caller = self.frames[frame_index - 1];
                            let caller_function = self
                                .image
                                .function(caller.function)
                                .ok_or(RunError::NotRunnable)?;
                            let source = returning_function
                                .frame_layout
                                .values
                                .get(*value as usize)
                                .ok_or(RunError::NotRunnable)?;
                            let destination = caller_function
                                .frame_layout
                                .values
                                .get(callee.destination as usize)
                                .ok_or(RunError::NotRunnable)?;
                            if let Err(fault) = self.frame_arena.copy_value(
                                callee.base,
                                source,
                                caller.base,
                                destination,
                            ) {
                                return Ok(self.fault(fault));
                            }
                            if let Err(fault) = self.frame_arena.pop(FrameReservation {
                                base: callee.base,
                                byte_len: callee.byte_len,
                            }) {
                                return Ok(self.fault(fault));
                            }
                            self.frames[frame_index] = Frame::EMPTY;
                            self.frame_depth = frame_index;
                            self.frames[frame_index - 1].block = callee.caller_block;
                            self.frames[frame_index - 1].instruction = callee.caller_instruction;
                            break;
                        }
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
                            let current = match self.tasks.current() {
                                Ok(current) => current,
                                Err(error) => return Ok(self.fault(task_fault(error))),
                            };
                            if current == TaskId::ROOT {
                                let outcome = Outcome::Halted(returned);
                                self.lifecycle = Lifecycle::Terminal(outcome);
                                self.tasks.cancel_all();
                                self.frame_depth = 0;
                                return Ok(outcome);
                            }
                            if returned.is_some() {
                                return Ok(self.fault(VmFault::InvalidValueType));
                            }
                            let completed_frame = self.frames[0];
                            if let Err(fault) = self.frame_arena.pop(FrameReservation {
                                base: completed_frame.base,
                                byte_len: completed_frame.byte_len,
                            }) {
                                return Ok(self.fault(fault));
                            }
                            self.frames[0] = Frame::EMPTY;
                            self.frame_depth = 0;
                            if let Err(error) = self.tasks.complete_current() {
                                return Ok(self.fault(task_fault(error)));
                            }
                            match self.activate_next_task() {
                                Ok(true) => continue 'run,
                                Ok(false) => return Ok(Outcome::TasksWaiting),
                                Err(fault) => return Ok(self.fault(fault)),
                            }
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
                    | ResolvedInstruction::CallVirtual { .. }
                    | ResolvedInstruction::CallInterface { .. }
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
                                ResolvedInstruction::CallVirtual {
                                    dst,
                                    declaration,
                                    args,
                                }
                                | ResolvedInstruction::CallInterface {
                                    dst,
                                    declaration,
                                    args,
                                } => {
                                    let receiver = match args.first().copied() {
                                        Some(receiver) => {
                                            match self.read_register(frame_index, receiver) {
                                                Ok(RuntimeValue::Reference(reference)) => reference,
                                                Ok(RuntimeValue::Null) => {
                                                    return Ok(
                                                        self.guest_trap(GuestTrap::NullReference)
                                                    );
                                                }
                                                Ok(_) => {
                                                    return Ok(self.fault(VmFault::InvalidValueType))
                                                }
                                                Err(fault) => return Ok(self.fault(fault)),
                                            }
                                        }
                                        None => return Ok(self.fault(VmFault::InvalidValueType)),
                                    };
                                    let actual = match self.reference_type(receiver) {
                                        Ok(actual) => actual,
                                        Err(fault) => return Ok(self.fault(fault)),
                                    };
                                    let Some(target) =
                                        self.image.dispatch_target(actual, *declaration)
                                    else {
                                        return Ok(self.fault(VmFault::InvalidResolvedId));
                                    };
                                    (
                                        *dst,
                                        target,
                                        args.as_ref(),
                                        block_index,
                                        instruction_index + 1,
                                    )
                                }
                                _ => unreachable!(),
                            };
                        let Some(target_function) = self.image.function(target) else {
                            return Ok(self.fault(VmFault::InvalidResolvedId));
                        };
                        if target_function.parameter_count != args.len() {
                            return Ok(self.fault(VmFault::InvalidValueType));
                        }
                        for source in args.iter() {
                            let caller_function = self
                                .image
                                .function(self.frames[frame_index].function)
                                .ok_or(RunError::NotRunnable)?;
                            if caller_function
                                .registers
                                .get(*source as usize)
                                .is_some_and(|value| value.kind == 8)
                            {
                                continue;
                            }
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
                            let caller = self.frames[frame_index];
                            let caller_function = self
                                .image
                                .function(caller.function)
                                .ok_or(RunError::NotRunnable)?;
                            if caller_function
                                .registers
                                .get(*source as usize)
                                .is_some_and(|value| value.kind == 8)
                            {
                                let source = caller_function
                                    .frame_layout
                                    .values
                                    .get(*source as usize)
                                    .ok_or(RunError::NotRunnable)?;
                                let destination = target_function
                                    .frame_layout
                                    .values
                                    .get(parameter)
                                    .ok_or(RunError::NotRunnable)?;
                                if let Err(fault) = self.frame_arena.copy_value(
                                    caller.base,
                                    source,
                                    callee.base,
                                    destination,
                                ) {
                                    let _ = self.frame_arena.pop(reservation);
                                    return Ok(self.fault(fault));
                                }
                                continue;
                            }
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
                            return Ok(self.guest_trap(GuestTrap::NegativeArraySize));
                        }
                        let RuntimeTypeLayout::Array { element } =
                            self.image.type_layout(*ty).ok_or(RunError::NotRunnable)?
                        else {
                            return Ok(self.fault(VmFault::InvalidResolvedId));
                        };
                        let layout = match array_layout(*element, length, self.heap.header_format())
                        {
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
                        if u64::from(request.block_bytes)
                            > self.image.storage_plan().heap_arena_bytes
                        {
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
                    ResolvedInstruction::ValueHash { form, dst, source } => {
                        let value = match self.read_register(frame_index, *source) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let Some(hash) = value.hash_code(*form) else {
                            return Ok(self.fault(VmFault::InvalidValueType));
                        };
                        if let Err(fault) =
                            self.write_register(frame_index, *dst, RuntimeValue::I32(hash))
                        {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::StringValueOf { form, dst, source } => {
                        let value = match self.read_register(frame_index, *source) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let conversion = if *form == 7 {
                            text::PendingConcat::reference_default(
                                &self.image,
                                &self.heap,
                                value,
                                *dst,
                            )
                        } else {
                            text::PendingConcat::scalar(value, *form, *dst)
                        };
                        let pending = match conversion {
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
                                return Ok(self.guest_trap(trap));
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
                        let value = match self.statics.read(static_slot, field.value_type.kind) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        if value == RuntimeValue::Null && !field.value_type.nullable {
                            return Ok(self.guest_trap(GuestTrap::NullReference));
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
                        if let Err(fault) =
                            self.statics
                                .write(static_slot, field.value_type.kind, value)
                        {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::FieldGet {
                        dst,
                        receiver,
                        field,
                    } => {
                        let receiver = match self.read_register(frame_index, *receiver) {
                            Ok(RuntimeValue::Null) => {
                                return Ok(self.guest_trap(GuestTrap::NullReference));
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
                            return Ok(self.guest_trap(GuestTrap::NullReference));
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
                                return Ok(self.guest_trap(GuestTrap::NullReference));
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
                                return Ok(self.guest_trap(GuestTrap::NullReference));
                            }
                            RuntimeValue::Null => {}
                            RuntimeValue::Reference(reference) => {
                                let actual = match self.reference_type(reference) {
                                    Ok(actual) => actual,
                                    Err(fault) => return Ok(self.fault(fault)),
                                };
                                if !self.image.is_assignable(actual, *ty) {
                                    return Ok(self.guest_trap(GuestTrap::ClassCast));
                                }
                            }
                            _ => return Ok(self.fault(VmFault::InvalidValueType)),
                        }
                        if let Err(fault) = self.write_register(frame_index, *dst, value) {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::ArrayCopy {
                        source,
                        destination,
                        source_start,
                        destination_start,
                        length,
                    } => {
                        let setup = (|| -> Result<PendingArrayCopy, InstructionFailure> {
                            let source = self
                                .read_register(frame_index, *source)
                                .map_err(InstructionFailure::Fault)?;
                            let destination = self
                                .read_register(frame_index, *destination)
                                .map_err(InstructionFailure::Fault)?;
                            let (source, source_ty, width, source_length) =
                                self.resolve_array(source)?;
                            let (
                                destination,
                                destination_ty,
                                destination_width,
                                destination_length,
                            ) = self.resolve_array(destination)?;
                            let source_element = self
                                .image
                                .array_element_type(source_ty)
                                .ok_or(InstructionFailure::Fault(VmFault::InvalidResolvedId))?;
                            let destination_element = self
                                .image
                                .array_element_type(destination_ty)
                                .ok_or(InstructionFailure::Fault(VmFault::InvalidResolvedId))?;
                            let assignable = source_element.kind == destination_element.kind
                                && (!source_element.nullable || destination_element.nullable)
                                && match (source_element.nominal, destination_element.nominal) {
                                    (Some(actual), Some(target)) => {
                                        self.image.is_assignable(actual, target)
                                    }
                                    (None, None) => true,
                                    _ => false,
                                };
                            if width != destination_width || !assignable {
                                return Err(InstructionFailure::Fault(VmFault::InvalidValueType));
                            }
                            let integer = |register| match self
                                .read_register(frame_index, register)
                                .map_err(InstructionFailure::Fault)?
                            {
                                RuntimeValue::I32(value) => Ok(value),
                                _ => Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
                            };
                            PendingArrayCopy::new(
                                (source, source_length),
                                (destination, destination_length),
                                integer(*source_start)?,
                                integer(*destination_start)?,
                                integer(*length)?,
                                width.bytes(),
                            )
                            .map_err(InstructionFailure::Trap)
                        })();
                        self.pending_array_copy = Some(match setup {
                            Ok(pending) => pending,
                            Err(InstructionFailure::Trap(trap)) => {
                                return Ok(self.guest_trap(trap));
                            }
                            Err(InstructionFailure::Fault(fault)) => return Ok(self.fault(fault)),
                        });
                        if let Some(outcome) =
                            self.resume_pending_array_copy(frame_index, &mut remaining)
                        {
                            return Ok(outcome);
                        }
                    }
                    ResolvedInstruction::ArrayLength { dst, array } => {
                        let array = match self.read_register(frame_index, *array) {
                            Ok(value) => value,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let (_, _, _, length) = match self.resolve_array(array) {
                            Ok(array) => array,
                            Err(InstructionFailure::Trap(trap)) => {
                                return Ok(self.guest_trap(trap));
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
                                return Ok(self.guest_trap(trap));
                            }
                            Err(InstructionFailure::Fault(fault)) => return Ok(self.fault(fault)),
                        };
                        let offset = match array_element_offset(index_value, length, element) {
                            Ok(offset) => offset,
                            Err(trap) => {
                                return Ok(self.guest_trap(trap));
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
                            return Ok(self.guest_trap(GuestTrap::NullReference));
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
                                return Ok(self.guest_trap(trap));
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
                                return Ok(self.guest_trap(trap));
                            }
                        };
                        if let Err(fault) =
                            store_value(&mut self.heap, reference, offset, element, value)
                        {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::TaskSpawn { dst, target, args } => {
                        let Some(target_function) = self.image.function(*target) else {
                            return Ok(self.fault(VmFault::InvalidResolvedId));
                        };
                        let reservation = match self.frame_arena.push(&target_function.frame_layout)
                        {
                            Ok(reservation) => reservation,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let child_frame = Frame {
                            function: *target,
                            base: reservation.base,
                            byte_len: reservation.byte_len,
                            block: target_function.first_block,
                            instruction: 0,
                            caller_block: usize::MAX,
                            caller_instruction: 0,
                            destination: u16::MAX,
                            initializer: None,
                        };
                        let caller = self.frames[frame_index];
                        let caller_function = self
                            .image
                            .function(caller.function)
                            .ok_or(RunError::NotRunnable)?;
                        for (parameter, source) in args.iter().enumerate() {
                            if caller_function
                                .registers
                                .get(*source as usize)
                                .is_some_and(|value| value.kind == 8)
                            {
                                let source = caller_function
                                    .frame_layout
                                    .values
                                    .get(*source as usize)
                                    .ok_or(RunError::NotRunnable)?;
                                let destination = target_function
                                    .frame_layout
                                    .values
                                    .get(parameter)
                                    .ok_or(RunError::NotRunnable)?;
                                if let Err(fault) = self.frame_arena.copy_value(
                                    caller.base,
                                    source,
                                    child_frame.base,
                                    destination,
                                ) {
                                    let _ = self.frame_arena.pop(reservation);
                                    return Ok(self.fault(fault));
                                }
                                continue;
                            }
                            let value = match self.read_register(frame_index, *source) {
                                Ok(value) => value,
                                Err(fault) => {
                                    let _ = self.frame_arena.pop(reservation);
                                    return Ok(self.fault(fault));
                                }
                            };
                            if let Err(fault) = write_frame_value(
                                &mut self.frame_arena,
                                child_frame,
                                target_function,
                                parameter as u16,
                                value,
                            ) {
                                let _ = self.frame_arena.pop(reservation);
                                return Ok(self.fault(fault));
                            }
                        }
                        let task = match self.tasks.spawn() {
                            Ok(task) => task,
                            Err(error) => {
                                let _ = self.frame_arena.pop(reservation);
                                return Ok(self.fault(task_fault(error)));
                            }
                        };
                        let Some(slot) = self.tasks.slot_of(task) else {
                            return Ok(self.fault(VmFault::CorruptLifecycle));
                        };
                        let start = match slot.checked_mul(self.image.maximum_call_depth()) {
                            Some(start) => start,
                            None => return Ok(self.fault(VmFault::InvalidStoragePlan)),
                        };
                        self.task_frames[start] = child_frame;
                        self.task_frame_depths[slot] = 1;
                        if let Err(fault) = self.write_register(
                            frame_index,
                            *dst,
                            RuntimeValue::I32(task.get() as i32),
                        ) {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::TaskJoin { task, resume_block } => {
                        let target = match self.read_register(frame_index, *task) {
                            Ok(RuntimeValue::I32(value)) if value > 0 => TaskId::new(value as u32),
                            Ok(_) => None,
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let Some(target) = target else {
                            return Ok(self.fault(VmFault::InvalidReference));
                        };
                        let current = match self.tasks.current() {
                            Ok(current) => current,
                            Err(error) => return Ok(self.fault(task_fault(error))),
                        };
                        match self.tasks.join(target) {
                            Ok(false) => {
                                let failure = self
                                    .tasks
                                    .slot_of(target)
                                    .and_then(|slot| self.task_failures.get(slot))
                                    .copied()
                                    .flatten();
                                if let Some(failure) = failure {
                                    let join_stack = self.failure_stack();
                                    let mut failure = failure;
                                    let available = MAXIMUM_FAILURE_FRAMES - failure.stack.length;
                                    let copied = available.min(join_stack.length);
                                    let start = failure.stack.length;
                                    failure.stack.frames[start..start + copied]
                                        .copy_from_slice(&join_stack.frames[..copied]);
                                    failure.stack.length += copied;
                                    failure.stack.omitted = failure
                                        .stack
                                        .omitted
                                        .saturating_add(join_stack.omitted)
                                        .saturating_add(join_stack.length - copied);
                                    if let Err(fault) = self.begin_exception(failure) {
                                        return Ok(self.fault(fault));
                                    }
                                    continue 'run;
                                }
                                self.frames[frame_index].block = *resume_block;
                                self.frames[frame_index].instruction = 0;
                                break;
                            }
                            Ok(true) => {
                                if let Err(fault) = self.save_task(current) {
                                    return Ok(self.fault(fault));
                                }
                                match self.activate_next_task() {
                                    Ok(true) => continue 'run,
                                    Ok(false) => return Ok(Outcome::TasksWaiting),
                                    Err(fault) => return Ok(self.fault(fault)),
                                }
                            }
                            Err(error) => return Ok(self.fault(task_fault(error))),
                        }
                    }
                    ResolvedInstruction::ChannelCreate { dst, capacity } => {
                        let destination = *dst;
                        let capacity_register = *capacity;
                        let capacity = match self.read_register(frame_index, capacity_register) {
                            Ok(RuntimeValue::I32(value)) => value,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let handle = match self.channels.create(capacity).map_err(channel_failure) {
                            Ok(handle) => handle,
                            Err(InstructionFailure::Trap(trap)) => {
                                return Ok(self.guest_trap(trap));
                            }
                            Err(InstructionFailure::Fault(fault)) => {
                                return Ok(self.fault(fault));
                            }
                        };
                        if let Err(fault) =
                            self.write_register(frame_index, destination, RuntimeValue::I32(handle))
                        {
                            return Ok(self.fault(fault));
                        }
                        self.frames[frame_index].instruction += 1;
                    }
                    ResolvedInstruction::ChannelSend {
                        channel,
                        value,
                        resume_block,
                    } => {
                        let channel_register = *channel;
                        let value_register = *value;
                        let continuation = *resume_block;
                        let handle = match self.read_register(frame_index, channel_register) {
                            Ok(RuntimeValue::I32(value)) => value,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let value = match self.read_register(frame_index, value_register) {
                            Ok(RuntimeValue::I32(value)) => value,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let current = match self.tasks.current() {
                            Ok(current) => current,
                            Err(error) => return Ok(self.fault(task_fault(error))),
                        };
                        let Some(slot) = self.tasks.slot_of(current) else {
                            return Ok(self.fault(VmFault::CorruptLifecycle));
                        };
                        let result = match self
                            .channels
                            .send(handle, slot, current, value, continuation)
                            .map_err(channel_failure)
                        {
                            Ok(result) => result,
                            Err(InstructionFailure::Trap(trap)) => {
                                return Ok(self.guest_trap(trap));
                            }
                            Err(InstructionFailure::Fault(fault)) => {
                                return Ok(self.fault(fault));
                            }
                        };
                        match result {
                            SendResult::Complete => {}
                            SendResult::WakeReceiver {
                                task,
                                destination,
                                resume_block,
                            } => {
                                if let Err(fault) = self.resume_saved_channel_task(
                                    task,
                                    handle as u32,
                                    resume_block,
                                    Some((destination, value)),
                                ) {
                                    return Ok(self.fault(fault));
                                }
                            }
                            SendResult::Suspend => {
                                if let Err(error) = self.tasks.suspend_channel(handle as u32) {
                                    return Ok(self.fault(task_fault(error)));
                                }
                                if let Err(fault) = self.save_task(current) {
                                    return Ok(self.fault(fault));
                                }
                                match self.activate_next_task() {
                                    Ok(true) => continue 'run,
                                    Ok(false) => return Ok(Outcome::TasksWaiting),
                                    Err(fault) => return Ok(self.fault(fault)),
                                }
                            }
                        }
                        self.frames[frame_index].block = continuation;
                        self.frames[frame_index].instruction = 0;
                        break;
                    }
                    ResolvedInstruction::ChannelReceive {
                        dst,
                        channel,
                        resume_block,
                    } => {
                        let destination = *dst;
                        let channel_register = *channel;
                        let continuation = *resume_block;
                        let handle = match self.read_register(frame_index, channel_register) {
                            Ok(RuntimeValue::I32(value)) => value,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let current = match self.tasks.current() {
                            Ok(current) => current,
                            Err(error) => return Ok(self.fault(task_fault(error))),
                        };
                        let Some(slot) = self.tasks.slot_of(current) else {
                            return Ok(self.fault(VmFault::CorruptLifecycle));
                        };
                        let result = match self
                            .channels
                            .receive(handle, slot, current, destination, continuation)
                            .map_err(channel_failure)
                        {
                            Ok(result) => result,
                            Err(InstructionFailure::Trap(trap)) => {
                                return Ok(self.guest_trap(trap));
                            }
                            Err(InstructionFailure::Fault(fault)) => {
                                return Ok(self.fault(fault));
                            }
                        };
                        match result {
                            ReceiveResult::Value { value, wake_sender } => {
                                if let Some((task, sender_resume_block)) = wake_sender {
                                    if let Err(fault) = self.resume_saved_channel_task(
                                        task,
                                        handle as u32,
                                        sender_resume_block,
                                        None,
                                    ) {
                                        return Ok(self.fault(fault));
                                    }
                                }
                                if let Err(fault) = self.write_register(
                                    frame_index,
                                    destination,
                                    RuntimeValue::I32(value),
                                ) {
                                    return Ok(self.fault(fault));
                                }
                            }
                            ReceiveResult::Suspend => {
                                if let Err(error) = self.tasks.suspend_channel(handle as u32) {
                                    return Ok(self.fault(task_fault(error)));
                                }
                                if let Err(fault) = self.save_task(current) {
                                    return Ok(self.fault(fault));
                                }
                                match self.activate_next_task() {
                                    Ok(true) => continue 'run,
                                    Ok(false) => return Ok(Outcome::TasksWaiting),
                                    Err(fault) => return Ok(self.fault(fault)),
                                }
                            }
                        }
                        self.frames[frame_index].block = continuation;
                        self.frames[frame_index].instruction = 0;
                        break;
                    }
                    ResolvedInstruction::CapabilityCallSync { .. }
                    | ResolvedInstruction::CapabilityCallAsync { .. } => {
                        return Ok(Outcome::HostRequest);
                    }
                    ResolvedInstruction::Throw { exception } => {
                        let reference = match self.read_register(frame_index, *exception) {
                            Ok(RuntimeValue::Reference(reference)) => reference,
                            Ok(_) => return Ok(self.fault(VmFault::InvalidValueType)),
                            Err(fault) => return Ok(self.fault(fault)),
                        };
                        let failure = TaskFailure {
                            exception: reference,
                            stack: self.failure_stack(),
                        };
                        if let Err(fault) = self.begin_exception(failure) {
                            return Ok(self.fault(fault));
                        }
                        continue 'run;
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
                                return Ok(self.guest_trap(trap));
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

    fn begin_exception(&mut self, failure: TaskFailure) -> Result<(), VmFault> {
        if self.pending_exception.is_some() {
            return Err(VmFault::CorruptLifecycle);
        }
        let actual_type = self.reference_type(failure.exception)?;
        self.pending_exception = Some(PendingException {
            failure,
            actual_type,
            next_handler: 0,
            retires_instruction: true,
        });
        Ok(())
    }

    fn begin_runtime_exception(&mut self, tag: u8, detail: &str) -> Result<(), VmFault> {
        if self.pending_raise.is_some()
            || self.pending_exception.is_some()
            || detail.len() > super::host::MAXIMUM_HOST_FAILURE_DETAIL_BYTES
        {
            return Err(VmFault::CorruptLifecycle);
        }
        let ty = self
            .image
            .runtime_exception_type(tag)
            .ok_or(VmFault::InvalidResolvedId)?;
        let mut units = [0; super::host::MAXIMUM_HOST_FAILURE_DETAIL_BYTES];
        let mut length = 0;
        for unit in detail.encode_utf16() {
            units[length] = unit;
            length += 1;
        }
        self.pending_raise = Some(PendingRaise {
            ty,
            units,
            length,
            text: text::PendingHostString::new(u16::MAX),
            message: None,
            exception: None,
            stack: self.failure_stack(),
            retires_instruction: true,
        });
        Ok(())
    }

    fn resume_runtime_exception(&mut self, remaining: &mut u32) -> Option<Outcome> {
        let mut pending = self.pending_raise.take()?;
        if pending.message.is_none() {
            let result = pending.text.resume(
                &self.image,
                &mut self.heap,
                &pending.units[..pending.length],
                *remaining,
            );
            let (used, value) = match result {
                Ok(result) => result,
                Err(text::TextError::Exhausted {
                    used,
                    block_bytes,
                    requested,
                    collection_attempted,
                }) => {
                    let Some(consumed) = self.consumed_dynamic_cost.checked_add(u64::from(used))
                    else {
                        let _ = pending.text.abort(&mut self.heap);
                        return Some(self.fault(VmFault::AccountingOverflow));
                    };
                    self.consumed_dynamic_cost = consumed;
                    *remaining -= used;
                    if collection_attempted
                        || u64::from(block_bytes) > self.image.storage_plan().heap_arena_bytes
                    {
                        let _ = pending.text.abort(&mut self.heap);
                        return Some(self.allocation_exhausted(
                            AllocationRequestKind::String,
                            requested,
                            collection_attempted,
                            pending.stack.frames[0],
                        ));
                    }
                    self.pending_raise = Some(pending);
                    self.string_collection_pending = Some(StringCollectionTarget::ExceptionMessage);
                    self.collector.start();
                    return Some(Outcome::SliceExhausted);
                }
                Err(text::TextError::Fault(fault)) => {
                    let _ = pending.text.abort(&mut self.heap);
                    return Some(self.fault(fault));
                }
                Err(text::TextError::Trap(_)) => {
                    let _ = pending.text.abort(&mut self.heap);
                    return Some(self.fault(VmFault::CorruptLifecycle));
                }
            };
            let Some(consumed) = self.consumed_dynamic_cost.checked_add(u64::from(used)) else {
                let _ = pending.text.abort(&mut self.heap);
                return Some(self.fault(VmFault::AccountingOverflow));
            };
            self.consumed_dynamic_cost = consumed;
            *remaining -= used;
            match value {
                Some((_, RuntimeValue::Reference(reference))) => pending.message = Some(reference),
                Some(_) => return Some(self.fault(VmFault::InvalidValueType)),
                None => {
                    self.pending_raise = Some(pending);
                    return Some(Outcome::SliceExhausted);
                }
            }
        }
        if pending.exception.is_none() {
            if self.pending_allocation.is_none() {
                let Some(RuntimeTypeLayout::Object(layout)) = self.image.type_layout(pending.ty)
                else {
                    return Some(self.fault(VmFault::InvalidStoragePlan));
                };
                let Some(type_id) = self.image.type_id(pending.ty) else {
                    return Some(self.fault(VmFault::InvalidResolvedId));
                };
                let retry = AllocationRetry {
                    request: AllocationRequest {
                        block_bytes: layout.block_bytes,
                        type_id,
                    },
                    destination: u16::MAX,
                    logical_bytes: layout.payload_bytes,
                    shape: AllocationShape::Exception,
                    source: pending.stack.frames[0],
                };
                match retry.reserve(&mut self.heap, false) {
                    Ok(Some(allocation)) => self.pending_allocation = Some(allocation),
                    Ok(None) => {
                        self.pending_raise = Some(pending);
                        self.allocation_retry = Some(retry);
                        self.collector.start();
                        return Some(Outcome::SliceExhausted);
                    }
                    Err(fault) => return Some(self.fault(fault)),
                }
            }
            self.pending_raise = Some(pending);
            let frame = self.frame_depth.checked_sub(1)?;
            if let Some(outcome) = self.resume_pending_allocation(frame, remaining) {
                return Some(outcome);
            }
            pending = self.pending_raise.take()?;
        }
        if *remaining == 0 {
            self.pending_raise = Some(pending);
            return Some(Outcome::SliceExhausted);
        }
        *remaining -= 1;
        let Some(consumed) = self.consumed_dynamic_cost.checked_add(1) else {
            return Some(self.fault(VmFault::AccountingOverflow));
        };
        self.consumed_dynamic_cost = consumed;
        let Some(exception) = pending.exception else {
            return Some(self.fault(VmFault::CorruptLifecycle));
        };
        let Some(message) = pending.message else {
            return Some(self.fault(VmFault::CorruptLifecycle));
        };
        if let Err(fault) = store_value(
            &mut self.heap,
            exception,
            0,
            ValueWidth::Ref,
            RuntimeValue::Reference(message),
        ) {
            return Some(self.fault(fault));
        }
        if let Err(fault) = self.begin_exception(TaskFailure {
            exception,
            stack: pending.stack,
        }) {
            return Some(self.fault(fault));
        }
        self.pending_exception.as_mut()?.retires_instruction = pending.retires_instruction;
        None
    }

    pub(crate) fn exception_diagnostic(
        &self,
        artifact: &crate::artifact::DecodedArtifact,
    ) -> String {
        let Some(pending) = self.pending_exception else {
            return String::new();
        };
        let mut result = String::from("Uncaught exception: ");
        let mut reference = pending.failure.exception;
        let mut seen = [None; 4];
        for index in 0..seen.len() {
            if seen[..index].contains(&Some(reference)) {
                result.push_str("[cyclic cause]");
                break;
            }
            seen[index] = Some(reference);
            let Ok(ty) = self.reference_type(reference) else {
                result.push_str("[invalid exception]");
                break;
            };
            let module = &artifact.modules[ty.module as usize];
            let crate::artifact::NominalType::Class { name, .. } = module.types[ty.ty as usize]
            else {
                break;
            };
            let name = module.strings[name as usize].slice(&artifact.bytes);
            // Metadata and Guest text are bounded and cannot inject diagnostic control characters.
            result.extend(String::from_utf8_lossy(name).chars().take(256).map(|c| {
                if c.is_control() {
                    ' '
                } else {
                    c
                }
            }));
            if let Ok(value @ RuntimeValue::Reference(_)) =
                load_value(&self.heap, reference, 0, ValueWidth::Ref)
            {
                if let Ok(backing) = text::backing(&self.image, &self.heap, value) {
                    result.push_str(": ");
                    let units = (0..backing.length().min(256)).map(|i| {
                        text::code_unit(&self.image, &self.heap, backing, i).unwrap_or(0xfffd)
                    });
                    result.extend(
                        char::decode_utf16(units)
                            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                            .map(|c| if c.is_control() { ' ' } else { c }),
                    );
                    if backing.length() > 256 {
                        result.push('…');
                    }
                }
            }
            match load_value(&self.heap, reference, 4, ValueWidth::Ref) {
                Ok(RuntimeValue::Reference(cause)) => {
                    result.push_str("\nCaused by: ");
                    reference = cause;
                    if index == seen.len() - 1 {
                        result.push_str("[further causes omitted]");
                    }
                }
                _ => break,
            }
        }
        result
    }

    pub(super) fn owns_artifact(&self, artifact: &VerifiedArtifact) -> bool {
        self.image.content_hash() == artifact.content_hash()
    }

    fn resume_exception(&mut self, remaining: &mut u32) -> Result<Option<Outcome>, VmFault> {
        while let Some(pending) = self.pending_exception {
            if *remaining == 0 {
                return Ok(Some(Outcome::SliceExhausted));
            }
            *remaining -= 1;
            self.consumed_dynamic_cost = self
                .consumed_dynamic_cost
                .checked_add(1)
                .ok_or(VmFault::AccountingOverflow)?;
            let index = self
                .frame_depth
                .checked_sub(1)
                .ok_or(VmFault::CorruptLifecycle)?;
            let frame = self.frames[index];
            let function = self
                .image
                .function(frame.function)
                .ok_or(VmFault::InvalidResolvedId)?;
            if let Some(handler) = function.handlers.get(pending.next_handler).copied() {
                self.pending_exception
                    .as_mut()
                    .ok_or(VmFault::CorruptLifecycle)?
                    .next_handler += 1;
                if (handler.protected_start..handler.protected_end).contains(&frame.block)
                    && handler
                        .catch_type
                        .is_none_or(|catch| self.image.is_assignable(pending.actual_type, catch))
                {
                    self.write_register(
                        index,
                        handler.exception_register,
                        RuntimeValue::Reference(pending.failure.exception),
                    )?;
                    self.frames[index].block = handler.handler_block;
                    self.frames[index].instruction = 0;
                    self.pending_exception = None;
                    return Ok(None);
                }
                continue;
            }
            self.frame_arena.pop(FrameReservation {
                base: frame.base,
                byte_len: frame.byte_len,
            })?;
            self.frames[index] = Frame::EMPTY;
            self.frame_depth = index;
            if let Some(ty) = frame.initializer {
                let ty = self
                    .image
                    .type_index(ty)
                    .ok_or(VmFault::InvalidResolvedId)?;
                self.type_initialization[ty] = TypeInitializationState::Failed;
            }
            if index != 0 {
                // The caller remains at its original call instruction, not its return continuation.
                self.pending_exception
                    .as_mut()
                    .ok_or(VmFault::CorruptLifecycle)?
                    .next_handler = 0;
                continue;
            }
            let task = self.tasks.current().map_err(task_fault)?;
            if task == TaskId::ROOT {
                self.failure_stack = Some(pending.failure.stack);
                let outcome = Outcome::UncaughtException;
                self.lifecycle = Lifecycle::Terminal(outcome);
                self.tasks.cancel_all();
                return Ok(Some(outcome));
            }
            let slot = self.tasks.slot_of(task).ok_or(VmFault::CorruptLifecycle)?;
            self.task_failures[slot] = Some(pending.failure);
            self.pending_exception = None;
            self.tasks.complete_current().map_err(task_fault)?;
            return Ok(if self.activate_next_task()? {
                None
            } else {
                Some(Outcome::TasksWaiting)
            });
        }
        Ok(None)
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
        self.capture_failure_stack();
        self.cancel_pending_allocation();
        self.cancel_pending_concat();
        self.cancel_pending_host_string();
        if let Some(pending) = self.pending_raise.take() {
            let _ = pending.text.abort(&mut self.heap);
        }
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
        self.capture_failure_stack();
        // Deferred allocation work retains the precise original instruction.
        if let Some(stack) = &mut self.failure_stack {
            if stack.length > 0 {
                stack.frames[0] = source;
            }
        }
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
                    .heap_arena_bytes
                    .try_into()
                    .unwrap_or(u32::MAX)
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

    fn capture_failure_stack(&mut self) {
        if self.failure_stack.is_none() {
            self.failure_stack = Some(self.failure_stack());
        }
    }

    pub(crate) fn failure_stack(&self) -> FailureStack {
        if let Some(stack) = self.failure_stack {
            return stack;
        }
        let empty = AllocationSource {
            module: u32::MAX,
            function: u32::MAX,
            block: u32::MAX,
            instruction: u32::MAX,
        };
        let depth = self.frame_depth.min(self.frames.len());
        let length = depth.min(MAXIMUM_FAILURE_FRAMES);
        let mut stack = FailureStack {
            frames: [empty; MAXIMUM_FAILURE_FRAMES],
            length,
            omitted: self.frame_depth.saturating_sub(length),
        };
        for (index, target) in stack.frames[..length].iter_mut().enumerate() {
            *target = self.allocation_source(depth - index - 1);
        }
        stack
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
            let mut runtime_roots = [None; 41];
            let mut runtime_root_count = 0_usize;
            self.visit_runtime_roots(|reference| {
                if let Some(slot) = runtime_roots.get_mut(runtime_root_count) {
                    *slot = Some(reference);
                }
                runtime_root_count += 1;
            });
            if runtime_root_count > runtime_roots.len() {
                return Ok(self.fault(VmFault::InvalidStoragePlan));
            }
            match self.collector.step(
                &mut self.heap,
                &self.image,
                RootSet {
                    statics: &self.statics,
                    frames: &self.frames,
                    saved_frames: &self.task_frames,
                    task_failures: &self.task_failures,
                    frame_arena: &self.frame_arena,
                    frame_depth: self.frame_depth,
                    runtime_roots: &runtime_roots[..runtime_root_count],
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
                    StringCollectionTarget::RecordResponse => {
                        let Some(pending) = self.pending_record.as_mut() else {
                            return Ok(self.fault(VmFault::CorruptLifecycle));
                        };
                        pending.mark_collection_attempted();
                    }
                    StringCollectionTarget::ExceptionMessage => {
                        let Some(pending) = self.pending_raise.as_mut() else {
                            return Ok(self.fault(VmFault::CorruptLifecycle));
                        };
                        pending.text.mark_collection_attempted();
                    }
                }
                return Ok(Outcome::SliceExhausted);
            }
            let Some(retry) = self.allocation_retry.take() else {
                return Ok(self.fault(VmFault::CorruptLifecycle));
            };
            match retry.reserve(&mut self.heap, true) {
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
        let is_exception = matches!(pending, PendingAllocation::Exception(_));
        let destination = pending.state().destination;
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
            // A successful reservation after collection still initializes incrementally.
            self.pending_allocation = Some(pending);
            return Some(Outcome::SliceExhausted);
        };
        if is_exception {
            let Some(raise) = self.pending_raise.as_mut() else {
                return Some(self.fault(VmFault::CorruptLifecycle));
            };
            raise.exception = Some(reference);
            return None;
        }
        if let Err(fault) =
            self.write_register(frame_index, destination, RuntimeValue::Reference(reference))
        {
            return Some(self.fault(fault));
        }
        self.frames[frame_index].instruction += 1;
        None
    }

    fn resume_pending_array_copy(
        &mut self,
        frame_index: usize,
        remaining: &mut u32,
    ) -> Option<Outcome> {
        let mut pending = self.pending_array_copy.take()?;
        let (used, done) = match pending.advance(&mut self.heap, *remaining) {
            Ok(result) => result,
            Err(fault) => return Some(self.fault(fault)),
        };
        let Some(consumed) = self.consumed_dynamic_cost.checked_add(u64::from(used)) else {
            return Some(self.fault(VmFault::AccountingOverflow));
        };
        self.consumed_dynamic_cost = consumed;
        *remaining -= used;
        if !done {
            self.pending_array_copy = Some(pending);
            return Some(Outcome::SliceExhausted);
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
                if u64::from(block_bytes) > self.image.storage_plan().heap_arena_bytes {
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
                self.guest_trap(trap)
            }
            text::TextError::Fault(fault) => self.fault(fault),
            text::TextError::Exhausted { .. } => self.fault(VmFault::CorruptLifecycle),
        }
    }

    fn guest_trap(&mut self, trap: GuestTrap) -> Outcome {
        let (role, message) = match trap {
            GuestTrap::DivisionByZero => (1, "/ by zero"),
            GuestTrap::IndexOutOfBounds => (2, "Index out of bounds"),
            GuestTrap::NegativeArraySize => (3, "Negative array size"),
            GuestTrap::NullReference => (4, "Null reference"),
            GuestTrap::ClassCast => (5, "Invalid cast"),
            GuestTrap::InvalidArgument => (6, "Invalid argument"),
            GuestTrap::InvalidExitCode => (6, "Invalid exit code"),
            GuestTrap::StackOverflow => {
                let outcome = Outcome::Crashed(trap);
                self.lifecycle = Lifecycle::Terminal(outcome);
                return outcome;
            }
        };
        match self.begin_runtime_exception(role, message) {
            Ok(()) => Outcome::SliceExhausted,
            Err(fault) => self.fault(fault),
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

    fn visit_runtime_roots(&self, mut visit: impl FnMut(Ref32)) {
        if let Some(pending) = &self.pending_record {
            pending.visit_roots(&mut visit);
        }
        if let Some(pending) = &self.pending_raise {
            if let Some(message) = pending.message {
                visit(message);
            }
            if let Some(exception) = pending.exception {
                visit(exception);
            }
        }
        if let Some(pending) = self.pending_exception {
            visit(pending.failure.exception);
        }
        if let Some(pending) = self.pending_array_copy {
            visit(pending.source);
            visit(pending.destination);
        }
        if let Some(pending) = self.pending_text {
            pending.visit_roots(&mut visit);
        }
        if let Some(pending) = self.pending_concat {
            pending.visit_roots(&mut visit);
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
                if u64::from(block_bytes) > self.image.storage_plan().heap_arena_bytes
                    || collection_attempted
                {
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
        if self.trace_enabled {
            trace_field(&mut self.trace, bytes);
        }
    }

    pub(super) fn entered_blocks(&self) -> u64 {
        self.entered_blocks
    }

    pub(super) fn executed_instructions(&self) -> u64 {
        self.executed_instructions
    }

    pub(super) fn retired_instructions(&self) -> u64 {
        self.retired_instructions
    }

    fn trace_block_entry(
        &mut self,
        frame_index: usize,
        block_index: usize,
        remaining: u32,
    ) -> Result<(), RunError> {
        if !self.trace_enabled {
            return Ok(());
        }
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
            let safepoint = self.image.safepoint_map(
                frame.function,
                frame.block,
                u32::try_from(frame.instruction).map_err(|_| RunError::NotRunnable)?,
            );
            trace_field(
                &mut self.trace,
                &u32::try_from(active_function.register_count)
                    .map_err(|_| RunError::NotRunnable)?
                    .to_le_bytes(),
            );
            for register in 0..active_function.register_count {
                if active_function.registers[register].kind == 8 {
                    let layout = active_function
                        .frame_layout
                        .values
                        .get(register)
                        .ok_or(RunError::NotRunnable)?;
                    let nominal = active_function.registers[register]
                        .nominal
                        .ok_or(RunError::NotRunnable)?;
                    trace_field(&mut self.trace, &[2]);
                    trace_field(&mut self.trace, &nominal.module.to_le_bytes());
                    trace_field(&mut self.trace, &nominal.ty.to_le_bytes());
                    trace_field(
                        &mut self.trace,
                        &(layout.components.len() as u32).to_le_bytes(),
                    );
                    for component in &layout.components {
                        if component.atom == crate::artifact::PhysicalAtom::Ref32
                            && safepoint.is_some_and(|map| {
                                !map.reference_offsets.contains(&component.offset)
                            })
                        {
                            trace_register(&mut self.trace, None, None)?;
                            continue;
                        }
                        let value = self
                            .frame_arena
                            .read_component_value(frame.base, *component)
                            .map_err(|_| RunError::NotRunnable)?;
                        let reference_type = match value {
                            RuntimeValue::Reference(reference) => Some(
                                self.reference_type(reference)
                                    .map_err(|_| RunError::NotRunnable)?,
                            ),
                            _ => None,
                        };
                        trace_register(&mut self.trace, Some(value), reference_type)?;
                    }
                    continue;
                }
                if let Some(map) = safepoint {
                    let register_layout = active_function
                        .frame_layout
                        .values
                        .get(register)
                        .ok_or(RunError::NotRunnable)?;
                    // Dead reference slots may still contain IDs reclaimed by the collector.
                    if active_function.registers[register].kind == 7
                        && register_layout.components.first().is_some_and(|component| {
                            !map.reference_offsets.contains(&component.offset)
                        })
                    {
                        trace_register(&mut self.trace, None, None)?;
                        continue;
                    }
                }
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
                trace_register(&mut self.trace, Some(value), reference_type)?;
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

    pub(super) fn resource_snapshot(&self) -> MachineResourceSnapshot {
        let heap_capacity_bytes = self.image.storage_plan().heap_arena_bytes;
        let tasks = self.tasks.snapshot();
        MachineResourceSnapshot {
            heap_capacity_bytes,
            heap_used_bytes: heap_capacity_bytes
                .saturating_sub(u64::from(self.heap.total_free_bytes())),
            live_objects: u64::from(self.heap.live_objects()),
            mutable_resident_bytes: self
                .image
                .storage_plan()
                .mutable_resident_bytes()
                .saturating_add(
                    self.pending_record
                        .as_ref()
                        .map_or(0, |pending| pending.resident_bytes()),
                ),
            task_capacity: tasks.capacity,
            live_tasks: tasks.live,
            runnable_tasks: tasks.runnable,
            suspended_tasks: tasks.suspended,
            completed_tasks: tasks.completed,
        }
    }

    #[cfg(test)]
    pub(super) fn test_peak_active_frame_bytes(&self) -> u32 {
        self.frame_arena.peak_active_bytes()
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
    pub(super) fn test_exception_message(&self) -> String {
        let reference = self.pending_exception.unwrap().failure.exception;
        let RuntimeValue::Reference(message) =
            load_value(&self.heap, reference, 0, ValueWidth::Ref).unwrap()
        else {
            panic!("factory message is missing");
        };
        let units: Vec<u16> = (0..self.string_length(message))
            .map(|index| self.string_get(message, index))
            .collect();
        String::from_utf16(&units).unwrap()
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
    pub(super) fn test_record_collection_safe(&self) -> bool {
        self.pending_record
            .as_ref()
            .is_some_and(|record| record.test_collection_safe())
    }

    #[cfg(test)]
    pub(super) fn test_collect_exception_roots(&mut self) -> Result<(), VmFault> {
        let mut runtime_roots = [None; 41];
        let mut count = 0;
        self.visit_runtime_roots(|reference| {
            runtime_roots[count] = Some(reference);
            count += 1;
        });
        self.collector.start();
        while self.collector.is_active() {
            self.collector.step(
                &mut self.heap,
                &self.image,
                RootSet {
                    statics: &self.statics,
                    frames: &self.frames,
                    saved_frames: &self.task_frames,
                    task_failures: &self.task_failures,
                    frame_arena: &self.frame_arena,
                    frame_depth: self.frame_depth,
                    runtime_roots: &runtime_roots[..count],
                    external: &self.external_roots,
                },
            )?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn test_has_exception_work(&self) -> bool {
        self.pending_exception.is_some() || self.task_failures.iter().any(Option::is_some)
    }

    #[cfg(test)]
    pub(super) fn test_factory_payload_published(&self) -> bool {
        self.pending_raise
            .is_some_and(|pending| pending.exception.is_some())
    }

    #[cfg(test)]
    pub(super) fn test_remove_emergency_oom(&mut self) {
        self.emergency_oom = None;
    }

    #[cfg(test)]
    pub(super) fn test_reserved_bytes(&self) -> usize {
        core::mem::size_of::<Self>()
            + self.frames.len() * core::mem::size_of::<Frame>()
            + self.task_frames.len() * core::mem::size_of::<Frame>()
            + self.task_frame_depths.len() * core::mem::size_of::<usize>()
            + self.task_failures.len() * core::mem::size_of::<Option<TaskFailure>>()
            + self.task_host_failures.len() * core::mem::size_of::<Option<PendingHostFailure>>()
            + self.tasks.reserved_bytes()
            + self.channels.reserved_bytes()
            + self.frame_arena.reserved_bytes()
            + self.statics.reserved_bytes()
            + self.type_initialization.len() * core::mem::size_of::<TypeInitializationState>()
            + self.heap.test_reserved_bytes()
            + self.external_roots.reserved_bytes()
    }

    #[cfg(test)]
    pub(super) fn test_snapshot(&self) -> (u8, usize, Box<[Option<RuntimeValue>]>) {
        let width = self.image.registers_per_frame();
        let mut registers = vec![None; self.frames.len() * width];
        for frame_index in 0..self.frame_depth {
            let frame = self.frames[frame_index];
            let Some(function) = self.image.function(frame.function) else {
                continue;
            };
            for register in 0..function.register_count {
                let flat = frame_index * width + register;
                if let Some(value) = self.test_register(flat) {
                    registers[flat] = Some(value);
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
    pub(super) fn test_active_registers(&self) -> Box<[Option<RuntimeValue>]> {
        let width = self.image.registers_per_frame();
        let base = self.frame_depth.saturating_sub(1) * width;
        (0..width)
            .map(|register| self.test_register(base + register))
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
    register: Option<RuntimeValue>,
    reference_type: Option<super::TypeKey>,
) -> Result<(), RunError> {
    match register {
        None => trace_field(trace, &[0]),
        Some(value) => {
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
        ResolvedInstruction::InlineConstruct { dst, components } => {
            let destination = function
                .frame_layout
                .values
                .get(*dst as usize)
                .ok_or(InstructionFailure::Fault(VmFault::InvalidValueType))?;
            if components.len() != destination.components.len() {
                return Err(InstructionFailure::Fault(VmFault::InvalidValueType));
            }
            for (source, destination) in components.iter().zip(destination.components.iter()) {
                let source = function
                    .frame_layout
                    .values
                    .get(*source as usize)
                    .and_then(|layout| layout.components.first())
                    .ok_or(InstructionFailure::Fault(VmFault::InvalidValueType))?;
                arena
                    .copy_component(frame.base, source, frame.base, destination)
                    .map_err(InstructionFailure::Fault)?;
            }
            Ok(())
        }
        ResolvedInstruction::InlineComponent {
            dst,
            src,
            component,
        } => {
            let source = function
                .frame_layout
                .values
                .get(*src as usize)
                .and_then(|layout| layout.components.get(*component as usize))
                .ok_or(InstructionFailure::Fault(VmFault::InvalidValueType))?;
            let destination = function
                .frame_layout
                .values
                .get(*dst as usize)
                .and_then(|layout| layout.components.first())
                .ok_or(InstructionFailure::Fault(VmFault::InvalidValueType))?;
            arena
                .copy_component(frame.base, source, frame.base, destination)
                .map_err(InstructionFailure::Fault)
        }
        ResolvedInstruction::Nop => Ok(()),
        ResolvedInstruction::Move { dst, src } => {
            if function
                .registers
                .get(*src as usize)
                .is_some_and(|value| value.kind != 8)
            {
                let value = read_frame_value(arena, frame, function, *src)
                    .map_err(InstructionFailure::Fault)?;
                return write_frame_value(arena, frame, function, *dst, value)
                    .map_err(InstructionFailure::Fault);
            }
            let source = function
                .frame_layout
                .values
                .get(*src as usize)
                .ok_or(InstructionFailure::Fault(VmFault::InvalidValueType))?;
            let destination = function
                .frame_layout
                .values
                .get(*dst as usize)
                .ok_or(InstructionFailure::Fault(VmFault::InvalidValueType))?;
            arena
                .copy_value(frame.base, source, frame.base, destination)
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
        ResolvedInstruction::Convert { form, dst, src } => {
            let destination = function
                .registers
                .get(*dst as usize)
                .ok_or(InstructionFailure::Fault(VmFault::InvalidValueType))?
                .kind;
            let source = read_frame_value(arena, frame, function, *src)
                .map_err(InstructionFailure::Fault)?;
            let value = convert_with_signedness(source, destination, *form)?;
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
        (8, RuntimeValue::I32(a), RuntimeValue::I32(b)) => {
            let (a, b) = (a as u32, b as u32);
            Ok(RuntimeValue::I32(match operation {
                Arithmetic::Add => a.wrapping_add(b),
                Arithmetic::Sub => a.wrapping_sub(b),
                Arithmetic::Mul => a.wrapping_mul(b),
                Arithmetic::Div => a
                    .checked_div(b)
                    .ok_or(InstructionFailure::Trap(GuestTrap::DivisionByZero))?,
                Arithmetic::Rem => a
                    .checked_rem(b)
                    .ok_or(InstructionFailure::Trap(GuestTrap::DivisionByZero))?,
            } as i32))
        }
        (9, RuntimeValue::I64(a), RuntimeValue::I64(b)) => {
            let (a, b) = (a as u64, b as u64);
            Ok(RuntimeValue::I64(match operation {
                Arithmetic::Add => a.wrapping_add(b),
                Arithmetic::Sub => a.wrapping_sub(b),
                Arithmetic::Mul => a.wrapping_mul(b),
                Arithmetic::Div => a
                    .checked_div(b)
                    .ok_or(InstructionFailure::Trap(GuestTrap::DivisionByZero))?,
                Arithmetic::Rem => a
                    .checked_rem(b)
                    .ok_or(InstructionFailure::Trap(GuestTrap::DivisionByZero))?,
            } as i64))
        }
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
        (8, RuntimeValue::I32(a), RuntimeValue::I32(b)) => ordered(a as u32, b as u32, op),
        (9, RuntimeValue::I64(a), RuntimeValue::I64(b)) => ordered(a as u64, b as u64, op),
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

fn convert_with_signedness(
    value: RuntimeValue,
    destination: u8,
    form: u8,
) -> Result<RuntimeValue, InstructionFailure> {
    if form & 1 != 0 {
        let unsigned = match value {
            RuntimeValue::I32(v) => u64::from(v as u32),
            RuntimeValue::I64(v) => v as u64,
            _ => return Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
        };
        return match destination {
            1 => Ok(RuntimeValue::I32(unsigned as i32)),
            2 => Ok(RuntimeValue::I64(unsigned as i64)),
            3 => Ok(RuntimeValue::F32(((unsigned as f64) as f32).to_bits())),
            4 => Ok(RuntimeValue::F64((unsigned as f64).to_bits())),
            _ => Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
        };
    }
    if form & 2 != 0 {
        match (value, destination) {
            (RuntimeValue::F32(v), 1) => {
                return Ok(RuntimeValue::I32(f32::from_bits(v) as u32 as i32))
            }
            (RuntimeValue::F32(v), 2) => {
                return Ok(RuntimeValue::I64(f32::from_bits(v) as u64 as i64))
            }
            (RuntimeValue::F64(v), 1) => {
                return Ok(RuntimeValue::I32(f64::from_bits(v) as u32 as i32))
            }
            (RuntimeValue::F64(v), 2) => {
                return Ok(RuntimeValue::I64(f64::from_bits(v) as u64 as i64))
            }
            _ => {}
        }
    }
    convert(value, destination)
}
