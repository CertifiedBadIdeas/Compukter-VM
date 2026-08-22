use crate::VerifiedArtifact;

use super::{
    error::{AdmissionError, RunError},
    host::{
        CapabilityBinding, EntryValue, ExecutionProfile, ResolvedCapability, ResolvedOperation,
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
            .finish_non_exhaustive()
    }
}

impl Session {
    pub fn admit(
        artifact: VerifiedArtifact,
        profile: ExecutionProfile,
        bindings: &[CapabilityBinding<'_>],
    ) -> Result<Self, AdmissionError> {
        let (capabilities, capability_mask) = resolve_capabilities(&artifact, bindings)?;
        let maximum_host_arguments = checked_usize(profile.maximum_host_arguments)?;
        let outbound_capacity = checked_usize(profile.maximum_outbound_utf16_code_units)?;
        let inbound_capacity = checked_usize(profile.maximum_inbound_utf16_code_units)?;
        let maximum_accepted_responses = profile.maximum_accepted_responses;
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
        let outbound_utf16 = zeroed_u16(outbound_capacity)?;
        let inbound_utf16 = zeroed_u16(inbound_capacity)?;
        Ok(Self {
            machine,
            capabilities,
            entry_arguments,
            outbound_utf16,
            inbound_utf16,
            maximum_accepted_responses,
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
}

fn resolve_capabilities(
    artifact: &VerifiedArtifact,
    bindings: &[CapabilityBinding<'_>],
) -> Result<(Box<[Option<ResolvedCapability>]>, u32), AdmissionError> {
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
    Ok((resolved.into_boxed_slice(), capability_mask))
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

fn zeroed_u16(length: usize) -> Result<Box<[u16]>, AdmissionError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| AdmissionError::AllocationFailed)?;
    values.resize(length, 0);
    Ok(values.into_boxed_slice())
}
