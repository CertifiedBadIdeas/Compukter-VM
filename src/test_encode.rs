use std::fmt;

use sha2::{Digest, Sha256};

use crate::artifact::{
    format, Block, Capability, Constant, DebugEntry, DecodedArtifact, DecodedModule,
    ExceptionEntry, Export, Field, Function, Import, Instruction, Manifest, NominalType, ValueType,
};

#[derive(Debug)]
pub(crate) struct EncodeError(&'static str);

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

type Section = (u16, u32, Vec<u8>, u32);
type ModuleSection = (u16, Vec<u8>, u32);

pub(crate) fn encode_artifact(artifact: &DecodedArtifact) -> Result<Vec<u8>, EncodeError> {
    let mut encoded_modules = Vec::new();
    encoded_modules
        .try_reserve_exact(artifact.modules.len())
        .map_err(|_| EncodeError("cannot reserve encoded modules"))?;
    for module in &artifact.modules {
        encoded_modules.push(encode_module(artifact, module)?);
    }

    let manifest = encode_manifest(&artifact.manifest);
    let capability_records = artifact
        .capabilities
        .iter()
        .map(encode_capability)
        .collect::<Vec<_>>();
    let capabilities_count = to_u32(capability_records.len(), "capability count exceeds u32")?;
    let capabilities = indexed(capability_records)?;

    let mut module_records = Vec::new();
    for (module, (_, hash)) in artifact.modules.iter().zip(&encoded_modules) {
        if hash != &module.semantic_hash {
            return Err(EncodeError(
                "recomputed module hash differs from decoded hash",
            ));
        }
        module_records.push(encode_module_record(module, *hash));
    }
    let module_count = to_u32(module_records.len(), "module count exceeds u32")?;
    let modules = indexed(module_records)?;

    let mut sections = vec![
        (format::MANIFEST, 0, manifest, 1),
        (format::MODULES, 0, modules, module_count),
        (format::CAPABILITIES, 0, capabilities, capabilities_count),
    ];
    for (module_id, ((module, _), decoded)) in encoded_modules
        .into_iter()
        .zip(&artifact.modules)
        .enumerate()
    {
        let scope = to_u32(module_id + 1, "module scope exceeds u32")?;
        sections.extend(
            module
                .into_iter()
                .map(|(kind, payload, count)| (kind, scope, payload, count)),
        );
        if !decoded.debug.is_empty() {
            let records = decoded
                .debug
                .iter()
                .map(|entry| encode_debug(artifact, entry))
                .collect::<Result<Vec<_>, _>>()?;
            let count = to_u32(records.len(), "debug count exceeds u32")?;
            sections.push((format::DEBUG, scope, indexed(records)?, count));
        }
    }
    assemble(
        sections,
        artifact.header.semantic_features,
        artifact.header.entry_module,
        artifact.header.entry_function,
    )
}

fn encode_module(
    artifact: &DecodedArtifact,
    module: &DecodedModule,
) -> Result<(Vec<ModuleSection>, [u8; 32]), EncodeError> {
    let strings = module
        .strings
        .iter()
        .map(|range| range.slice(&artifact.bytes).to_vec())
        .collect::<Vec<_>>();
    let types = module.types.iter().map(encode_type).collect::<Vec<_>>();
    let constants = module
        .constants
        .iter()
        .map(encode_constant)
        .collect::<Vec<_>>();
    let imports = module.imports.iter().map(encode_import).collect::<Vec<_>>();
    let exports = module.exports.iter().map(encode_export).collect::<Vec<_>>();
    let fields = module.fields.iter().map(encode_field).collect::<Vec<_>>();
    let functions = module
        .functions
        .iter()
        .map(encode_function)
        .collect::<Vec<_>>();
    let blocks = module.blocks.iter().map(encode_block).collect::<Vec<_>>();
    let code = module
        .code
        .iter()
        .map(|record| encode_instruction_record(&record.instructions))
        .collect::<Result<Vec<_>, _>>()?;
    let exceptions = module
        .exceptions
        .iter()
        .map(encode_exception)
        .collect::<Vec<_>>();

    let records = [
        (format::STRINGS, strings),
        (format::TYPES, types),
        (format::CONSTANTS, constants),
        (format::IMPORTS, imports),
        (format::EXPORTS, exports),
        (format::FIELDS, fields),
        (format::FUNCTIONS, functions),
        (format::BLOCKS, blocks),
        (format::CODE, code),
        (format::EXCEPTIONS, exceptions),
    ];
    let mut sections = Vec::new();
    for (kind, records) in records {
        let count = to_u32(records.len(), "module record count exceeds u32")?;
        sections.push((kind, indexed(records)?, count));
    }
    let hash = semantic_hash(&sections);
    Ok((sections, hash))
}

fn encode_manifest(value: &Manifest) -> Vec<u8> {
    let mut bytes = Vec::new();
    for field in [
        value.required_heap_bytes,
        value.required_stack_bytes,
        value.maximum_coroutines,
        value.maximum_call_depth,
        value.maximum_host_requests,
        value.maximum_events,
        value.maximum_block_cost,
        value.minimum_slice_cost,
        value.required_capabilities,
        value.optional_capabilities,
    ] {
        u32le(&mut bytes, field);
    }
    bytes.extend(value.compiler_abi);
    bytes.extend(value.standard_library_abi);
    u64le(&mut bytes, 0);
    bytes
}

fn encode_capability(value: &Capability) -> Vec<u8> {
    let mut bytes = Vec::new();
    u32le(&mut bytes, value.namespace);
    u32le(&mut bytes, value.name);
    u16le(&mut bytes, value.abi_major);
    u16le(&mut bytes, value.minimum_abi_minor);
    u32le(&mut bytes, value.flags);
    u32le(&mut bytes, value.operation_count);
    u32le(&mut bytes, 0);
    bytes
}

fn encode_module_record(module: &DecodedModule, hash: [u8; 32]) -> Vec<u8> {
    let mut bytes = Vec::new();
    u32le(&mut bytes, module.name_string);
    u32le(&mut bytes, module.flags);
    bytes.extend(hash);
    for field in [
        module.declared_imports,
        module.declared_exports,
        module.declared_types,
        module.declared_functions,
        0,
    ] {
        u32le(&mut bytes, field);
    }
    bytes
}

fn encode_type(value: &NominalType) -> Vec<u8> {
    let mut bytes = Vec::new();
    match value {
        NominalType::Class {
            flags,
            generic_arity,
            name,
            super_type,
            interfaces,
            field_start,
            field_count,
            method_start,
            method_count,
        } => encode_classlike(
            &mut bytes,
            0,
            *flags,
            *generic_arity,
            *name,
            super_type.0,
            interfaces,
            *field_start,
            *field_count,
            *method_start,
            *method_count,
        ),
        NominalType::Interface {
            flags,
            generic_arity,
            name,
            super_type,
            interfaces,
            method_start,
            method_count,
        } => encode_classlike(
            &mut bytes,
            1,
            *flags,
            *generic_arity,
            *name,
            super_type.0,
            interfaces,
            0,
            0,
            *method_start,
            *method_count,
        ),
        NominalType::Array { name, element } => {
            bytes.extend([2, 0]);
            u16le(&mut bytes, 0);
            u32le(&mut bytes, *name);
            encode_value_type(&mut bytes, *element);
        }
        NominalType::Function {
            name,
            flags,
            result,
            parameters,
        } => {
            bytes.extend([3, 0]);
            u16le(&mut bytes, 0);
            u32le(&mut bytes, *name);
            u16le(&mut bytes, parameters.len() as u16);
            u16le(&mut bytes, *flags);
            encode_value_type(&mut bytes, *result);
            for parameter in parameters {
                encode_value_type(&mut bytes, *parameter);
            }
        }
    }
    bytes
}

#[allow(clippy::too_many_arguments)]
fn encode_classlike(
    bytes: &mut Vec<u8>,
    tag: u8,
    flags: u8,
    generic_arity: u16,
    name: u32,
    super_type: u32,
    interfaces: &[crate::artifact::TypeId],
    field_start: u32,
    field_count: u32,
    method_start: u32,
    method_count: u32,
) {
    bytes.extend([tag, flags]);
    u16le(bytes, generic_arity);
    u32le(bytes, name);
    u32le(bytes, super_type);
    u32le(bytes, interfaces.len() as u32);
    u32le(bytes, field_start);
    u32le(bytes, field_count);
    u32le(bytes, method_start);
    u32le(bytes, method_count);
    for interface in interfaces {
        u32le(bytes, interface.0);
    }
}

fn encode_constant(value: &Constant) -> Vec<u8> {
    let mut bytes = Vec::new();
    match value {
        Constant::I32(value) => {
            bytes.push(0);
            bytes.extend(value.to_le_bytes());
        }
        Constant::I64(value) => {
            bytes.push(1);
            bytes.extend(value.to_le_bytes());
        }
        Constant::F32(value) => {
            bytes.push(2);
            u32le(&mut bytes, *value);
        }
        Constant::F64(value) => {
            bytes.push(3);
            u64le(&mut bytes, *value);
        }
        Constant::Bool(value) => bytes.extend([4, u8::from(*value)]),
        Constant::Char(value) => {
            bytes.push(5);
            u32le(&mut bytes, *value as u32);
        }
        Constant::String(value) => {
            bytes.push(6);
            u32le(&mut bytes, *value);
        }
        Constant::Null => bytes.push(7),
    }
    bytes
}

fn encode_import(value: &Import) -> Vec<u8> {
    let mut bytes = vec![value.kind, 0, 0, 0];
    u32le(&mut bytes, value.target_module.0);
    u32le(&mut bytes, value.target_name);
    u32le(&mut bytes, value.expected_signature.0);
    bytes.extend(value.target_hash);
    bytes
}

fn encode_export(value: &Export) -> Vec<u8> {
    let mut bytes = vec![value.kind, value.visibility];
    u16le(&mut bytes, 0);
    u32le(&mut bytes, value.name);
    u32le(&mut bytes, value.local_symbol);
    u32le(&mut bytes, value.signature.0);
    bytes
}

fn encode_field(value: &Field) -> Vec<u8> {
    let mut bytes = Vec::new();
    u32le(&mut bytes, value.owner.0);
    u32le(&mut bytes, value.name);
    encode_value_type(&mut bytes, value.value_type);
    u32le(&mut bytes, value.flags);
    u32le(&mut bytes, 0);
    bytes
}

fn encode_function(value: &Function) -> Vec<u8> {
    let mut bytes = Vec::new();
    for field in [value.owner.0, value.name, value.signature.0, value.flags] {
        u32le(&mut bytes, field);
    }
    u16le(&mut bytes, value.register_count);
    u16le(&mut bytes, value.parameter_count);
    for field in [
        value.first_block.0,
        value.block_count,
        value.first_exception,
        value.exception_count,
    ] {
        u32le(&mut bytes, field);
    }
    for register in &value.registers {
        encode_value_type(&mut bytes, *register);
    }
    bytes
}

fn encode_block(value: &Block) -> Vec<u8> {
    let mut bytes = Vec::new();
    for field in [
        value.owner_function.0,
        value.code_record.0,
        value.instruction_count,
        value.declared_fixed_cost,
        value.flags,
        0,
    ] {
        u32le(&mut bytes, field);
    }
    bytes
}

fn encode_exception(value: &ExceptionEntry) -> Vec<u8> {
    let mut bytes = Vec::new();
    for field in [
        value.owner_function.0,
        value.first_protected_block.0,
        value.protected_block_count,
        value.catch_type.0,
        value.handler_block.0,
    ] {
        u32le(&mut bytes, field);
    }
    u16le(&mut bytes, value.exception_register);
    u16le(&mut bytes, 0);
    bytes
}

fn encode_debug(artifact: &DecodedArtifact, value: &DebugEntry) -> Result<Vec<u8>, EncodeError> {
    let path = value.source_path.slice(&artifact.bytes);
    encode_debug_record(value, path)
}

fn encode_debug_record(value: &DebugEntry, path: &[u8]) -> Result<Vec<u8>, EncodeError> {
    let mut bytes = Vec::new();
    for field in [
        value.function.0,
        value.block.0,
        value.instruction,
        value.start_utf16,
        value.end_utf16,
        value.inline_parent,
        to_u32(path.len(), "debug path length exceeds u32")?,
    ] {
        u32le(&mut bytes, field);
    }
    bytes.extend(path);
    Ok(bytes)
}

fn encode_value_type(bytes: &mut Vec<u8>, value: ValueType) {
    bytes.extend([value.kind, value.flags]);
    u16le(bytes, 0);
    u32le(bytes, value.nominal_type.0);
}

pub(crate) fn encode_instruction_record(values: &[Instruction]) -> Result<Vec<u8>, EncodeError> {
    let mut bytes = Vec::new();
    for value in values {
        let (opcode, form, operands) = encode_instruction(value)?;
        frame(&mut bytes, opcode, form, &operands)?;
    }
    Ok(bytes)
}

fn encode_instruction(value: &Instruction) -> Result<(u8, u8, Vec<u8>), EncodeError> {
    let mut operands = Vec::new();
    let (opcode, form) = match value {
        Instruction::Nop => (0x00, 0),
        Instruction::Move { dst, src } => {
            regs(&mut operands, &[*dst, *src]);
            (0x01, 0)
        }
        Instruction::Const { dst, constant } => {
            reg(&mut operands, *dst);
            id(&mut operands, *constant);
            (0x02, 0)
        }
        Instruction::Null { dst } => {
            reg(&mut operands, *dst);
            (0x03, 0)
        }
        Instruction::Convert { dst, src } => {
            regs(&mut operands, &[*dst, *src]);
            (0x04, 0)
        }
        Instruction::Add {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x10, *form, *dst, *lhs, *rhs),
        Instruction::Sub {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x11, *form, *dst, *lhs, *rhs),
        Instruction::Mul {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x12, *form, *dst, *lhs, *rhs),
        Instruction::Div {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x13, *form, *dst, *lhs, *rhs),
        Instruction::Rem {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x14, *form, *dst, *lhs, *rhs),
        Instruction::Neg { form, dst, src } => {
            regs(&mut operands, &[*dst, *src]);
            (0x15, *form)
        }
        Instruction::BitAnd {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x16, *form, *dst, *lhs, *rhs),
        Instruction::BitOr {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x17, *form, *dst, *lhs, *rhs),
        Instruction::BitXor {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x18, *form, *dst, *lhs, *rhs),
        Instruction::ShiftLeft {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x19, *form, *dst, *lhs, *rhs),
        Instruction::ShiftRight {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x1a, *form, *dst, *lhs, *rhs),
        Instruction::ShiftUnsigned {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x1b, *form, *dst, *lhs, *rhs),
        Instruction::Equal {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x20, *form, *dst, *lhs, *rhs),
        Instruction::NotEqual {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x21, *form, *dst, *lhs, *rhs),
        Instruction::Less {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x22, *form, *dst, *lhs, *rhs),
        Instruction::LessEqual {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x23, *form, *dst, *lhs, *rhs),
        Instruction::Greater {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x24, *form, *dst, *lhs, *rhs),
        Instruction::GreaterEqual {
            form,
            dst,
            lhs,
            rhs,
        } => arithmetic_operands(&mut operands, 0x25, *form, *dst, *lhs, *rhs),
        Instruction::RefEqual { dst, lhs, rhs } => {
            regs(&mut operands, &[*dst, *lhs, *rhs]);
            (0x26, 7)
        }
        Instruction::RefNotEqual { dst, lhs, rhs } => {
            regs(&mut operands, &[*dst, *lhs, *rhs]);
            (0x27, 7)
        }
        Instruction::NewObject { dst, type_ref } => {
            reg(&mut operands, *dst);
            id(&mut operands, *type_ref);
            (0x30, 0)
        }
        Instruction::NewArray {
            dst,
            type_ref,
            length,
        } => {
            reg(&mut operands, *dst);
            id(&mut operands, *type_ref);
            reg(&mut operands, *length);
            (0x31, 0)
        }
        Instruction::ArrayLength { dst, array } => {
            regs(&mut operands, &[*dst, *array]);
            (0x32, 0)
        }
        Instruction::ArrayLoad { dst, array, index } => {
            regs(&mut operands, &[*dst, *array, *index]);
            (0x33, 0)
        }
        Instruction::ArrayStore {
            array,
            index,
            value,
        } => {
            regs(&mut operands, &[*array, *index, *value]);
            (0x34, 0)
        }
        Instruction::FieldGet {
            dst,
            receiver,
            field_ref,
        } => {
            regs(&mut operands, &[*dst, *receiver]);
            id(&mut operands, *field_ref);
            (0x35, 0)
        }
        Instruction::FieldSet {
            receiver,
            field_ref,
            value,
        } => {
            reg(&mut operands, *receiver);
            id(&mut operands, *field_ref);
            reg(&mut operands, *value);
            (0x36, 0)
        }
        Instruction::StaticGet { dst, field_ref } => {
            reg(&mut operands, *dst);
            id(&mut operands, *field_ref);
            (0x37, 0)
        }
        Instruction::StaticSet { field_ref, value } => {
            id(&mut operands, *field_ref);
            reg(&mut operands, *value);
            (0x38, 0)
        }
        Instruction::IsType {
            dst,
            value,
            type_ref,
        } => {
            regs(&mut operands, &[*dst, *value]);
            id(&mut operands, *type_ref);
            (0x39, 0)
        }
        Instruction::CheckedCast {
            dst,
            value,
            type_ref,
        } => {
            regs(&mut operands, &[*dst, *value]);
            id(&mut operands, *type_ref);
            (0x3a, 0)
        }
        Instruction::CallDirect {
            dst,
            function_ref,
            args,
        } => {
            call_operands(&mut operands, *dst, *function_ref, args)?;
            (0x40, 0)
        }
        Instruction::CallVirtual {
            dst,
            function_ref,
            args,
        } => {
            call_operands(&mut operands, *dst, *function_ref, args)?;
            (0x41, 0)
        }
        Instruction::CallInterface {
            dst,
            function_ref,
            args,
        } => {
            call_operands(&mut operands, *dst, *function_ref, args)?;
            (0x42, 0)
        }
        Instruction::CoroutineSpawn {
            dst,
            function_ref,
            args,
        } => {
            reg(&mut operands, *dst);
            id(&mut operands, *function_ref);
            encode_args(&mut operands, args)?;
            (0x50, 0)
        }
        Instruction::CapabilityCallSync {
            dst,
            capability,
            operation,
            args,
        } => {
            capability_operands(&mut operands, *dst, *capability, *operation, args)?;
            (0x51, 0)
        }
        Instruction::Jump { target } => {
            id(&mut operands, *target);
            (0xe0, 0)
        }
        Instruction::Branch {
            condition,
            true_block,
            false_block,
        } => {
            reg(&mut operands, *condition);
            id(&mut operands, *true_block);
            id(&mut operands, *false_block);
            (0xe1, 0)
        }
        Instruction::SwitchI32 {
            key,
            default_block,
            cases,
        } => {
            reg(&mut operands, *key);
            id(&mut operands, *default_block);
            id(
                &mut operands,
                to_u32(cases.len(), "switch case count exceeds u32")?,
            );
            for case in cases {
                operands.extend(case.value.to_le_bytes());
                id(&mut operands, case.target);
            }
            (0xe2, 0)
        }
        Instruction::Return { value } => {
            reg(&mut operands, *value);
            (0xe3, 0)
        }
        Instruction::Throw { exception } => {
            reg(&mut operands, *exception);
            (0xe4, 0)
        }
        Instruction::CallSuspend {
            dst,
            function_ref,
            args,
            resume_block,
        } => {
            call_operands(&mut operands, *dst, *function_ref, args)?;
            id(&mut operands, *resume_block);
            (0xe5, 0)
        }
        Instruction::Yield { resume_block } => {
            id(&mut operands, *resume_block);
            (0xe6, 0)
        }
        Instruction::Sleep {
            duration,
            resume_block,
        } => {
            reg(&mut operands, *duration);
            id(&mut operands, *resume_block);
            (0xe7, 0)
        }
        Instruction::CoroutineJoin {
            dst,
            coroutine,
            resume_block,
        } => {
            regs(&mut operands, &[*dst, *coroutine]);
            id(&mut operands, *resume_block);
            (0xe8, 0)
        }
        Instruction::CapabilityCallAsync {
            dst,
            capability,
            operation,
            args,
            resume_block,
        } => {
            capability_operands(&mut operands, *dst, *capability, *operation, args)?;
            id(&mut operands, *resume_block);
            (0xe9, 0)
        }
        Instruction::Unreachable => (0xff, 0),
    };
    Ok((opcode, form, operands))
}

fn arithmetic_operands(
    operands: &mut Vec<u8>,
    opcode: u8,
    form: u8,
    dst: u16,
    lhs: u16,
    rhs: u16,
) -> (u8, u8) {
    regs(operands, &[dst, lhs, rhs]);
    (opcode, form)
}

fn call_operands(
    operands: &mut Vec<u8>,
    dst: u16,
    function_ref: u32,
    args: &[u16],
) -> Result<(), EncodeError> {
    reg(operands, dst);
    id(operands, function_ref);
    encode_args(operands, args)
}

fn capability_operands(
    operands: &mut Vec<u8>,
    dst: u16,
    capability: u32,
    operation: u32,
    args: &[u16],
) -> Result<(), EncodeError> {
    reg(operands, dst);
    id(operands, capability);
    id(operands, operation);
    encode_args(operands, args)
}

fn encode_args(operands: &mut Vec<u8>, args: &[u16]) -> Result<(), EncodeError> {
    id(
        operands,
        to_u32(args.len(), "instruction argument count exceeds u32")?,
    );
    regs(operands, args);
    Ok(())
}

fn regs(bytes: &mut Vec<u8>, values: &[u16]) {
    for value in values {
        reg(bytes, *value);
    }
}

fn reg(bytes: &mut Vec<u8>, value: u16) {
    u16le(bytes, value);
}

fn id(bytes: &mut Vec<u8>, mut value: u32) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn frame(bytes: &mut Vec<u8>, opcode: u8, form: u8, operands: &[u8]) -> Result<(), EncodeError> {
    let length = operands
        .len()
        .checked_add(4)
        .ok_or(EncodeError("instruction length overflows usize"))?;
    bytes.extend([opcode, form]);
    u16le(
        bytes,
        u16::try_from(length).map_err(|_| EncodeError("instruction length exceeds u16"))?,
    );
    bytes.extend(operands);
    Ok(())
}

fn indexed(records: Vec<Vec<u8>>) -> Result<Vec<u8>, EncodeError> {
    let count = to_u32(records.len(), "indexed count exceeds u32")?;
    let record_bytes = records.iter().try_fold(0_usize, |total, record| {
        total
            .checked_add(record.len())
            .ok_or(EncodeError("indexed record bytes overflow usize"))
    })?;
    let mut bytes = Vec::new();
    u32le(&mut bytes, count);
    u32le(&mut bytes, 0);
    u64le(
        &mut bytes,
        u64::try_from(record_bytes).map_err(|_| EncodeError("record bytes exceed u64"))?,
    );
    let mut offset = 0_u32;
    u32le(&mut bytes, offset);
    for record in &records {
        offset = offset
            .checked_add(to_u32(record.len(), "indexed record length exceeds u32")?)
            .ok_or(EncodeError("indexed offsets overflow u32"))?;
        u32le(&mut bytes, offset);
    }
    bytes.resize(align8(bytes.len())?, 0);
    for record in records {
        bytes.extend(record);
    }
    Ok(bytes)
}

fn semantic_hash(sections: &[ModuleSection]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"Compukter module v1\0");
    for (kind, payload, _) in sections {
        hasher.update(kind.to_le_bytes());
        hasher.update((payload.len() as u64).to_le_bytes());
        hasher.update(payload);
    }
    hasher.finalize().into()
}

