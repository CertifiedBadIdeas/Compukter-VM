use crate::{EntryArguments, VerifiedArtifact};

use super::{
    error::{AdmissionError, RunError},
    host::{
        AccountingSnapshot, AdvanceOutcome, CapabilityBinding, EntryArgumentLimits, EntryValue,
        ExecutionProfile, HostArguments, HostFailure, HostFailureKind, HostRequestView,
        HostResponse, HostValueInput, HostValueSlot, HostValueView, ManagedAllocationFailure,
        QuotaExhaustion, QuotaKind, RequestId, ResolvedCapability, ResolvedOperation, ResumeError,
        TaskId,
    },
    image::{AdmittedReference, ExecutionImage, ExecutionProfile as ImageProfile},
    machine::Machine,
    value::{EntryArgument, RuntimeValue},
};

pub struct Session {
    machine: Machine,
    capabilities: Box<[Option<ResolvedCapability>]>,
    entry_arguments: Box<[EntryArgument]>,
    outbound_utf16: Box<[u16]>,
    inbound_utf16: Box<[u16]>,
    maximum_host_requests: u64,
    maximum_accepted_responses: u64,
    argument_slots: Box<[HostValueSlot]>,
    argument_count: usize,
    pending_request: Option<PendingRequest>,
    terminal: Option<SessionTerminal>,
    next_request_id: u64,
    preparing_request: Option<PreparingRequest>,
    maximum_slice_budget: u32,
    inbound_length: usize,
    resuming_host_string: bool,
    published_requests: u64,
    accepted_responses: u64,
    entry_argument_limits: EntryArgumentLimits,
    entry_contract: EntryArguments,
}

#[derive(Clone, Copy, Debug)]
struct PendingRequest {
    id: RequestId,
    task: TaskId,
    capability: u32,
    operation: u32,
}

#[derive(Clone, Copy, Debug)]
struct PreparingRequest {
    id: RequestId,
    task: TaskId,
    capability: u32,
    operation: u32,
    argument: usize,
    string_offset: u32,
}

#[derive(Clone, Copy, Debug)]
enum SessionTerminal {
    HostFailed(HostFailure),
    Faulted(super::error::VmFault),
    QuotaExhausted(QuotaExhaustion),
}

struct CapabilityResolution {
    capabilities: Box<[Option<ResolvedCapability>]>,
    mask: u32,
}

