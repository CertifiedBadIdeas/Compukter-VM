use std::sync::Arc;

use crate::artifact::{
    Constant, DecodedArtifact, Instruction, NominalType, SwitchCase, TypeId, ValueType,
};
use crate::VerifiedArtifact;

use super::{
    error::AdmissionError,
    value::{ReferenceValue, RuntimeValue},
    FunctionKey, TypeKey,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AdmittedReference {
    pub ty: TypeKey,
    pub handle: u32,
    pub generation: u32,
    pub live: bool,
}

#[derive(Clone, Debug)]
pub(super) struct ExecutionProfile {
    pub heap_bytes: u32,
    pub frame_storage_bytes: u64,
    pub maximum_call_depth: u32,
    pub maximum_coroutines: u32,
    pub maximum_host_requests: u32,
    pub maximum_events: u32,
    pub maximum_slice_budget: u32,
    pub compiler_abi: [u8; 32],
    pub standard_library_abi: [u8; 32],
    pub capability_mask: u32,
    pub host_references: Box<[AdmittedReference]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ResolvedValueType {
    pub kind: u8,
    pub nullable: bool,
    pub nominal: Option<TypeKey>,
}

#[derive(Debug)]
pub(super) struct ResolvedFunction {
    pub key: FunctionKey,
    pub register_count: usize,
    pub parameter_count: usize,
    pub registers: Box<[ResolvedValueType]>,
    pub result: ResolvedValueType,
    pub first_block: usize,
    pub block_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ResolvedSwitchCase {
    pub value: i32,
    pub target: usize,
}

#[derive(Debug)]
pub(super) enum ResolvedInstruction {
    Nop,
    Move {
        dst: u16,
        src: u16,
    },
    Const {
        dst: u16,
        constant: usize,
    },
    Null {
        dst: u16,
    },
    Convert {
        dst: u16,
        src: u16,
    },
    Add {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Sub {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Mul {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Div {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Rem {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Neg {
        form: u8,
        dst: u16,
        src: u16,
    },
    BitAnd {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    BitOr {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    BitXor {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    ShiftLeft {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    ShiftRight {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    ShiftUnsigned {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Equal {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    NotEqual {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Less {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    LessEqual {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Greater {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    GreaterEqual {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    RefEqual {
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    RefNotEqual {
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    CallDirect {
        dst: u16,
        target: usize,
        args: Box<[u16]>,
    },
    Jump {
        target: usize,
    },
    Branch {
        condition: u16,
        true_block: usize,
        false_block: usize,
    },
    SwitchI32 {
        key: u16,
        default_block: usize,
        cases: Box<[ResolvedSwitchCase]>,
    },
    Return {
        value: u16,
    },
    Unreachable,
}

#[derive(Debug)]
pub(super) struct ResolvedBlock {
    pub function: usize,
    pub fixed_cost: u32,
    pub instructions: Box<[ResolvedInstruction]>,
}

#[derive(Debug)]
pub(super) struct ResolvedHostReference {
    pub value: ReferenceValue,
    pub live: bool,
    pub assignable_to: Box<[TypeKey]>,
}

#[derive(Clone, Debug)]
pub(super) struct ExecutionImage(Arc<ExecutionImageInner>);

#[derive(Debug)]
struct ExecutionImageInner {
    content_hash: [u8; 32],
    entry: usize,
    functions: Box<[ResolvedFunction]>,
    blocks: Box<[ResolvedBlock]>,
    constants: Box<[RuntimeValue]>,
    host_references: Box<[ResolvedHostReference]>,
    registers_per_frame: usize,
    maximum_call_depth: usize,
    minimum_slice_cost: u32,
    maximum_slice_budget: u32,
}

impl ExecutionImage {
    pub(super) fn admit(
        artifact: VerifiedArtifact,
        profile: ExecutionProfile,
    ) -> Result<Self, AdmissionError> {
        let decoded = artifact.decoded();
        check_profile(decoded, &profile)?;
        let function_offsets =
            offsets(decoded.modules.iter().map(|module| module.functions.len()))?;
        let block_offsets = offsets(decoded.modules.iter().map(|module| module.blocks.len()))?;
        let constant_offsets =
            offsets(decoded.modules.iter().map(|module| module.constants.len()))?;

        let mut constants = reserved(
            *constant_offsets
                .last()
                .ok_or(AdmissionError::InvalidEntry)?,
        )?;
        for module in &decoded.modules {
            for constant in &module.constants {
                constants.push(resolve_constant(constant)?);
            }
        }

        let total_functions = *function_offsets
            .last()
            .ok_or(AdmissionError::InvalidEntry)?;
        let mut functions = reserved(total_functions)?;
        let mut registers_per_frame = 0_usize;
        for (module_id, module) in decoded.modules.iter().enumerate() {
            for (function_id, function) in module.functions.iter().enumerate() {
                if function.flags & (1 << 3) != 0 {
                    return Err(AdmissionError::InvalidEntry);
                }
                let signature_key = resolve_type(decoded, module_id, function.signature)
                    .ok_or(AdmissionError::InvalidEntry)?;
                let NominalType::Function { result, .. } = &decoded.modules
                    [signature_key.module as usize]
                    .types[signature_key.ty as usize]
                else {
                    return Err(AdmissionError::InvalidEntry);
                };
                let mut registers = reserved(function.registers.len())?;
                for value_type in &function.registers {
                    registers.push(resolve_value_type(decoded, module_id, *value_type)?);
                }
                registers_per_frame = registers_per_frame.max(registers.len());
                functions.push(ResolvedFunction {
                    key: FunctionKey {
                        module: module_id as u32,
                        function: function_id as u32,
                    },
                    register_count: function.register_count as usize,
                    parameter_count: function.parameter_count as usize,
                    registers: registers.into_boxed_slice(),
                    result: resolve_value_type(decoded, signature_key.module as usize, *result)?,
                    first_block: block_offsets[module_id]
                        .checked_add(function.first_block.0 as usize)
                        .ok_or(AdmissionError::StoragePlanOverflow)?,
                    block_count: function.block_count as usize,
                });
            }
        }

        let mut blocks = reserved(*block_offsets.last().ok_or(AdmissionError::InvalidEntry)?)?;
        for (module_id, module) in decoded.modules.iter().enumerate() {
            for (block_id, block) in module.blocks.iter().enumerate() {
                let function = function_offsets[module_id]
                    .checked_add(block.owner_function.0 as usize)
                    .ok_or(AdmissionError::StoragePlanOverflow)?;
                let code = module
                    .code
                    .get(block_id)
                    .ok_or(AdmissionError::InvalidEntry)?;
                let mut instructions = reserved(code.instructions.len())?;
                for instruction in code.instructions.iter() {
                    instructions.push(resolve_instruction(
                        decoded,
                        module_id,
                        &function_offsets,
                        &block_offsets,
                        &constant_offsets,
                        instruction,
                    )?);
                }
                blocks.push(ResolvedBlock {
                    function,
                    fixed_cost: code.fixed_cost,
                    instructions: instructions.into_boxed_slice(),
                });
            }
        }

        let mut host_references = reserved(profile.host_references.len())?;
        for (index, reference) in profile.host_references.iter().enumerate() {
            if !type_exists(decoded, reference.ty)
                || profile.host_references[..index].iter().any(|prior| {
                    prior.handle == reference.handle && prior.generation == reference.generation
                })
            {
                return Err(AdmissionError::InvalidEntry);
            }
            let assignable_to = assignable_types(decoded, reference.ty)?;
            host_references.push(ResolvedHostReference {
                value: ReferenceValue {
                    image: artifact.content_hash(),
                    ty: reference.ty,
                    handle: reference.handle,
                    generation: reference.generation,
                },
                live: reference.live,
                assignable_to,
            });
        }

        let entry_key = artifact.entry();
        let entry = function_offsets
            .get(entry_key.module as usize)
            .and_then(|offset| offset.checked_add(entry_key.function as usize))
            .filter(|entry| *entry < functions.len())
            .ok_or(AdmissionError::InvalidEntry)?;
        if decoded.modules[entry_key.module as usize].functions[entry_key.function as usize].flags
            & 1
            != 0
        {
            return Err(AdmissionError::InvalidEntry);
        }

        Ok(Self(Arc::new(ExecutionImageInner {
            content_hash: artifact.content_hash(),
            entry,
            functions: functions.into_boxed_slice(),
            blocks: blocks.into_boxed_slice(),
            constants: constants.into_boxed_slice(),
            host_references: host_references.into_boxed_slice(),
            registers_per_frame,
            maximum_call_depth: decoded.manifest.maximum_call_depth as usize,
            minimum_slice_cost: decoded.manifest.minimum_slice_cost,
            maximum_slice_budget: profile.maximum_slice_budget,
        })))
    }

    pub(super) fn entry(&self) -> FunctionKey {
        self.0.functions[self.0.entry].key
    }

    pub(super) fn functions(&self) -> &[ResolvedFunction] {
        &self.0.functions
    }

    pub(super) fn registers_per_frame(&self) -> usize {
        self.0.registers_per_frame
    }
}

pub(super) fn frame_charge(registers: u64) -> Result<u64, AdmissionError> {
    let bytes = registers
        .checked_mul(16)
        .and_then(|value| value.checked_add(32))
        .ok_or(AdmissionError::StoragePlanOverflow)?;
    bytes
        .checked_add(15)
        .map(|value| value & !15)
        .ok_or(AdmissionError::StoragePlanOverflow)
}

fn check_profile(
    artifact: &DecodedArtifact,
    profile: &ExecutionProfile,
) -> Result<(), AdmissionError> {
    let manifest = artifact.manifest;
    if manifest.compiler_abi != profile.compiler_abi {
        return Err(AdmissionError::CompilerAbiMismatch);
    }
    if manifest.standard_library_abi != profile.standard_library_abi {
        return Err(AdmissionError::StandardLibraryAbiMismatch);
    }
    if manifest.required_capabilities & !profile.capability_mask != 0 {
        return Err(AdmissionError::MissingCapability {
            index: (manifest.required_capabilities & !profile.capability_mask).trailing_zeros()
                as u8,
        });
    }
    check_limit(
        manifest.required_heap_bytes,
        profile.heap_bytes,
        |required, available| AdmissionError::HeapLimit {
            required,
            available,
        },
    )?;
    check_limit(
        manifest.maximum_call_depth,
        profile.maximum_call_depth,
        |required, available| AdmissionError::CallDepthLimit {
            required,
            available,
        },
    )?;
    check_limit(
        manifest.maximum_coroutines,
        profile.maximum_coroutines,
        |required, available| AdmissionError::CoroutineLimit {
            required,
            available,
        },
    )?;
    check_limit(
        manifest.maximum_host_requests,
        profile.maximum_host_requests,
        |required, available| AdmissionError::HostRequestLimit {
            required,
            available,
        },
    )?;
    check_limit(
        manifest.maximum_events,
        profile.maximum_events,
        |required, available| AdmissionError::EventLimit {
            required,
            available,
        },
    )?;
    check_limit(
        manifest.minimum_slice_cost,
        profile.maximum_slice_budget,
        |required, available| AdmissionError::SliceLimit {
            required,
            available,
        },
    )?;
    let largest_register_count = artifact
        .modules
        .iter()
        .flat_map(|module| &module.functions)
        .filter(|function| function.flags & (1 << 3) == 0)
        .map(|function| u64::from(function.register_count))
        .max()
        .ok_or(AdmissionError::InvalidEntry)?;
    let reservation = frame_charge(largest_register_count)?
        .checked_mul(u64::from(manifest.maximum_call_depth))
        .ok_or(AdmissionError::StoragePlanOverflow)?;
    if u64::from(manifest.required_stack_bytes) < reservation {
        return Err(AdmissionError::FrameStorageLimit {
            required: reservation,
            available: u64::from(manifest.required_stack_bytes),
        });
    }
    if profile.frame_storage_bytes < reservation {
        return Err(AdmissionError::FrameStorageLimit {
            required: reservation,
            available: profile.frame_storage_bytes,
        });
    }
    Ok(())
}

fn check_limit<F>(required: u32, available: u32, error: F) -> Result<(), AdmissionError>
where
    F: FnOnce(u32, u32) -> AdmissionError,
{
    if required > available {
        Err(error(required, available))
    } else {
        Ok(())
    }
}

fn offsets(lengths: impl Iterator<Item = usize>) -> Result<Box<[usize]>, AdmissionError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(lengths.size_hint().0 + 1)
        .map_err(|_| AdmissionError::AllocationFailed)?;
    values.push(0_usize);
    for length in lengths {
        let next = values
            .last()
            .and_then(|value| value.checked_add(length))
            .ok_or(AdmissionError::StoragePlanOverflow)?;
        values.push(next);
    }
    Ok(values.into_boxed_slice())
}

fn reserved<T>(capacity: usize) -> Result<Vec<T>, AdmissionError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| AdmissionError::AllocationFailed)?;
    Ok(values)
}

fn resolve_constant(value: &Constant) -> Result<RuntimeValue, AdmissionError> {
    Ok(match value {
        Constant::I32(value) => RuntimeValue::I32(*value),
        Constant::I64(value) => RuntimeValue::I64(*value),
        Constant::F32(bits) => RuntimeValue::F32(*bits),
        Constant::F64(bits) => RuntimeValue::F64(*bits),
        Constant::Bool(value) => RuntimeValue::Bool(*value),
        Constant::Char(value) => RuntimeValue::Char(*value),
        Constant::Null => RuntimeValue::Null,
        Constant::String(_) => return Err(AdmissionError::InvalidEntry),
    })
}

fn resolve_value_type(
    artifact: &DecodedArtifact,
    module: usize,
    value: ValueType,
) -> Result<ResolvedValueType, AdmissionError> {
    Ok(ResolvedValueType {
        kind: value.kind,
        nullable: value.flags & 1 != 0,
        nominal: if value.kind == 7 {
            Some(
                resolve_type(artifact, module, value.nominal_type)
                    .ok_or(AdmissionError::InvalidEntry)?,
            )
        } else {
            None
        },
    })
}

fn resolve_type(artifact: &DecodedArtifact, module: usize, reference: TypeId) -> Option<TypeKey> {
    if reference.0 == u32::MAX {
        return None;
    }
    if reference.0 & 0x8000_0000 == 0 {
        let ty = reference.0 as usize;
        return (ty < artifact.modules.get(module)?.types.len()).then_some(TypeKey {
            module: module as u32,
            ty: reference.0,
        });
    }
    let import = artifact
        .modules
        .get(module)?
        .imports
        .get((reference.0 & 0x7fff_ffff) as usize)?;
    if import.kind != 0 {
        return None;
    }
    let target_module = import.target_module.0 as usize;
    let target = artifact.modules.get(target_module)?;
    let name = import.target_name as usize;
    let import_name = target.strings.get(name)?.slice(&artifact.bytes);
    target
        .exports
        .iter()
        .find(|export| {
            export.kind == 0
                && target
                    .strings
                    .get(export.name as usize)
                    .is_some_and(|range| range.slice(&artifact.bytes) == import_name)
        })
        .map(|export| TypeKey {
            module: target_module as u32,
            ty: export.local_symbol,
        })
}

fn resolve_function(
    artifact: &DecodedArtifact,
    module: usize,
    reference: u32,
) -> Option<FunctionKey> {
    if reference == u32::MAX {
        return None;
    }
    if reference & 0x8000_0000 == 0 {
        return artifact
            .modules
            .get(module)?
            .functions
            .get(reference as usize)
            .map(|_| FunctionKey {
                module: module as u32,
                function: reference,
            });
    }
    let import = artifact
        .modules
        .get(module)?
        .imports
        .get((reference & 0x7fff_ffff) as usize)?;
    if import.kind != 1 {
        return None;
    }
    let target_module = import.target_module.0 as usize;
    let target = artifact.modules.get(target_module)?;
    let import_name = target
        .strings
        .get(import.target_name as usize)?
        .slice(&artifact.bytes);
    target
        .exports
        .iter()
        .find(|export| {
            export.kind == 1
                && target
                    .strings
                    .get(export.name as usize)
                    .is_some_and(|range| range.slice(&artifact.bytes) == import_name)
        })
        .and_then(|export| {
            target
                .functions
                .get(export.local_symbol as usize)
                .map(|_| FunctionKey {
                    module: target_module as u32,
                    function: export.local_symbol,
                })
        })
}

fn resolve_instruction(
    artifact: &DecodedArtifact,
    module: usize,
    function_offsets: &[usize],
    block_offsets: &[usize],
    constant_offsets: &[usize],
    instruction: &Instruction,
) -> Result<ResolvedInstruction, AdmissionError> {
    macro_rules! binary {
        ($variant:ident, $form:expr, $dst:expr, $lhs:expr, $rhs:expr) => {
            ResolvedInstruction::$variant {
                form: *$form,
                dst: *$dst,
                lhs: *$lhs,
                rhs: *$rhs,
            }
        };
    }
    let block = |target: u32| {
        block_offsets[module]
            .checked_add(target as usize)
            .ok_or(AdmissionError::StoragePlanOverflow)
    };
    Ok(match instruction {
        Instruction::Nop => ResolvedInstruction::Nop,
        Instruction::Move { dst, src } => ResolvedInstruction::Move {
            dst: *dst,
            src: *src,
        },
        Instruction::Const { dst, constant } => ResolvedInstruction::Const {
            dst: *dst,
            constant: constant_offsets[module]
                .checked_add(*constant as usize)
                .ok_or(AdmissionError::StoragePlanOverflow)?,
        },
        Instruction::Null { dst } => ResolvedInstruction::Null { dst: *dst },
        Instruction::Convert { dst, src } => ResolvedInstruction::Convert {
            dst: *dst,
            src: *src,
        },
        Instruction::Add {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(Add, form, dst, lhs, rhs),
        Instruction::Sub {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(Sub, form, dst, lhs, rhs),
        Instruction::Mul {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(Mul, form, dst, lhs, rhs),
        Instruction::Div {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(Div, form, dst, lhs, rhs),
        Instruction::Rem {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(Rem, form, dst, lhs, rhs),
        Instruction::Neg { form, dst, src } => ResolvedInstruction::Neg {
            form: *form,
            dst: *dst,
            src: *src,
        },
        Instruction::BitAnd {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(BitAnd, form, dst, lhs, rhs),
        Instruction::BitOr {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(BitOr, form, dst, lhs, rhs),
        Instruction::BitXor {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(BitXor, form, dst, lhs, rhs),
        Instruction::ShiftLeft {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(ShiftLeft, form, dst, lhs, rhs),
        Instruction::ShiftRight {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(ShiftRight, form, dst, lhs, rhs),
        Instruction::ShiftUnsigned {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(ShiftUnsigned, form, dst, lhs, rhs),
        Instruction::Equal {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(Equal, form, dst, lhs, rhs),
        Instruction::NotEqual {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(NotEqual, form, dst, lhs, rhs),
        Instruction::Less {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(Less, form, dst, lhs, rhs),
        Instruction::LessEqual {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(LessEqual, form, dst, lhs, rhs),
        Instruction::Greater {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(Greater, form, dst, lhs, rhs),
        Instruction::GreaterEqual {
            form,
            dst,
            lhs,
            rhs,
        } => binary!(GreaterEqual, form, dst, lhs, rhs),
        Instruction::RefEqual { dst, lhs, rhs } => ResolvedInstruction::RefEqual {
            dst: *dst,
            lhs: *lhs,
            rhs: *rhs,
        },
        Instruction::RefNotEqual { dst, lhs, rhs } => ResolvedInstruction::RefNotEqual {
            dst: *dst,
            lhs: *lhs,
            rhs: *rhs,
        },
        Instruction::CallDirect {
            dst,
            function_ref,
            args,
        } => {
            let key = resolve_function(artifact, module, *function_ref)
                .ok_or(AdmissionError::InvalidEntry)?;
            let target = function_offsets[key.module as usize]
                .checked_add(key.function as usize)
                .ok_or(AdmissionError::StoragePlanOverflow)?;
            ResolvedInstruction::CallDirect {
                dst: *dst,
                target,
                args: args.clone(),
            }
        }
        Instruction::Jump { target } => ResolvedInstruction::Jump {
            target: block(*target)?,
        },
        Instruction::Branch {
            condition,
            true_block,
            false_block,
        } => ResolvedInstruction::Branch {
            condition: *condition,
            true_block: block(*true_block)?,
            false_block: block(*false_block)?,
        },
        Instruction::SwitchI32 {
            key,
            default_block,
            cases,
        } => {
            let mut resolved = reserved(cases.len())?;
            for SwitchCase { value, target } in cases.iter() {
                resolved.push(ResolvedSwitchCase {
                    value: *value,
                    target: block(*target)?,
                });
            }
            ResolvedInstruction::SwitchI32 {
                key: *key,
                default_block: block(*default_block)?,
                cases: resolved.into_boxed_slice(),
            }
        }
        Instruction::Return { value } => ResolvedInstruction::Return { value: *value },
        Instruction::Unreachable => ResolvedInstruction::Unreachable,
        _ => return Err(AdmissionError::InvalidEntry),
    })
}

fn type_exists(artifact: &DecodedArtifact, key: TypeKey) -> bool {
    artifact
        .modules
        .get(key.module as usize)
        .and_then(|module| module.types.get(key.ty as usize))
        .is_some()
}

fn assignable_types(
    artifact: &DecodedArtifact,
    actual: TypeKey,
) -> Result<Box<[TypeKey]>, AdmissionError> {
    let mut result = reserved(
        artifact
            .modules
            .iter()
            .map(|module| module.types.len())
            .sum(),
    )?;
    let mut pending = reserved(result.capacity().max(1))?;
    pending.push(actual);
    while let Some(current) = pending.pop() {
        if result.contains(&current) {
            continue;
        }
        result.push(current);
        match &artifact.modules[current.module as usize].types[current.ty as usize] {
            NominalType::Class {
                super_type,
                interfaces,
                ..
            }
            | NominalType::Interface {
                super_type,
                interfaces,
                ..
            } => {
                if let Some(parent) = resolve_type(artifact, current.module as usize, *super_type) {
                    if !result.contains(&parent) && !pending.contains(&parent) {
                        pending.push(parent);
                    }
                }
                for interface in interfaces {
                    let interface = resolve_type(artifact, current.module as usize, *interface)
                        .ok_or(AdmissionError::InvalidEntry)?;
                    if !result.contains(&interface) && !pending.contains(&interface) {
                        pending.push(interface);
                    }
                }
            }
            NominalType::Array { .. } | NominalType::Function { .. } => {}
        }
    }
    Ok(result.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{error::AdmissionError, fixtures, FunctionKey};

    #[test]
    fn portable_frame_charge_is_checked_and_aligned() {
        assert_eq!(48, frame_charge(1).unwrap());
        assert_eq!(64, frame_charge(2).unwrap());
        assert_eq!(160, frame_charge(8).unwrap());
        assert_eq!(
            Err(AdmissionError::StoragePlanOverflow),
            frame_charge(u64::MAX)
        );
    }

    #[test]
    fn admission_resolves_entry_and_rejects_non_tier0_families() {
        let artifact = fixtures::scalar_artifact();
        let image = ExecutionImage::admit(artifact, fixtures::profile()).unwrap();
        assert_eq!(
            image.entry(),
            FunctionKey {
                module: 0,
                function: 0
            }
        );
        assert!(image
            .functions()
            .iter()
            .all(|function| function.register_count <= image.registers_per_frame()));

        let artifact = fixtures::artifact_with_new_object();
        assert_eq!(
            Err(AdmissionError::InvalidEntry),
            ExecutionImage::admit(artifact, fixtures::profile()).map(|_| ())
        );
    }

    #[test]
    fn admission_is_atomic_across_profile_limits_and_identities() {
        for profile in fixtures::profiles_below_each_manifest_limit() {
            assert!(ExecutionImage::admit(fixtures::scalar_artifact(), profile).is_err());
        }
    }
}