fn assemble(
    sections: Vec<Section>,
    semantic_features: u32,
    entry_module: u32,
    entry_function: u32,
) -> Result<Vec<u8>, EncodeError> {
    let directory_bytes = sections
        .len()
        .checked_mul(format::DIRECTORY_ENTRY_SIZE)
        .ok_or(EncodeError("directory length overflows usize"))?;
    let first_payload = align8(
        format::HEADER_SIZE
            .checked_add(directory_bytes)
            .ok_or(EncodeError("header and directory overflow usize"))?,
    )?;
    let mut cursor = first_payload;
    let mut entries = Vec::new();
    for (kind, scope, payload, count) in &sections {
        entries.push((*kind, *scope, cursor, payload.len(), *count));
        cursor = align8(
            cursor
                .checked_add(payload.len())
                .ok_or(EncodeError("section end overflows usize"))?,
        )?;
    }
    let last = entries
        .last()
        .ok_or(EncodeError("artifact has no sections"))?;
    let payload_end = last
        .2
        .checked_add(last.3)
        .ok_or(EncodeError("payload end overflows usize"))?;

    let mut bytes = Vec::new();
    bytes.extend(b"CPKT");
    for value in [1_u16, 0, 1, 0, 64, 32] {
        u16le(&mut bytes, value);
    }
    u32le(
        &mut bytes,
        to_u32(sections.len(), "section count exceeds u32")?,
    );
    u32le(&mut bytes, semantic_features);
    u64le(&mut bytes, format::HEADER_SIZE as u64);
    u64le(
        &mut bytes,
        u64::try_from(payload_end).map_err(|_| EncodeError("payload end exceeds u64"))?,
    );
    u32le(&mut bytes, entry_module);
    u32le(&mut bytes, entry_function);
    bytes.resize(format::HEADER_SIZE, 0);
    for (kind, scope, offset, length, count) in &entries {
        u16le(&mut bytes, *kind);
        u16le(
            &mut bytes,
            if *kind == format::DEBUG {
                0
            } else {
                format::KNOWN_FLAGS
            },
        );
        u32le(&mut bytes, *scope);
        u64le(&mut bytes, *offset as u64);
        u64le(&mut bytes, *length as u64);
        u32le(&mut bytes, *count);
        u32le(&mut bytes, 0);
    }
    bytes.resize(first_payload, 0);
    for ((_, _, offset, _, _), (_, _, payload, _)) in entries.iter().zip(sections) {
        bytes.resize(*offset, 0);
        bytes.extend(payload);
    }
    bytes.resize(payload_end, 0);
    let digest = Sha256::digest(&bytes);
    bytes.extend(digest);
    Ok(bytes)
}