impl core::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("capability_count", &self.capabilities.len())
            .field("entry_capacity", &self.entry_arguments.len())
            .field("outbound_utf16_capacity", &self.outbound_utf16.len())
            .field("inbound_utf16_capacity", &self.inbound_utf16.len())
            .field("maximum_host_requests", &self.maximum_host_requests)
            .field(
                "maximum_accepted_responses",
                &self.maximum_accepted_responses,
            )
            .field("pending_request", &self.pending_request)
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub fn admit(
        artifact: VerifiedArtifact,
        profile: ExecutionProfile,
        bindings: &[CapabilityBinding<'_>],
    ) -> Result<Self, AdmissionError> {
        let entry_contract = artifact.entry().arguments;
        let CapabilityResolution {
            capabilities,
            mask: capability_mask,
        } = resolve_capabilities(&artifact, bindings)?;
        let maximum_host_arguments = checked_usize(profile.maximum_host_arguments)?;
        let outbound_capacity = checked_usize(profile.maximum_outbound_utf16_code_units)?;
        let inbound_capacity = checked_usize(profile.maximum_inbound_utf16_code_units)?;
        let maximum_host_requests = u64::from(profile.maximum_host_requests);
        let maximum_accepted_responses = profile.maximum_accepted_responses;
        let maximum_slice_budget = profile.maximum_slice_budget;
        let entry_argument_limits = profile.entry_argument_limits;
        let image_profile = ImageProfile {
            heap_bytes: profile.heap_bytes,
            frame_storage_bytes: profile.frame_storage_bytes,
            maximum_call_depth: profile.maximum_call_depth,
            maximum_coroutines: profile.maximum_coroutines,
            maximum_host_requests: profile.maximum_host_requests,
            maximum_events: profile.maximum_events,
            maximum_slice_budget: profile.maximum_slice_budget,
            compiler_abi: profile.compiler_abi,
            standard_library_abi: profile.standard_library_abi,
            capability_mask,
            host_references: Box::<[AdmittedReference]>::default(),
        };
        let image = ExecutionImage::admit_with_capabilities(
            artifact,
            image_profile,
            capabilities.as_ref(),
        )?;
        let machine = Machine::new(image)?;
        let entry_arguments = initialized_entries(maximum_host_arguments)?;
        let argument_slots = empty_argument_slots(maximum_host_arguments)?;
        let outbound_utf16 = zeroed_u16(outbound_capacity)?;
        let inbound_utf16 = zeroed_u16(inbound_capacity)?;
        Ok(Self {
            machine,
            capabilities,
            entry_arguments,
            outbound_utf16,
            inbound_utf16,
            maximum_host_requests,
            maximum_accepted_responses,
            argument_slots,
            argument_count: 0,
            pending_request: None,
            terminal: None,
            next_request_id: 1,
            preparing_request: None,
            maximum_slice_budget,
            inbound_length: 0,
            resuming_host_string: false,
            published_requests: 0,
            accepted_responses: 0,
            entry_argument_limits,
            entry_contract,
        })
    }

    pub fn start(&mut self, arguments: &[EntryValue<'_>]) -> Result<(), RunError> {
        let expected = match self.entry_contract {
            EntryArguments::None => 0,
            EntryArguments::StringArray => 1,
        };
        if arguments.len() != expected {
            return Err(RunError::EntryArity {
                expected: expected as u16,
                supplied: u16::try_from(arguments.len()).unwrap_or(u16::MAX),
            });
        }
        if matches!(self.entry_contract, EntryArguments::StringArray)
            && !matches!(arguments, [EntryValue::StringArray(_)])
        {
            return Err(RunError::EntryType { parameter: 0 });
        }
        if arguments.len() > self.entry_arguments.len() {
            return Err(RunError::EntryArity {
                expected: u16::try_from(self.entry_arguments.len()).unwrap_or(u16::MAX),
                supplied: u16::try_from(arguments.len()).unwrap_or(u16::MAX),
            });
        }
        for (index, source) in arguments.iter().enumerate() {
            self.entry_arguments[index] = match *source {
                EntryValue::I32(value) => EntryArgument::unowned(RuntimeValue::I32(value)),
                EntryValue::I64(value) => EntryArgument::unowned(RuntimeValue::I64(value)),
                EntryValue::F32(value) => EntryArgument::unowned(RuntimeValue::F32(value)),
                EntryValue::F64(value) => EntryArgument::unowned(RuntimeValue::F64(value)),
                EntryValue::Bool(value) => EntryArgument::unowned(RuntimeValue::Bool(value)),
                EntryValue::Char(value) => EntryArgument::unowned(RuntimeValue::Char(value)),
                EntryValue::StringArray(values) => self
                    .machine
                    .materialize_entry_string_array(values, self.entry_argument_limits)?,
            };
        }
        self.machine.start(&self.entry_arguments[..arguments.len()])
    }

    pub fn advance(
        &mut self,
        guest_budget: u32,
        maintenance_budget: u32,
    ) -> Result<AdvanceOutcome<'_>, RunError> {
        if let Some(terminal) = self.terminal {
            return Ok(match terminal {
                SessionTerminal::HostFailed(failure) => AdvanceOutcome::HostFailed(failure),
                SessionTerminal::Faulted(fault) => AdvanceOutcome::Faulted(fault),
                SessionTerminal::QuotaExhausted(exhaustion) => {
                    AdvanceOutcome::QuotaExhausted(exhaustion)
                }
            });
        }
        if self.pending_request.is_some() {
            return self.request_outcome().map_err(|_| RunError::NotRunnable);
        }
        if self.preparing_request.is_some() {
            return self.advance_request_preparation(guest_budget, maintenance_budget);
        }
        if self.resuming_host_string {
            let outcome = self.machine.run_capability_string_slice(
                &self.inbound_utf16[..self.inbound_length],
                guest_budget,
                maintenance_budget,
            )?;
            self.resuming_host_string = self.machine.capability_string_response_pending();
            return self.map_machine_outcome(outcome);
        }
        let outcome = self.machine.run_slice(guest_budget, maintenance_budget)?;
        self.map_machine_outcome(outcome)
    }

    fn map_machine_outcome(
        &mut self,
        outcome: super::error::Outcome,
    ) -> Result<AdvanceOutcome<'_>, RunError> {
        match outcome {
            super::error::Outcome::SliceExhausted => Ok(AdvanceOutcome::SliceExhausted),
            super::error::Outcome::HostRequest => self.begin_request(),
            super::error::Outcome::AllocationExhausted(exhaustion) => Ok(
                AdvanceOutcome::AllocationExhausted(ManagedAllocationFailure {
                    diagnostic: exhaustion.diagnostic,
                    collection_attempted: exhaustion.collection_attempted,
                }),
            ),
            super::error::Outcome::Halted(value) => match value {
                None => Ok(AdvanceOutcome::Halted(None)),
                Some(value) => match runtime_view(value) {
                    Some(value) => Ok(AdvanceOutcome::Halted(Some(value))),
                    None => self.establish_fault(super::error::VmFault::InvalidValueType),
                },
            },
            super::error::Outcome::Crashed(trap) => Ok(AdvanceOutcome::Crashed(trap)),
            super::error::Outcome::Faulted(fault) => Ok(AdvanceOutcome::Faulted(fault)),
        }
    }

    pub fn resume(
        &mut self,
        request_id: RequestId,
        response: HostResponse<'_>,
    ) -> Result<(), ResumeError> {
        self.resume_for(TaskId::ROOT, request_id, response)
    }

    pub fn resume_for(
        &mut self,
        task: TaskId,
        request_id: RequestId,
        response: HostResponse<'_>,
    ) -> Result<(), ResumeError> {
        if self.terminal.is_some() {
            return Err(ResumeError::NoPendingRequest);
        }
        let pending = self.pending_request.ok_or(ResumeError::NoPendingRequest)?;
        if task != pending.task {
            return Err(ResumeError::WrongTask);
        }
        if request_id != pending.id {
            return Err(ResumeError::WrongRequestId);
        }
        let expected = self
            .capabilities
            .get(pending.capability as usize)
            .and_then(Option::as_ref)
            .and_then(|capability| capability.operations.get(pending.operation as usize))
            .map(|operation| operation.result)
            .ok_or(ResumeError::NoPendingRequest)?;
        if let HostResponse::Success(input) = response {
            if input.value_type() != expected {
                return Err(ResumeError::WrongResponseType);
            }
            if let HostValueInput::String(units) = input {
                if units.len() > self.inbound_utf16.len() {
                    return Err(ResumeError::ResponseTooLarge);
                }
            }
        }
        if self.accepted_responses >= self.maximum_accepted_responses {
            self.terminal = Some(SessionTerminal::QuotaExhausted(QuotaExhaustion {
                kind: QuotaKind::AcceptedResponses,
                limit: self.maximum_accepted_responses,
                consumed: self.accepted_responses,
            }));
            return Ok(());
        }
        if let HostResponse::Failure(failure) = response {
            self.accept_response(request_id, response);
            self.pending_request = None;
            self.terminal = Some(SessionTerminal::HostFailed(failure));
            return Ok(());
        }
        let HostResponse::Success(input) = response else {
            unreachable!();
        };
        let value = match input {
            HostValueInput::Unit => None,
            HostValueInput::I32(value) => Some(RuntimeValue::I32(value)),
            HostValueInput::I64(value) => Some(RuntimeValue::I64(value)),
            HostValueInput::F32(value) => Some(RuntimeValue::F32(value)),
            HostValueInput::F64(value) => Some(RuntimeValue::F64(value)),
            HostValueInput::Bool(value) => Some(RuntimeValue::Bool(value)),
            HostValueInput::Char(value) => Some(RuntimeValue::Char(value)),
            HostValueInput::String(units) => {
                self.inbound_utf16[..units.len()].copy_from_slice(units);
                if let Err(fault) = self
                    .machine
                    .begin_capability_string_response(units.is_empty())
                {
                    self.accept_response(request_id, response);
                    self.pending_request = None;
                    self.terminal = Some(SessionTerminal::Faulted(fault));
                    return Ok(());
                }
                self.inbound_length = units.len();
                self.resuming_host_string = self.machine.capability_string_response_pending();
                self.accept_response(request_id, response);
                self.pending_request = None;
                return Ok(());
            }
        };
        self.machine
            .complete_capability(value)
            .map_err(|_| ResumeError::WrongResponseType)?;
        self.accept_response(request_id, response);
        self.pending_request = None;
        Ok(())
    }

    pub(crate) fn resume_internal(
        &mut self,
        request_id: RequestId,
        response: HostResponse<'_>,
    ) -> Result<(), ResumeError> {
        self.resume(request_id, response)?;
        self.published_requests = self
            .published_requests
            .checked_sub(1)
            .expect("an internal response always follows a published request");
        self.accepted_responses = self
            .accepted_responses
            .checked_sub(1)
            .expect("an internal response is always accepted");
        Ok(())
    }

    pub fn accounting(&self) -> AccountingSnapshot {
        AccountingSnapshot {
            fixed_guest_units: self.machine.consumed_fixed_cost(),
            dynamic_guest_units: self.machine.consumed_dynamic_cost(),
            maintenance_units: self.machine.consumed_maintenance_cost(),
            entered_blocks: self.machine.entered_blocks(),
            executed_instructions: self.machine.executed_instructions(),
            published_requests: self.published_requests,
            accepted_responses: self.accepted_responses,
            trace_digest: self.machine.trace_digest(),
        }
    }

    fn begin_request(&mut self) -> Result<AdvanceOutcome<'_>, RunError> {
        if self.published_requests >= self.maximum_host_requests {
            return self.establish_quota(QuotaExhaustion {
                kind: QuotaKind::HostRequests,
                limit: self.maximum_host_requests,
                consumed: self.published_requests,
            });
        }
        let Some(next_request_id) = self.next_request_id.checked_add(1) else {
            return self.establish_fault(super::error::VmFault::AccountingOverflow);
        };
        let id = RequestId::new(self.next_request_id).ok_or(RunError::NotRunnable)?;
        let prepared = (|| {
            let suspension = self.machine.capability_suspension()?;
            if suspension.arguments.len() > self.argument_slots.len() {
                return Err(super::error::VmFault::InvalidStoragePlan);
            }
            let schema = self
                .capabilities
                .get(suspension.capability as usize)
                .and_then(Option::as_ref)
                .and_then(|capability| capability.operations.get(suspension.operation as usize))
                .ok_or(super::error::VmFault::InvalidResolvedId)?;
            let mut outbound_used = 0_u32;
            let mut contains_string = false;
            for (index, register) in suspension.arguments.iter().copied().enumerate() {
                if schema.arguments.get(index) == Some(&super::host::HostValueType::String) {
                    contains_string = true;
                    let length = self.machine.capability_string_length(register)?;
                    self.argument_slots[index] = HostValueSlot::String {
                        start: outbound_used,
                        length,
                    };
                    outbound_used = outbound_used
                        .checked_add(length)
                        .ok_or(super::error::VmFault::AccountingOverflow)?;
                } else {
                    let value = self.machine.capability_argument(register)?;
                    let slot =
                        runtime_slot(value).ok_or(super::error::VmFault::UnsupportedInstruction)?;
                    self.argument_slots[index] = slot;
                }
            }
            Ok((
                suspension.capability,
                suspension.operation,
                suspension.arguments.len(),
                outbound_used,
                contains_string,
            ))
        })();
        let (capability, operation, argument_count, outbound_used, contains_string) = match prepared
        {
            Ok(prepared) => prepared,
            Err(fault) => return self.establish_fault(fault),
        };
        self.argument_count = argument_count;
        let outbound_limit = u64::try_from(self.outbound_utf16.len()).unwrap_or(u64::MAX);
        if u64::from(outbound_used) > outbound_limit {
            return self.establish_quota(QuotaExhaustion {
                kind: QuotaKind::HostRequestCodeUnits,
                limit: outbound_limit,
                consumed: u64::from(outbound_used),
            });
        }
        if contains_string && outbound_used != 0 {
            self.preparing_request = Some(PreparingRequest {
                id,
                task: TaskId::ROOT,
                capability,
                operation,
                argument: 0,
                string_offset: 0,
            });
            return Ok(AdvanceOutcome::SliceExhausted);
        }
        self.publish_prepared_request(id, TaskId::ROOT, next_request_id, capability, operation)
    }

    fn publish_prepared_request(
        &mut self,
        id: RequestId,
        task: TaskId,
        next_request_id: u64,
        capability: u32,
        operation: u32,
    ) -> Result<AdvanceOutcome<'_>, RunError> {
        self.next_request_id = next_request_id;
        self.pending_request = Some(PendingRequest {
            id,
            task,
            capability,
            operation,
        });
        self.published_requests += 1;
        trace_request(
            &mut self.machine,
            id,
            capability,
            operation,
            &self.argument_slots[..self.argument_count],
            &self.outbound_utf16,
        );
        self.request_outcome().map_err(|_| RunError::NotRunnable)
    }

    fn accept_response(&mut self, request_id: RequestId, response: HostResponse<'_>) {
        self.accepted_responses += 1;
        trace_response(&mut self.machine, request_id, response);
    }

    fn advance_request_preparation(
        &mut self,
        guest_budget: u32,
        maintenance_budget: u32,
    ) -> Result<AdvanceOutcome<'_>, RunError> {
        if guest_budget == 0 || guest_budget > self.maximum_slice_budget {
            return Err(RunError::InvalidSliceBudget {
                minimum: 1,
                maximum: self.maximum_slice_budget,
                supplied: guest_budget,
            });
        }
        if maintenance_budget > self.maximum_slice_budget {
            return Err(RunError::InvalidSliceBudget {
                minimum: 0,
                maximum: self.maximum_slice_budget,
                supplied: maintenance_budget,
            });
        }
        let mut state = self.preparing_request.ok_or(RunError::NotRunnable)?;
        let mut remaining = guest_budget;
        while state.argument < self.argument_count {
            let HostValueSlot::String { start, length } = self.argument_slots[state.argument]
            else {
                state.argument += 1;
                state.string_offset = 0;
                continue;
            };
            while state.string_offset < length && remaining != 0 {
                let end = state.string_offset.saturating_add(8).min(length);
                let register = self
                    .machine
                    .capability_suspension()
                    .map_err(|_| RunError::NotRunnable)?
                    .arguments
                    .get(state.argument)
                    .copied()
                    .ok_or(RunError::NotRunnable)?;
                while state.string_offset < end {
                    let unit = self
                        .machine
                        .capability_string_code_unit(register, state.string_offset)
                        .map_err(|_| RunError::NotRunnable)?;
                    let destination = start
                        .checked_add(state.string_offset)
                        .and_then(|index| usize::try_from(index).ok())
                        .ok_or(RunError::NotRunnable)?;
                    *self
                        .outbound_utf16
                        .get_mut(destination)
                        .ok_or(RunError::NotRunnable)? = unit;
                    state.string_offset += 1;
                }
                if let Err(fault) = self.machine.charge_capability_dynamic(1) {
                    return self.establish_fault(fault);
                }
                remaining -= 1;
            }
            if state.string_offset != length {
                self.preparing_request = Some(state);
                return Ok(AdvanceOutcome::SliceExhausted);
            }
            state.argument += 1;
            state.string_offset = 0;
        }
        self.preparing_request = None;
        let next_request_id = state.id.get().checked_add(1).ok_or(RunError::NotRunnable)?;
        self.publish_prepared_request(
            state.id,
            state.task,
            next_request_id,
            state.capability,
            state.operation,
        )
    }

    fn request_outcome(&self) -> Result<AdvanceOutcome<'_>, super::error::VmFault> {
        let pending = self
            .pending_request
            .ok_or(super::error::VmFault::CorruptLifecycle)?;
        let capability = self
            .capabilities
            .get(pending.capability as usize)
            .and_then(Option::as_ref)
            .ok_or(super::error::VmFault::InvalidResolvedId)?;
        Ok(AdvanceOutcome::HostRequest(HostRequestView {
            id: pending.id,
            task: pending.task,
            capability,
            operation: pending.operation,
            arguments: HostArguments {
                slots: &self.argument_slots[..self.argument_count],
                utf16: &self.outbound_utf16,
            },
        }))
    }

    fn establish_fault(
        &mut self,
        fault: super::error::VmFault,
    ) -> Result<AdvanceOutcome<'_>, RunError> {
        self.terminal = Some(SessionTerminal::Faulted(fault));
        Ok(AdvanceOutcome::Faulted(fault))
    }

    fn establish_quota(
        &mut self,
        exhaustion: QuotaExhaustion,
    ) -> Result<AdvanceOutcome<'_>, RunError> {
        self.terminal = Some(SessionTerminal::QuotaExhausted(exhaustion));
        Ok(AdvanceOutcome::QuotaExhausted(exhaustion))
    }

    #[cfg(test)]
    pub(crate) fn test_set_next_request_id(&mut self, value: u64) {
        self.next_request_id = value;
    }

    #[cfg(test)]
    pub(super) fn test_reserved_mutable_bytes(&self) -> usize {
        self.machine.test_reserved_bytes()
            + self.entry_arguments.len() * core::mem::size_of::<EntryArgument>()
            + self.outbound_utf16.len() * core::mem::size_of::<u16>()
            + self.inbound_utf16.len() * core::mem::size_of::<u16>()
            + self.argument_slots.len() * core::mem::size_of::<HostValueSlot>()
    }
}

