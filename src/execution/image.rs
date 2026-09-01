use std::sync::Arc;

use crate::artifact::{
    ByteRange, Constant, DecodedArtifact, Instruction, NominalType, SwitchCase, TypeId, ValueType,
};
use crate::VerifiedArtifact;

use super::{
    error::AdmissionError,
    host::{HostValueType, ResolvedCapability},
    layout::{object_layout, FieldSpec, RuntimeTypeLayout, StoragePlan, ValueWidth},
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
    pub platform_abi: [u8; 32],
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
    StringLength {
        dst: u16,
        string: u16,
    },
    StringGet {
        dst: u16,
        string: u16,
        index: u16,
    },
    StringEquals {
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    StringCompare {
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    StringHash {
        dst: u16,
        string: u16,
    },
    StringConcat {
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    StringSubstring {
        dst: u16,
        string: u16,
        start: u16,
        end: u16,
    },
    StringFromCharArray {
        dst: u16,
        array: u16,
        start: u16,
        end: u16,
    },
    NewObject {
        dst: u16,
        ty: TypeKey,
    },
    NewArray {
        dst: u16,
        ty: TypeKey,
        length: u16,
    },
    StaticGet {
        dst: u16,
        field: ResolvedField,
    },
    StaticSet {
        field: ResolvedField,
        value: u16,
    },
    FieldGet {
        dst: u16,
        receiver: u16,
        field: ResolvedField,
    },
    FieldSet {
        receiver: u16,
        field: ResolvedField,
        value: u16,
    },
    IsType {
        dst: u16,
        value: u16,
        ty: TypeKey,
    },
    ArrayLength {
        dst: u16,
        array: u16,
    },
    ArrayLoad {
        dst: u16,
        array: u16,
        index: u16,
    },
    ArrayStore {
        array: u16,
        index: u16,
        value: u16,
    },
    CheckedCast {
        dst: u16,
        value: u16,
        ty: TypeKey,
    },
    CallDirect {
        dst: u16,
        target: usize,
        args: Box<[u16]>,
    },
    CallSuspend {
        dst: u16,
        target: usize,
        args: Box<[u16]>,
        resume_block: usize,
    },
    CapabilityCallSync {
        dst: u16,
        capability: u32,
        operation: u32,
        args: Box<[u16]>,
    },
    CapabilityCallAsync {
        dst: u16,
        capability: u32,
        operation: u32,
        args: Box<[u16]>,
        resume_block: usize,
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
    pub ty: TypeKey,
    pub live: bool,
    pub assignable_to: Box<[TypeKey]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ResolvedField {
    pub owner: TypeKey,
    pub value_type: ResolvedValueType,
    pub offset: Option<u32>,
    pub static_slot: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ResolvedLiteral {
    pub bytes: ByteRange,
    pub code_units: u32,
}

type ResolvedLiterals = (Box<[ResolvedLiteral]>, Box<[usize]>);

#[derive(Clone, Debug)]
pub(super) struct ExecutionImage(Arc<ExecutionImageInner>);

#[derive(Debug)]
struct ExecutionImageInner {
    content_hash: [u8; 32],
    artifact_bytes: Arc<[u8]>,
    entry: usize,
    functions: Box<[ResolvedFunction]>,
    blocks: Box<[ResolvedBlock]>,
    constants: Box<[RuntimeValue]>,
    host_references: Box<[ResolvedHostReference]>,
    type_offsets: Box<[usize]>,
    type_layouts: Box<[RuntimeTypeLayout]>,
    assignable_types: Box<[Box<[TypeKey]>]>,
    array_element_types: Box<[Option<ResolvedValueType>]>,
    fields: Box<[ResolvedField]>,
    literals: Box<[ResolvedLiteral]>,
    literal_ids: Box<[usize]>,
    string_type: Option<TypeKey>,
    storage_plan: StoragePlan,
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
        Self::admit_with_capabilities(artifact, profile, &[])
    }

    pub(super) fn admit_with_capabilities(
        artifact: VerifiedArtifact,
        profile: ExecutionProfile,
        capabilities: &[Option<ResolvedCapability>],
    ) -> Result<Self, AdmissionError> {
        let decoded = artifact.decoded();
        check_profile(decoded, &profile)?;
        let function_offsets =
            offsets(decoded.modules.iter().map(|module| module.functions.len()))?;
        let block_offsets = offsets(decoded.modules.iter().map(|module| module.blocks.len()))?;
        let constant_offsets =
            offsets(decoded.modules.iter().map(|module| module.constants.len()))?;
        let type_offsets = offsets(decoded.modules.iter().map(|module| module.types.len()))?;
        let field_offsets = offsets(decoded.modules.iter().map(|module| module.fields.len()))?;
        let literal_offsets = offsets(
            decoded
                .modules
                .iter()
                .map(|module| module.utf16_literals.len()),
        )?;
        let type_layouts = derive_type_layouts(decoded, &type_offsets, &field_offsets)?;
        let mut assignable_type_sets =
            reserved(*type_offsets.last().ok_or(AdmissionError::InvalidEntry)?)?;
        for (module, nominal_types) in decoded.modules.iter().enumerate() {
            for ty in 0..nominal_types.types.len() {
                assignable_type_sets.push(assignable_types(
                    decoded,
                    TypeKey {
                        module: checked_u32(module)?,
                        ty: checked_u32(ty)?,
                    },
                )?);
            }
        }
        let mut array_element_types =
            reserved(*type_offsets.last().ok_or(AdmissionError::InvalidEntry)?)?;
        for (module, nominal_types) in decoded.modules.iter().enumerate() {
            for nominal in &nominal_types.types {
                array_element_types.push(match nominal {
                    NominalType::Array { element, .. } => {
                        Some(resolve_value_type(decoded, module, *element)?)
                    }
                    _ => None,
                });
            }
        }
        let (fields, static_slot_count) =
            resolve_fields(decoded, &type_offsets, &field_offsets, &type_layouts)?;
        let string_type = resolve_standard_string_type(decoded)?;
        let (mut literals, literal_ids) = resolve_literals(decoded, &literal_offsets)?;
        if string_type.is_some() && !literals.iter().any(|literal| literal.code_units == 0) {
            let mut with_empty = literals.into_vec();
            with_empty
                .try_reserve_exact(1)
                .map_err(|_| AdmissionError::AllocationFailed)?;
            with_empty.push(ResolvedLiteral {
                bytes: ByteRange { start: 0, end: 0 },
                code_units: 0,
            });
            literals = with_empty.into_boxed_slice();
        }
        let storage_plan = StoragePlan {
            heap_bytes: profile.heap_bytes,
            handle_capacity: profile.heap_bytes / 32,
            type_count: checked_u32(type_layouts.len())?,
            field_count: checked_u32(fields.len())?,
            static_slot_count,
            literal_count: checked_u32(literals.len())?,
            literal_id_count: checked_u32(literal_ids.len())?,
            reference_offset_count: checked_u32(
                type_layouts
                    .iter()
                    .map(|layout| match layout {
                        RuntimeTypeLayout::Object(object) => object.reference_offsets.len(),
                        RuntimeTypeLayout::Array { .. } | RuntimeTypeLayout::NonHeap => 0,
                    })
                    .try_fold(0_usize, |total, count| total.checked_add(count))
                    .ok_or(AdmissionError::StoragePlanOverflow)?,
            )?,
        };

        let mut constants = reserved(
            *constant_offsets
                .last()
                .ok_or(AdmissionError::InvalidEntry)?,
        )?;
        for (module_id, module) in decoded.modules.iter().enumerate() {
            for constant in &module.constants {
                constants.push(resolve_constant(
                    constant,
                    module_id,
                    &literal_offsets,
                    &literal_ids,
                )?);
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
        let instruction_resolution = InstructionResolution {
            function_offsets: &function_offsets,
            block_offsets: &block_offsets,
            constant_offsets: &constant_offsets,
            field_offsets: &field_offsets,
            fields: &fields,
            functions: &functions,
            capabilities,
            string_type,
        };
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
                        function,
                        &instruction_resolution,
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
                value: ReferenceValue::host(reference.handle, reference.generation)
                    .ok_or(AdmissionError::StoragePlanOverflow)?,
                ty: reference.ty,
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
        Ok(Self(Arc::new(ExecutionImageInner {
            content_hash: artifact.content_hash(),
            artifact_bytes: decoded.bytes.clone(),
            entry,
            functions: functions.into_boxed_slice(),
            blocks: blocks.into_boxed_slice(),
            constants: constants.into_boxed_slice(),
            host_references: host_references.into_boxed_slice(),
            type_offsets,
            type_layouts,
            assignable_types: assignable_type_sets.into_boxed_slice(),
            array_element_types: array_element_types.into_boxed_slice(),
            fields,
            literals,
            literal_ids,
            string_type,
            storage_plan,
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

    pub(super) fn entry_index(&self) -> usize {
        self.0.entry
    }

    pub(super) fn function(&self, index: usize) -> Option<&ResolvedFunction> {
        self.0.functions.get(index)
    }

    pub(super) fn block(&self, index: usize) -> Option<&ResolvedBlock> {
        self.0.blocks.get(index)
    }

    pub(super) fn constant(&self, index: usize) -> Option<RuntimeValue> {
        self.0.constants.get(index).copied()
    }

    pub(super) fn content_hash(&self) -> [u8; 32] {
        self.0.content_hash
    }

    pub(super) fn maximum_call_depth(&self) -> usize {
        self.0.maximum_call_depth
    }

    pub(super) fn minimum_slice_cost(&self) -> u32 {
        self.0.minimum_slice_cost
    }

    pub(super) fn maximum_slice_budget(&self) -> u32 {
        self.0.maximum_slice_budget
    }

    pub(super) fn host_reference(&self, value: ReferenceValue) -> Option<&ResolvedHostReference> {
        self.0
            .host_references
            .iter()
            .find(|reference| reference.value == value)
    }

    pub(super) fn reference_type(&self, value: ReferenceValue) -> Option<TypeKey> {
        match value.domain() {
            super::value::ReferenceDomain::Host => self
                .host_reference(value)
                .filter(|entry| entry.live)
                .map(|entry| entry.ty),
            super::value::ReferenceDomain::Literal => {
                self.literal_reference(value).and(self.0.string_type)
            }
            super::value::ReferenceDomain::Managed | super::value::ReferenceDomain::Emergency => {
                None
            }
        }
    }

    pub(super) fn storage_plan(&self) -> StoragePlan {
        self.0.storage_plan
    }

    pub(super) fn type_layout(&self, key: TypeKey) -> Option<&RuntimeTypeLayout> {
        let index = checked_global_index(&self.0.type_offsets, key)?;
        self.0.type_layouts.get(index)
    }

    pub(super) fn is_assignable(&self, actual: TypeKey, target: TypeKey) -> bool {
        checked_global_index(&self.0.type_offsets, actual)
            .and_then(|index| self.0.assignable_types.get(index))
            .is_some_and(|types| types.contains(&target))
    }

    pub(super) fn array_element_type(&self, array: TypeKey) -> Option<ResolvedValueType> {
        let index = checked_global_index(&self.0.type_offsets, array)?;
        self.0.array_element_types.get(index).copied().flatten()
    }

    pub(super) fn field(&self, index: usize) -> Option<&ResolvedField> {
        self.0.fields.get(index)
    }

    pub(super) fn fields(&self) -> &[ResolvedField] {
        &self.0.fields
    }

    pub(super) fn literal(&self, index: usize) -> Option<&ResolvedLiteral> {
        let canonical = *self.0.literal_ids.get(index)?;
        self.0.literals.get(canonical)
    }

    pub(super) fn literal_bytes(&self, literal: ResolvedLiteral) -> &[u8] {
        literal.bytes.slice(&self.0.artifact_bytes)
    }

    pub(super) fn literal_reference(&self, value: ReferenceValue) -> Option<ResolvedLiteral> {
        (value.domain() == super::value::ReferenceDomain::Literal)
            .then(|| self.0.literals.get(value.slot() as usize).copied())
            .flatten()
    }

    pub(super) fn string_type(&self) -> Option<TypeKey> {
        self.0.string_type
    }

    pub(super) fn empty_string(&self) -> Option<RuntimeValue> {
        let index = self
            .0
            .literals
            .iter()
            .position(|literal| literal.code_units == 0)?;
        Some(RuntimeValue::Reference(ReferenceValue::literal(
            index as u32,
        )?))
    }
}

fn resolve_standard_string_type(
    artifact: &DecodedArtifact,
) -> Result<Option<TypeKey>, AdmissionError> {
    let mut found = None;
    for (module_id, module) in artifact.modules.iter().enumerate() {
        if module.flags != 2 {
            continue;
        }
        for export in &module.exports {
            let is_string = export.kind == 0
                && export.visibility == 1
                && module
                    .strings
                    .get(export.name as usize)
                    .is_some_and(|name| name.slice(&artifact.bytes) == b"kotlin.String");
            if !is_string {
                continue;
            }
            let candidate = TypeKey {
                module: checked_u32(module_id)?,
                ty: export.local_symbol,
            };
            if found.replace(candidate).is_some() {
                return Err(AdmissionError::InvalidEntry);
            }
        }
    }
    Ok(found)
}

fn derive_type_layouts(
    artifact: &DecodedArtifact,
    type_offsets: &[usize],
    field_offsets: &[usize],
) -> Result<Box<[RuntimeTypeLayout]>, AdmissionError> {
    let total = *type_offsets.last().ok_or(AdmissionError::InvalidEntry)?;
    let mut layouts = reserved(total)?;
    layouts.resize_with(total, || None);
    let mut visiting = reserved(total)?;
    visiting.resize(total, false);
    for (module, module_types) in artifact.modules.iter().enumerate() {
        for ty in 0..module_types.types.len() {
            derive_type_layout(
                artifact,
                type_offsets,
                field_offsets,
                TypeKey {
                    module: checked_u32(module)?,
                    ty: checked_u32(ty)?,
                },
                &mut layouts,
                &mut visiting,
            )?;
        }
    }
    let mut resolved = reserved(total)?;
    for layout in layouts {
        resolved.push(layout.ok_or(AdmissionError::InvalidEntry)?);
    }
    Ok(resolved.into_boxed_slice())
}

fn derive_type_layout(
    artifact: &DecodedArtifact,
    type_offsets: &[usize],
    field_offsets: &[usize],
    key: TypeKey,
    layouts: &mut [Option<RuntimeTypeLayout>],
    visiting: &mut [bool],
) -> Result<(), AdmissionError> {
    let index = global_index(type_offsets, key)?;
    if layouts[index].is_some() {
        return Ok(());
    }
    if core::mem::replace(&mut visiting[index], true) {
        return Err(AdmissionError::InvalidEntry);
    }
    let module = artifact
        .modules
        .get(key.module as usize)
        .ok_or(AdmissionError::InvalidEntry)?;
    let nominal = module
        .types
        .get(key.ty as usize)
        .ok_or(AdmissionError::InvalidEntry)?;
    let layout = match nominal {
        NominalType::Class {
            super_type,
            field_start,
            field_count,
            ..
        } => {
            let superclass = resolve_type(artifact, key.module as usize, *super_type);
            let inherited = if let Some(superclass) = superclass {
                derive_type_layout(
                    artifact,
                    type_offsets,
                    field_offsets,
                    superclass,
                    layouts,
                    visiting,
                )?;
                match layouts[global_index(type_offsets, superclass)?].as_ref() {
                    Some(RuntimeTypeLayout::Object(layout)) => Some(layout),
                    _ => return Err(AdmissionError::InvalidEntry),
                }
            } else {
                None
            };
            let start = *field_start as usize;
            let count = *field_count as usize;
            let end = start
                .checked_add(count)
                .ok_or(AdmissionError::StoragePlanOverflow)?;
            let declared = module
                .fields
                .get(start..end)
                .ok_or(AdmissionError::InvalidEntry)?;
            let mut specs = reserved(declared.len())?;
            for (relative, field) in declared.iter().enumerate() {
                if field.flags & 2 != 0 {
                    continue;
                }
                let local_field = start
                    .checked_add(relative)
                    .ok_or(AdmissionError::StoragePlanOverflow)?;
                let field = field_offsets[key.module as usize]
                    .checked_add(local_field)
                    .ok_or(AdmissionError::StoragePlanOverflow)?;
                specs.push(FieldSpec {
                    field: checked_u32(field)?,
                    width: value_width(field_value_type(module, local_field)?)?,
                });
            }
            RuntimeTypeLayout::Object(object_layout(inherited, &specs)?)
        }
        NominalType::Array { element, .. } => RuntimeTypeLayout::Array {
            element: value_width(*element)?,
        },
        NominalType::Interface { .. } | NominalType::Function { .. } => RuntimeTypeLayout::NonHeap,
    };
    visiting[index] = false;
    layouts[index] = Some(layout);
    Ok(())
}

fn field_value_type(
    module: &crate::artifact::DecodedModule,
    local_field: usize,
) -> Result<ValueType, AdmissionError> {
    module
        .fields
        .get(local_field)
        .map(|field| field.value_type)
        .ok_or(AdmissionError::InvalidEntry)
}

fn resolve_fields(
    artifact: &DecodedArtifact,
    type_offsets: &[usize],
    field_offsets: &[usize],
    type_layouts: &[RuntimeTypeLayout],
) -> Result<(Box<[ResolvedField]>, u32), AdmissionError> {
    let total = *field_offsets.last().ok_or(AdmissionError::InvalidEntry)?;
    let mut fields = reserved(total)?;
    let mut static_slot_count = 0_u32;
    for (module_id, module) in artifact.modules.iter().enumerate() {
        for (local_field, field) in module.fields.iter().enumerate() {
            let owner = resolve_type(artifact, module_id, field.owner)
                .ok_or(AdmissionError::InvalidEntry)?;
            let global_field = field_offsets[module_id]
                .checked_add(local_field)
                .ok_or(AdmissionError::StoragePlanOverflow)?;
            let is_static = field.flags & 2 != 0;
            let static_slot = if is_static {
                let slot = static_slot_count;
                static_slot_count = static_slot_count
                    .checked_add(1)
                    .ok_or(AdmissionError::StoragePlanOverflow)?;
                Some(slot)
            } else {
                None
            };
            let offset = if is_static {
                None
            } else {
                match type_layouts.get(global_index(type_offsets, owner)?) {
                    Some(RuntimeTypeLayout::Object(layout)) => layout
                        .fields
                        .iter()
                        .find(|layout| layout.field as usize == global_field)
                        .map(|layout| layout.offset),
                    _ => None,
                }
                .ok_or(AdmissionError::InvalidEntry)
                .map(Some)?
            };
            fields.push(ResolvedField {
                owner,
                value_type: resolve_value_type(artifact, module_id, field.value_type)?,
                offset,
                static_slot,
            });
        }
    }
    Ok((fields.into_boxed_slice(), static_slot_count))
}

fn resolve_literals(
    artifact: &DecodedArtifact,
    literal_offsets: &[usize],
) -> Result<ResolvedLiterals, AdmissionError> {
    let total = *literal_offsets.last().ok_or(AdmissionError::InvalidEntry)?;
    let mut ranges = reserved(total)?;
    for module in &artifact.modules {
        for range in &module.utf16_literals {
            ranges.push(*range);
        }
    }
    deduplicate_literal_ranges(&artifact.bytes, &ranges)
}

pub(super) fn deduplicate_literal_ranges(
    bytes: &[u8],
    ranges: &[ByteRange],
) -> Result<ResolvedLiterals, AdmissionError> {
    let mut order = reserved(ranges.len())?;
    order.extend(0..ranges.len());
    order.sort_unstable_by(|left, right| {
        ranges[*left]
            .slice(bytes)
            .cmp(ranges[*right].slice(bytes))
            .then_with(|| left.cmp(right))
    });

    let mut unique_count = 0_usize;
    let mut previous: Option<&[u8]> = None;
    for index in &order {
        let raw = ranges[*index].slice(bytes);
        if previous != Some(raw) {
            unique_count = unique_count
                .checked_add(1)
                .ok_or(AdmissionError::StoragePlanOverflow)?;
            previous = Some(raw);
        }
    }

    let mut literals: Vec<ResolvedLiteral> = reserved(unique_count)?;
    let mut literal_ids = reserved(ranges.len())?;
    literal_ids.resize(ranges.len(), usize::MAX);
    for source_id in order {
        let range = ranges[source_id];
        let raw = range.slice(bytes);
        if literals
            .last()
            .is_none_or(|literal| literal.bytes.slice(bytes) != raw)
        {
            let code_units = checked_u32(raw.len() / 2)?;
            literals.push(ResolvedLiteral {
                bytes: range,
                code_units,
            });
        }
        literal_ids[source_id] = literals.len() - 1;
    }
    Ok((literals.into_boxed_slice(), literal_ids.into_boxed_slice()))
}

fn global_index(offsets: &[usize], key: TypeKey) -> Result<usize, AdmissionError> {
    checked_global_index(offsets, key).ok_or(AdmissionError::InvalidEntry)
}

fn checked_global_index(offsets: &[usize], key: TypeKey) -> Option<usize> {
    let module = key.module as usize;
    let start = *offsets.get(module)?;
    let end = *offsets.get(module.checked_add(1)?)?;
    start
        .checked_add(key.ty as usize)
        .filter(|index| *index < end)
}

fn checked_u32(value: usize) -> Result<u32, AdmissionError> {
    u32::try_from(value).map_err(|_| AdmissionError::StoragePlanOverflow)
}

fn value_width(value: ValueType) -> Result<ValueWidth, AdmissionError> {
    match value.kind {
        1 => Ok(ValueWidth::I32),
        2 => Ok(ValueWidth::I64),
        3 => Ok(ValueWidth::F32),
        4 => Ok(ValueWidth::F64),
        5 => Ok(ValueWidth::Bool),
        6 => Ok(ValueWidth::Char),
        7 => Ok(ValueWidth::Ref),
        _ => Err(AdmissionError::InvalidEntry),
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
    if profile.heap_bytes < 32 || !profile.heap_bytes.is_multiple_of(16) {
        return Err(AdmissionError::InvalidHeapSize {
            supplied: profile.heap_bytes,
        });
    }
    let manifest = artifact.manifest;
    if manifest.compiler_abi != profile.compiler_abi {
        return Err(AdmissionError::CompilerAbiMismatch);
    }
    if manifest.platform_abi != profile.platform_abi {
        return Err(AdmissionError::PlatformAbiMismatch);
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

fn resolve_constant(
    value: &Constant,
    module_id: usize,
    literal_offsets: &[usize],
    literal_ids: &[usize],
) -> Result<RuntimeValue, AdmissionError> {
    Ok(match value {
        Constant::I32(value) => RuntimeValue::I32(*value),
        Constant::I64(value) => RuntimeValue::I64(*value),
        Constant::F32(bits) => RuntimeValue::F32(*bits),
        Constant::F64(bits) => RuntimeValue::F64(*bits),
        Constant::Bool(value) => RuntimeValue::Bool(*value),
        Constant::Char(value) => RuntimeValue::Char(*value),
        Constant::Null => RuntimeValue::Null,
        Constant::String(literal) => {
            let source = literal_offsets
                .get(module_id)
                .and_then(|offset| offset.checked_add(literal.0 as usize))
                .ok_or(AdmissionError::InvalidEntry)?;
            let canonical = *literal_ids
                .get(source)
                .ok_or(AdmissionError::InvalidEntry)?;
            RuntimeValue::Reference(
                ReferenceValue::literal(
                    u32::try_from(canonical).map_err(|_| AdmissionError::StoragePlanOverflow)?,
                )
                .ok_or(AdmissionError::StoragePlanOverflow)?,
            )
        }
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

fn resolve_field_index(
    artifact: &DecodedArtifact,
    module: usize,
    field_offsets: &[usize],
    reference: u32,
) -> Result<usize, AdmissionError> {
    let (target_module, local_field) = if reference & 0x8000_0000 == 0 {
        (module, reference)
    } else {
        let import = artifact
            .modules
            .get(module)
            .and_then(|module| module.imports.get((reference & 0x7fff_ffff) as usize))
            .filter(|import| import.kind == 2)
            .ok_or(AdmissionError::InvalidEntry)?;
        let target_module = import.target_module.0 as usize;
        let target = artifact
            .modules
            .get(target_module)
            .ok_or(AdmissionError::InvalidEntry)?;
        let import_name = target
            .strings
            .get(import.target_name as usize)
            .ok_or(AdmissionError::InvalidEntry)?
            .slice(&artifact.bytes);
        let export = target
            .exports
            .iter()
            .find(|export| {
                export.kind == 2
                    && target
                        .strings
                        .get(export.name as usize)
                        .is_some_and(|range| range.slice(&artifact.bytes) == import_name)
            })
            .ok_or(AdmissionError::InvalidEntry)?;
        (target_module, export.local_symbol)
    };
    let start = *field_offsets
        .get(target_module)
        .ok_or(AdmissionError::InvalidEntry)?;
    let end = *field_offsets
        .get(target_module + 1)
        .ok_or(AdmissionError::InvalidEntry)?;
    start
        .checked_add(local_field as usize)
        .filter(|index| *index < end)
        .ok_or(AdmissionError::InvalidEntry)
}

struct InstructionResolution<'a> {
    function_offsets: &'a [usize],
    block_offsets: &'a [usize],
    constant_offsets: &'a [usize],
    field_offsets: &'a [usize],
    fields: &'a [ResolvedField],
    functions: &'a [ResolvedFunction],
    capabilities: &'a [Option<ResolvedCapability>],
    string_type: Option<TypeKey>,
}

fn resolve_instruction(
    artifact: &DecodedArtifact,
    module: usize,
    function: usize,
    resolution: &InstructionResolution<'_>,
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
        resolution.block_offsets[module]
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
            constant: resolution.constant_offsets[module]
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
        Instruction::StringLength { dst, string } => ResolvedInstruction::StringLength {
            dst: *dst,
            string: *string,
        },
        Instruction::StringGet { dst, string, index } => ResolvedInstruction::StringGet {
            dst: *dst,
            string: *string,
            index: *index,
        },
        Instruction::StringEquals { dst, lhs, rhs } => ResolvedInstruction::StringEquals {
            dst: *dst,
            lhs: *lhs,
            rhs: *rhs,
        },
        Instruction::StringCompare { dst, lhs, rhs } => ResolvedInstruction::StringCompare {
            dst: *dst,
            lhs: *lhs,
            rhs: *rhs,
        },
        Instruction::StringHash { dst, string } => ResolvedInstruction::StringHash {
            dst: *dst,
            string: *string,
        },
        Instruction::StringConcat { dst, lhs, rhs } => ResolvedInstruction::StringConcat {
            dst: *dst,
            lhs: *lhs,
            rhs: *rhs,
        },
        Instruction::StringSubstring {
            dst,
            string,
            start,
            end,
        } => ResolvedInstruction::StringSubstring {
            dst: *dst,
            string: *string,
            start: *start,
            end: *end,
        },
        Instruction::StringFromCharArray {
            dst,
            array,
            start,
            end,
        } => ResolvedInstruction::StringFromCharArray {
            dst: *dst,
            array: *array,
            start: *start,
            end: *end,
        },
        Instruction::NewObject { dst, type_ref } => ResolvedInstruction::NewObject {
            dst: *dst,
            ty: resolve_type(artifact, module, TypeId(*type_ref))
                .ok_or(AdmissionError::InvalidEntry)?,
        },
        Instruction::NewArray {
            dst,
            type_ref,
            length,
        } => ResolvedInstruction::NewArray {
            dst: *dst,
            ty: resolve_type(artifact, module, TypeId(*type_ref))
                .ok_or(AdmissionError::InvalidEntry)?,
            length: *length,
        },
        Instruction::StaticGet { dst, field_ref } => ResolvedInstruction::StaticGet {
            dst: *dst,
            field: *resolution
                .fields
                .get(resolve_field_index(
                    artifact,
                    module,
                    resolution.field_offsets,
                    *field_ref,
                )?)
                .ok_or(AdmissionError::InvalidEntry)?,
        },
        Instruction::StaticSet { field_ref, value } => ResolvedInstruction::StaticSet {
            field: *resolution
                .fields
                .get(resolve_field_index(
                    artifact,
                    module,
                    resolution.field_offsets,
                    *field_ref,
                )?)
                .ok_or(AdmissionError::InvalidEntry)?,
            value: *value,
        },
        Instruction::FieldGet {
            dst,
            receiver,
            field_ref,
        } => ResolvedInstruction::FieldGet {
            dst: *dst,
            receiver: *receiver,
            field: *resolution
                .fields
                .get(resolve_field_index(
                    artifact,
                    module,
                    resolution.field_offsets,
                    *field_ref,
                )?)
                .ok_or(AdmissionError::InvalidEntry)?,
        },
        Instruction::FieldSet {
            receiver,
            field_ref,
            value,
        } => ResolvedInstruction::FieldSet {
            receiver: *receiver,
            field: *resolution
                .fields
                .get(resolve_field_index(
                    artifact,
                    module,
                    resolution.field_offsets,
                    *field_ref,
                )?)
                .ok_or(AdmissionError::InvalidEntry)?,
            value: *value,
        },
        Instruction::IsType {
            dst,
            value,
            type_ref,
        } => ResolvedInstruction::IsType {
            dst: *dst,
            value: *value,
            ty: resolve_type(artifact, module, TypeId(*type_ref))
                .ok_or(AdmissionError::InvalidEntry)?,
        },
        Instruction::ArrayLength { dst, array } => ResolvedInstruction::ArrayLength {
            dst: *dst,
            array: *array,
        },
        Instruction::ArrayLoad { dst, array, index } => ResolvedInstruction::ArrayLoad {
            dst: *dst,
            array: *array,
            index: *index,
        },
        Instruction::ArrayStore {
            array,
            index,
            value,
        } => ResolvedInstruction::ArrayStore {
            array: *array,
            index: *index,
            value: *value,
        },
        Instruction::CheckedCast {
            dst,
            value,
            type_ref,
        } => ResolvedInstruction::CheckedCast {
            dst: *dst,
            value: *value,
            ty: resolve_type(artifact, module, TypeId(*type_ref))
                .ok_or(AdmissionError::InvalidEntry)?,
        },
        Instruction::CallDirect {
            dst,
            function_ref,
            args,
        } => {
            let key = resolve_function(artifact, module, *function_ref)
                .ok_or(AdmissionError::InvalidEntry)?;
            let target = resolution.function_offsets[key.module as usize]
                .checked_add(key.function as usize)
                .ok_or(AdmissionError::StoragePlanOverflow)?;
            ResolvedInstruction::CallDirect {
                dst: *dst,
                target,
                args: args.clone(),
            }
        }
        Instruction::CallSuspend {
            dst,
            function_ref,
            args,
            resume_block,
        } => {
            let key = resolve_function(artifact, module, *function_ref)
                .ok_or(AdmissionError::InvalidEntry)?;
            let target = resolution.function_offsets[key.module as usize]
                .checked_add(key.function as usize)
                .ok_or(AdmissionError::StoragePlanOverflow)?;
            ResolvedInstruction::CallSuspend {
                dst: *dst,
                target,
                args: args.clone(),
                resume_block: block(*resume_block)?,
            }
        }
        Instruction::CapabilityCallSync {
            dst,
            capability,
            operation,
            args,
        } => {
            validate_capability_call(
                resolution,
                function,
                *dst,
                *capability,
                *operation,
                args,
                false,
            )?;
            ResolvedInstruction::CapabilityCallSync {
                dst: *dst,
                capability: *capability,
                operation: *operation,
                args: args.clone(),
            }
        }
        Instruction::CapabilityCallAsync {
            dst,
            capability,
            operation,
            args,
            resume_block,
        } => {
            validate_capability_call(
                resolution,
                function,
                *dst,
                *capability,
                *operation,
                args,
                true,
            )?;
            ResolvedInstruction::CapabilityCallAsync {
                dst: *dst,
                capability: *capability,
                operation: *operation,
                args: args.clone(),
                resume_block: block(*resume_block)?,
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

fn validate_capability_call(
    resolution: &InstructionResolution<'_>,
    function: usize,
    destination: u16,
    capability: u32,
    operation: u32,
    arguments: &[u16],
    asynchronous: bool,
) -> Result<(), AdmissionError> {
    let resolved_capability = resolution
        .capabilities
        .get(capability as usize)
        .and_then(Option::as_ref)
        .ok_or(AdmissionError::MissingCapability {
            index: u8::try_from(capability).unwrap_or(u8::MAX),
        })?;
    let schema = resolved_capability
        .operations
        .get(operation as usize)
        .ok_or(AdmissionError::CapabilityOperationCount {
            capability,
            required: operation.saturating_add(1),
            available: u32::try_from(resolved_capability.operations.len()).unwrap_or(u32::MAX),
        })?;
    let resolved_function = resolution
        .functions
        .get(function)
        .ok_or(AdmissionError::InvalidEntry)?;
    let arguments_match = arguments.len() == schema.arguments.len()
        && arguments
            .iter()
            .zip(schema.arguments.iter())
            .all(|(register, expected)| {
                resolved_function
                    .registers
                    .get(*register as usize)
                    .is_some_and(|actual| {
                        host_type_matches(*actual, *expected, resolution.string_type)
                    })
            });
    let result_matches = match schema.result {
        HostValueType::Unit => destination == u16::MAX,
        expected => {
            destination != u16::MAX
                && resolved_function
                    .registers
                    .get(destination as usize)
                    .is_some_and(|actual| {
                        host_type_matches(*actual, expected, resolution.string_type)
                    })
        }
    };
    if schema.asynchronous != asynchronous || !arguments_match || !result_matches {
        return Err(AdmissionError::CapabilitySchema {
            capability,
            operation,
        });
    }
    Ok(())
}

fn host_type_matches(
    actual: ResolvedValueType,
    expected: HostValueType,
    string_type: Option<TypeKey>,
) -> bool {
    match expected {
        HostValueType::Unit => false,
        HostValueType::I32 => actual.kind == 1,
        HostValueType::I64 => actual.kind == 2,
        HostValueType::F32 => actual.kind == 3,
        HostValueType::F64 => actual.kind == 4,
        HostValueType::Bool => actual.kind == 5,
        HostValueType::Char => actual.kind == 6,
        HostValueType::String => {
            actual.kind == 7 && !actual.nullable && actual.nominal == string_type
        }
    }
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