fn align8(value: usize) -> Result<usize, EncodeError> {
    value
        .checked_add(7)
        .map(|value| value & !7)
        .ok_or(EncodeError("alignment overflows usize"))
}

fn to_u32(value: usize, detail: &'static str) -> Result<u32, EncodeError> {
    u32::try_from(value).map_err(|_| EncodeError(detail))
}

fn u16le(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend(value.to_le_bytes());
}

fn u32le(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend(value.to_le_bytes());
}

fn u64le(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend(value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{BlockId, ByteRange, FunctionId, ModuleId, TypeId};

    fn primitive(kind: u8) -> ValueType {
        ValueType {
            kind,
            flags: 0,
            nominal_type: TypeId(u32::MAX),
        }
    }

    #[test]
    fn encodes_every_nominal_type_shape() {
        let class = NominalType::Class {
            flags: 2,
            generic_arity: 1,
            name: 3,
            super_type: TypeId(u32::MAX),
            interfaces: vec![TypeId(4)],
            field_start: 5,
            field_count: 6,
            method_start: 7,
            method_count: 8,
        };
        let interface = NominalType::Interface {
            flags: 1,
            generic_arity: 2,
            name: 9,
            super_type: TypeId(u32::MAX),
            interfaces: Vec::new(),
            method_start: 10,
            method_count: 11,
        };
        let array = NominalType::Array {
            name: 12,
            element: primitive(1),
        };
        let function = NominalType::Function {
            name: 13,
            flags: 1,
            result: primitive(0),
            parameters: vec![primitive(2)],
        };

        let encoded = [class, interface, array, function]
            .iter()
            .map(encode_type)
            .collect::<Vec<_>>();
        assert_eq!(&encoded[0][..8], &[0, 2, 1, 0, 3, 0, 0, 0]);
        assert_eq!(
            &encoded[0][8..],
            &[
                0xff, 0xff, 0xff, 0xff, 1, 0, 0, 0, 5, 0, 0, 0, 6, 0, 0, 0, 7, 0, 0, 0, 8, 0, 0, 0,
                4, 0, 0, 0,
            ]
        );
        assert_eq!(&encoded[1][..8], &[1, 1, 2, 0, 9, 0, 0, 0]);
        assert_eq!(
            &encoded[1][8..],
            &[
                0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 0, 0, 0, 11, 0, 0,
                0,
            ]
        );
        assert_eq!(
            encoded[2],
            [2, 0, 0, 0, 12, 0, 0, 0, 1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff]
        );
        assert_eq!(
            encoded[3],
            [
                3, 0, 0, 0, 13, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 2, 0, 0,
                0, 0xff, 0xff, 0xff, 0xff,
            ]
        );
    }

    #[test]
    fn encodes_every_constant_shape() {
        let values = [
            Constant::I32(-1),
            Constant::I64(-2),
            Constant::F32(0x3f80_0000),
            Constant::F64(0x3ff0_0000_0000_0000),
            Constant::Bool(true),
            Constant::Char('x'),
            Constant::String(0),
            Constant::Null,
        ];
        let encoded = values.iter().map(encode_constant).collect::<Vec<_>>();
        assert_eq!(encoded[0], [0, 0xff, 0xff, 0xff, 0xff]);
        assert_eq!(
            encoded[1],
            [1, 0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
        );
        assert_eq!(encoded[2], [2, 0, 0, 0x80, 0x3f]);
        assert_eq!(encoded[3], [3, 0, 0, 0, 0, 0, 0, 0xf0, 0x3f]);
        assert_eq!(encoded[4], [4, 1]);
        assert_eq!(encoded[5], [5, b'x', 0, 0, 0]);
        assert_eq!(encoded[6], [6, 0, 0, 0, 0]);
        assert_eq!(encoded[7], [7]);
    }

    #[test]
    fn encodes_every_fixed_record_shape() {
        let capability = Capability {
            namespace: 1,
            name: 2,
            abi_major: 3,
            minimum_abi_minor: 4,
            flags: 1,
            operation_count: 5,
        };
        let import = Import {
            kind: 1,
            target_module: ModuleId(2),
            target_name: 3,
            expected_signature: TypeId(4),
            target_hash: [5; 32],
        };
        let export = Export {
            kind: 1,
            visibility: 1,
            name: 2,
            local_symbol: 3,
            signature: TypeId(4),
        };
        let field = Field {
            owner: TypeId(1),
            name: 2,
            value_type: primitive(1),
            flags: 3,
        };
        let function = Function {
            owner: TypeId(u32::MAX),
            name: 1,
            signature: TypeId(2),
            flags: 2,
            register_count: 1,
            parameter_count: 1,
            first_block: BlockId(3),
            block_count: 4,
            first_exception: 5,
            exception_count: 6,
            registers: vec![primitive(1)],
        };
        let block = Block {
            owner_function: FunctionId(1),
            code_record: BlockId(2),
            instruction_count: 3,
            declared_fixed_cost: 4,
            flags: 1,
        };
        let exception = ExceptionEntry {
            owner_function: FunctionId(1),
            first_protected_block: BlockId(2),
            protected_block_count: 3,
            catch_type: TypeId(4),
            handler_block: BlockId(5),
            exception_register: 6,
        };
        let debug = DebugEntry {
            function: FunctionId(1),
            block: BlockId(2),
            instruction: 3,
            start_utf16: 4,
            end_utf16: 5,
            inline_parent: u32::MAX,
            source_path: ByteRange { start: 0, end: 0 },
        };

        assert_eq!(encode_capability(&capability).len(), 24);
        assert_eq!(
            &encode_capability(&capability)[..12],
            &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 4, 0]
        );
        assert_eq!(encode_import(&import).len(), 48);
        assert_eq!(
            &encode_import(&import)[..16],
            &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0]
        );
        assert_eq!(
            encode_export(&export),
            [1, 1, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0]
        );
        assert_eq!(encode_field(&field).len(), 24);
        assert_eq!(encode_function(&function).len(), 44);
        assert_eq!(
            encode_block(&block),
            [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            encode_exception(&exception),
            [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 5, 0, 0, 0, 6, 0, 0, 0]
        );
        assert_eq!(
            encode_debug_record(&debug, b"a.kt").unwrap(),
            [
                1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 5, 0, 0, 0, 0xff, 0xff, 0xff, 0xff,
                4, 0, 0, 0, b'a', b'.', b'k', b't'
            ]
        );
    }
}