fn trace_request(
    machine: &mut Machine,
    id: RequestId,
    capability: u32,
    operation: u32,
    arguments: &[HostValueSlot],
    utf16: &[u16],
) {
    trace_field(machine, &[2]);
    trace_field(machine, &id.get().to_le_bytes());
    trace_field(machine, &capability.to_le_bytes());
    trace_field(machine, &operation.to_le_bytes());
    trace_field(
        machine,
        &u32::try_from(arguments.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    for argument in arguments {
        trace_slot(machine, *argument, utf16);
    }
}

fn trace_response(machine: &mut Machine, id: RequestId, response: HostResponse<'_>) {
    trace_field(machine, &[3]);
    trace_field(machine, &id.get().to_le_bytes());
    match response {
        HostResponse::Success(value) => {
            trace_field(machine, &[0]);
            trace_input(machine, value);
        }
        HostResponse::Failure(failure) => {
            trace_field(machine, &[1]);
            let kind = match failure.kind() {
                HostFailureKind::EndOfFile => 0,
                HostFailureKind::Unavailable => 1,
                HostFailureKind::InputOutput => 2,
                HostFailureKind::Cancelled => 3,
                HostFailureKind::Other => 4,
            };
            trace_field(machine, &[kind]);
            trace_field(machine, &failure.code().to_le_bytes());
        }
    }
}

fn trace_input(machine: &mut Machine, value: HostValueInput<'_>) {
    match value {
        HostValueInput::Unit => trace_field(machine, &[0]),
        HostValueInput::I32(value) => trace_scalar(machine, 1, &value.to_le_bytes()),
        HostValueInput::I64(value) => trace_scalar(machine, 2, &value.to_le_bytes()),
        HostValueInput::F32(value) => trace_scalar(machine, 3, &value.to_le_bytes()),
        HostValueInput::F64(value) => trace_scalar(machine, 4, &value.to_le_bytes()),
        HostValueInput::Bool(value) => trace_scalar(machine, 5, &[u8::from(value)]),
        HostValueInput::Char(value) => trace_scalar(machine, 6, &value.to_le_bytes()),
        HostValueInput::String(units) => trace_utf16(machine, units),
    }
}

fn trace_slot(machine: &mut Machine, value: HostValueSlot, utf16: &[u16]) {
    match value {
        HostValueSlot::Empty => trace_field(machine, &[0]),
        HostValueSlot::I32(value) => trace_scalar(machine, 1, &value.to_le_bytes()),
        HostValueSlot::I64(value) => trace_scalar(machine, 2, &value.to_le_bytes()),
        HostValueSlot::F32(value) => trace_scalar(machine, 3, &value.to_le_bytes()),
        HostValueSlot::F64(value) => trace_scalar(machine, 4, &value.to_le_bytes()),
        HostValueSlot::Bool(value) => trace_scalar(machine, 5, &[u8::from(value)]),
        HostValueSlot::Char(value) => trace_scalar(machine, 6, &value.to_le_bytes()),
        HostValueSlot::String { start, length } => {
            let start = usize::try_from(start).unwrap_or(usize::MAX);
            let length = usize::try_from(length).unwrap_or(usize::MAX);
            let units = start
                .checked_add(length)
                .and_then(|end| utf16.get(start..end))
                .unwrap_or_default();
            trace_utf16(machine, units);
        }
    }
}

fn trace_scalar(machine: &mut Machine, tag: u8, payload: &[u8]) {
    trace_field(machine, &[tag]);
    trace_field(machine, payload);
}

fn trace_utf16(machine: &mut Machine, units: &[u16]) {
    trace_field(machine, &[7]);
    trace_field(
        machine,
        &u32::try_from(units.len()).unwrap_or(u32::MAX).to_le_bytes(),
    );
    for unit in units {
        trace_field(machine, &unit.to_le_bytes());
    }
}

fn trace_field(machine: &mut Machine, bytes: &[u8]) {
    machine.trace_host_field(bytes);
}

fn runtime_slot(value: RuntimeValue) -> Option<HostValueSlot> {
    match value {
        RuntimeValue::I32(value) => Some(HostValueSlot::I32(value)),
        RuntimeValue::I64(value) => Some(HostValueSlot::I64(value)),
        RuntimeValue::F32(value) => Some(HostValueSlot::F32(value)),
        RuntimeValue::F64(value) => Some(HostValueSlot::F64(value)),
        RuntimeValue::Bool(value) => Some(HostValueSlot::Bool(value)),
        RuntimeValue::Char(value) => Some(HostValueSlot::Char(value)),
        RuntimeValue::Null | RuntimeValue::Reference(_) => None,
    }
}

fn runtime_view(value: RuntimeValue) -> Option<HostValueView<'static>> {
    match runtime_slot(value)? {
        HostValueSlot::I32(value) => Some(HostValueView::I32(value)),
        HostValueSlot::I64(value) => Some(HostValueView::I64(value)),
        HostValueSlot::F32(value) => Some(HostValueView::F32(value)),
        HostValueSlot::F64(value) => Some(HostValueView::F64(value)),
        HostValueSlot::Bool(value) => Some(HostValueView::Bool(value)),
        HostValueSlot::Char(value) => Some(HostValueView::Char(value)),
        HostValueSlot::Empty | HostValueSlot::String { .. } => None,
    }
}

fn resolve_capabilities(
    artifact: &VerifiedArtifact,
    bindings: &[CapabilityBinding<'_>],
) -> Result<CapabilityResolution, AdmissionError> {
    for (index, binding) in bindings.iter().enumerate() {
        if bindings[..index].iter().any(|prior| {
            prior.namespace() == binding.namespace()
                && prior.name() == binding.name()
                && prior.abi_major() == binding.abi_major()
        }) {
            return Err(AdmissionError::DuplicateCapabilityBinding);
        }
    }

    let decoded = artifact.decoded();
    let entry_module = decoded
        .modules
        .get(decoded.header.entry_module as usize)
        .ok_or(AdmissionError::InvalidEntry)?;
    let mut resolved = Vec::new();
    resolved
        .try_reserve_exact(decoded.capabilities.len())
        .map_err(|_| AdmissionError::AllocationFailed)?;
    let mut capability_mask = 0_u32;
    for (index, descriptor) in decoded.capabilities.iter().enumerate() {
        let namespace = entry_module
            .strings
            .get(descriptor.namespace as usize)
            .map(|range| range.slice(&decoded.bytes))
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .ok_or(AdmissionError::InvalidEntry)?;
        let name = entry_module
            .strings
            .get(descriptor.name as usize)
            .map(|range| range.slice(&decoded.bytes))
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .ok_or(AdmissionError::InvalidEntry)?;
        let binding = bindings.iter().find(|binding| {
            binding.namespace() == namespace
                && binding.name() == name
                && binding.abi_major() == descriptor.abi_major
                && binding.abi_minor() >= descriptor.minimum_abi_minor
        });
        let Some(binding) = binding else {
            if descriptor.flags == 1 {
                return Err(AdmissionError::MissingCapability {
                    index: u8::try_from(index).unwrap_or(u8::MAX),
                });
            }
            resolved.push(None);
            continue;
        };
        if binding.operations().len() < descriptor.operation_count as usize {
            return Err(AdmissionError::CapabilityOperationCount {
                capability: u32::try_from(index)
                    .map_err(|_| AdmissionError::StoragePlanOverflow)?,
                required: descriptor.operation_count,
                available: u32::try_from(binding.operations().len())
                    .map_err(|_| AdmissionError::StoragePlanOverflow)?,
            });
        }
        let mut operations = Vec::new();
        operations
            .try_reserve_exact(binding.operations().len())
            .map_err(|_| AdmissionError::AllocationFailed)?;
        for schema in binding.operations() {
            let mut arguments = Vec::new();
            arguments
                .try_reserve_exact(schema.arguments.len())
                .map_err(|_| AdmissionError::AllocationFailed)?;
            arguments.extend_from_slice(schema.arguments);
            operations.push(ResolvedOperation {
                arguments: arguments.into_boxed_slice(),
                result: schema.result,
                asynchronous: schema.asynchronous,
            });
        }
        if index < u32::BITS as usize {
            capability_mask |= 1_u32 << index;
        }
        resolved.push(Some(ResolvedCapability {
            namespace: boxed_str(namespace)?,
            name: boxed_str(name)?,
            abi_major: binding.abi_major(),
            abi_minor: binding.abi_minor(),
            operations: operations.into_boxed_slice(),
        }));
    }
    Ok(CapabilityResolution {
        capabilities: resolved.into_boxed_slice(),
        mask: capability_mask,
    })
}

fn boxed_str(value: &str) -> Result<Box<str>, AdmissionError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(value.len())
        .map_err(|_| AdmissionError::AllocationFailed)?;
    bytes.extend_from_slice(value.as_bytes());
    String::from_utf8(bytes)
        .map(String::into_boxed_str)
        .map_err(|_| AdmissionError::InvalidEntry)
}

fn checked_usize(value: u32) -> Result<usize, AdmissionError> {
    usize::try_from(value).map_err(|_| AdmissionError::StoragePlanOverflow)
}

fn initialized_entries(length: usize) -> Result<Box<[EntryArgument]>, AdmissionError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| AdmissionError::AllocationFailed)?;
    values.resize(length, EntryArgument::unowned(RuntimeValue::I32(0)));
    Ok(values.into_boxed_slice())
}

fn empty_argument_slots(length: usize) -> Result<Box<[HostValueSlot]>, AdmissionError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| AdmissionError::AllocationFailed)?;
    values.resize(length, HostValueSlot::Empty);
    Ok(values.into_boxed_slice())
}

fn zeroed_u16(length: usize) -> Result<Box<[u16]>, AdmissionError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| AdmissionError::AllocationFailed)?;
    values.resize(length, 0);
    Ok(values.into_boxed_slice())
}
