use crate::VerifiedArtifact;

use super::{
    error::{AdmissionError, RunError},
    host::{
        AdvanceOutcome, CapabilityBinding, EntryValue, ExecutionProfile, HostArguments,
        HostFailure, HostRequestView, HostResponse, HostValueInput, HostValueSlot, HostValueView,
        ManagedAllocationFailure, QuotaExhaustion, QuotaKind, RequestId, ResolvedCapability,
        ResolvedOperation, ResumeError,
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
}

#[derive(Clone, Copy, Debug)]
struct PendingRequest {
    id: RequestId,
    capability: u32,
    operation: u32,
}

#[derive(Clone, Copy, Debug)]
struct PreparingRequest {
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
        let CapabilityResolution {
            capabilities,
            mask: capability_mask,
        } = resolve_capabilities(&artifact, bindings)?;
        let maximum_host_arguments = checked_usize(profile.maximum_host_arguments)?;
        let outbound_capacity = checked_usize(profile.maximum_outbound_utf16_code_units)?;
        let inbound_capacity = checked_usize(profile.maximum_inbound_utf16_code_units)?;
        let maximum_accepted_responses = profile.maximum_accepted_responses;
        let maximum_slice_budget = profile.maximum_slice_budget;
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
        })
    }

    pub fn start(&mut self, arguments: &[EntryValue]) -> Result<(), RunError> {
        if arguments.len() > self.entry_arguments.len() {
            return Err(RunError::EntryArity {
                expected: u16::try_from(self.entry_arguments.len()).unwrap_or(u16::MAX),
                supplied: u16::try_from(arguments.len()).unwrap_or(u16::MAX),
            });
        }
        for (destination, source) in self.entry_arguments.iter_mut().zip(arguments) {
            *destination = EntryArgument::unowned(match *source {
                EntryValue::I32(value) => RuntimeValue::I32(value),
                EntryValue::I64(value) => RuntimeValue::I64(value),
                EntryValue::F32(value) => RuntimeValue::F32(value),
                EntryValue::F64(value) => RuntimeValue::F64(value),
                EntryValue::Bool(value) => RuntimeValue::Bool(value),
                EntryValue::Char(value) => RuntimeValue::Char(value),
            });
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
        if self.terminal.is_some() {
            return Err(ResumeError::NoPendingRequest);
        }
        let pending = self.pending_request.ok_or(ResumeError::NoPendingRequest)?;
        if request_id != pending.id {
            return Err(ResumeError::WrongRequestId);
        }
        if let HostResponse::Failure(failure) = response {
            self.pending_request = None;
            self.terminal = Some(SessionTerminal::HostFailed(failure));
            return Ok(());
        }
        let HostResponse::Success(input) = response else {
            unreachable!();
        };
        let expected = self
            .capabilities
            .get(pending.capability as usize)
            .and_then(Option::as_ref)
            .and_then(|capability| capability.operations.get(pending.operation as usize))
            .map(|operation| operation.result)
            .ok_or(ResumeError::NoPendingRequest)?;
        if input.value_type() != expected {
            return Err(ResumeError::WrongResponseType);
        }
        let value = match input {
            HostValueInput::Unit => None,
            HostValueInput::I32(value) => Some(RuntimeValue::I32(value)),
            HostValueInput::I64(value) => Some(RuntimeValue::I64(value)),
            HostValueInput::F32(value) => Some(RuntimeValue::F32(value)),
            HostValueInput::F64(value) => Some(RuntimeValue::F64(value)),
            HostValueInput::Bool(value) => Some(RuntimeValue::Bool(value)),
            HostValueInput::Char(value) => Some(RuntimeValue::Char(value)),
            HostValueInput::String(units) => {
                if units.len() > self.inbound_utf16.len() {
                    return Err(ResumeError::ResponseTooLarge);
                }
                if let Err(fault) = self
                    .machine
                    .begin_capability_string_response(units.is_empty())
                {
                    self.pending_request = None;
                    self.terminal = Some(SessionTerminal::Faulted(fault));
                    return Ok(());
                }
                self.inbound_utf16[..units.len()].copy_from_slice(units);
                self.inbound_length = units.len();
                self.resuming_host_string = self.machine.capability_string_response_pending();
                self.pending_request = None;
                return Ok(());
            }
        };
        self.machine
            .complete_capability(value)
            .map_err(|_| ResumeError::WrongResponseType)?;
        self.pending_request = None;
        Ok(())
    }

    fn begin_request(&mut self) -> Result<AdvanceOutcome<'_>, RunError> {
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
                capability,
                operation,
                argument: 0,
                string_offset: 0,
            });
            return Ok(AdvanceOutcome::SliceExhausted);
        }
        self.publish_prepared_request(capability, operation)
    }

    fn publish_prepared_request(
        &mut self,
        capability: u32,
        operation: u32,
    ) -> Result<AdvanceOutcome<'_>, RunError> {
        let Some(next) = self.next_request_id.checked_add(1) else {
            return self.establish_fault(super::error::VmFault::AccountingOverflow);
        };
        let id = RequestId::new(self.next_request_id).ok_or(RunError::NotRunnable)?;
        self.next_request_id = next;
        self.pending_request = Some(PendingRequest {
            id,
            capability,
            operation,
        });
        self.request_outcome().map_err(|_| RunError::NotRunnable)
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
        self.publish_prepared_request(state.capability, state.operation)
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
    pub(super) fn test_set_next_request_id(&mut self, value: u64) {
        self.next_request_id = value;
    }
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
