use crate::{
    artifact::{format, Instruction, SwitchCase},
    bytes::Cursor,
    diagnostic::{Code, Diagnostic, DiagnosticSet, Family},
    limits::ArtifactLimits,
};

pub(crate) fn decode_code_record(
    bytes: &[u8],
    expected_count: u32,
    limits: &ArtifactLimits,
) -> Result<Box<[Instruction]>, DiagnosticSet> {
    let expected = usize::try_from(expected_count).map_err(|_| {
        error(
            Code::LimitExceeded,
            0,
            "instruction count does not fit usize",
        )
    })?;
    if expected > limits.records_per_section {
        return Err(error(
            Code::LimitExceeded,
            0,
            "instruction count limit exceeded",
        ));
    }
    if expected > bytes.len() / 4 {
        return Err(error(
            Code::BadInstruction,
            0,
            "declared instruction count cannot fit code bytes",
        ));
    }
    let mut instructions = Vec::new();
    instructions
        .try_reserve_exact(expected)
        .map_err(|_| error(Code::LimitExceeded, 0, "cannot reserve instructions"))?;
    let mut cursor = Cursor::new(bytes);
    let mut saw_terminator = false;
    while cursor.position() < bytes.len() {
        let offset = cursor.position();
        if saw_terminator {
            return Err(error(
                Code::BadInstruction,
                offset,
                "instruction follows a terminator",
            ));
        }
        let opcode = read_u8(&mut cursor, offset)?;
        let form = read_u8(&mut cursor, offset)?;
        let length = read_u16(&mut cursor, offset)? as usize;
        if length < 4 {
            return Err(error(
                Code::BadInstruction,
                offset,
                "instruction length is below four",
            ));
        }
        let operands = cursor.take(length - 4).map_err(|_| {
            error(
                Code::BadInstruction,
                offset,
                "instruction overruns code record",
            )
        })?;
        let instruction = decode_instruction(opcode, form, operands, limits, offset)?;
        saw_terminator = instruction.is_terminator();
        instructions.push(instruction);
        if instructions.len() > expected {
            return Err(error(
                Code::BadInstruction,
                offset,
                "instruction count exceeds block declaration",
            ));
        }
    }
    if instructions.len() != expected || !saw_terminator {
        return Err(error(
            Code::BadInstruction,
            bytes.len(),
            "instruction count or final terminator is invalid",
        ));
    }
    Ok(instructions.into_boxed_slice())
}

fn decode_instruction(
    opcode: u8,
    form: u8,
    operands: &[u8],
    limits: &ArtifactLimits,
    offset: usize,
) -> Result<Instruction, DiagnosticSet> {
    validate_form(opcode, form, offset)?;
    let mut cursor = Cursor::new(operands);
    let instruction = match opcode {
        0x00 => Instruction::Nop,
        0x01 => Instruction::Move {
            dst: reg(&mut cursor, offset)?,
            src: reg(&mut cursor, offset)?,
        },
        0x02 => Instruction::Const {
            dst: reg(&mut cursor, offset)?,
            constant: id(&mut cursor, offset)?,
        },
        0x03 => Instruction::Null {
            dst: reg(&mut cursor, offset)?,
        },
        0x04 => Instruction::Convert {
            dst: reg(&mut cursor, offset)?,
            src: reg(&mut cursor, offset)?,
        },
        0x10 => arithmetic(form, &mut cursor, offset, Arithmetic::Add)?,
        0x11 => arithmetic(form, &mut cursor, offset, Arithmetic::Sub)?,
        0x12 => arithmetic(form, &mut cursor, offset, Arithmetic::Mul)?,
        0x13 => arithmetic(form, &mut cursor, offset, Arithmetic::Div)?,
        0x14 => arithmetic(form, &mut cursor, offset, Arithmetic::Rem)?,
        0x15 => Instruction::Neg {
            form,
            dst: reg(&mut cursor, offset)?,
            src: reg(&mut cursor, offset)?,
        },
        0x16 => arithmetic(form, &mut cursor, offset, Arithmetic::BitAnd)?,
        0x17 => arithmetic(form, &mut cursor, offset, Arithmetic::BitOr)?,
        0x18 => arithmetic(form, &mut cursor, offset, Arithmetic::BitXor)?,
        0x19 => arithmetic(form, &mut cursor, offset, Arithmetic::ShiftLeft)?,
        0x1a => arithmetic(form, &mut cursor, offset, Arithmetic::ShiftRight)?,
        0x1b => arithmetic(form, &mut cursor, offset, Arithmetic::ShiftUnsigned)?,
        0x20 => comparison(form, &mut cursor, offset, Comparison::Equal)?,
        0x21 => comparison(form, &mut cursor, offset, Comparison::NotEqual)?,
        0x22 => comparison(form, &mut cursor, offset, Comparison::Less)?,
        0x23 => comparison(form, &mut cursor, offset, Comparison::LessEqual)?,
        0x24 => comparison(form, &mut cursor, offset, Comparison::Greater)?,
        0x25 => comparison(form, &mut cursor, offset, Comparison::GreaterEqual)?,
        0x26 => {
            let (dst, lhs, rhs) = regs3(&mut cursor, offset)?;
            Instruction::RefEqual { dst, lhs, rhs }
        }
        0x27 => {
            let (dst, lhs, rhs) = regs3(&mut cursor, offset)?;
            Instruction::RefNotEqual { dst, lhs, rhs }
        }
        0x30 => Instruction::NewObject {
            dst: reg(&mut cursor, offset)?,
            type_ref: id(&mut cursor, offset)?,
        },
        0x31 => Instruction::NewArray {
            dst: reg(&mut cursor, offset)?,
            type_ref: id(&mut cursor, offset)?,
            length: reg(&mut cursor, offset)?,
        },
        0x32 => Instruction::ArrayLength {
            dst: reg(&mut cursor, offset)?,
            array: reg(&mut cursor, offset)?,
        },
        0x33 => {
            let (dst, array, index) = regs3(&mut cursor, offset)?;
            Instruction::ArrayLoad { dst, array, index }
        }
        0x34 => {
            let (array, index, value) = regs3(&mut cursor, offset)?;
            Instruction::ArrayStore {
                array,
                index,
                value,
            }
        }
        0x35 => Instruction::FieldGet {
            dst: reg(&mut cursor, offset)?,
            receiver: reg(&mut cursor, offset)?,
            field_ref: id(&mut cursor, offset)?,
        },
        0x36 => Instruction::FieldSet {
            receiver: reg(&mut cursor, offset)?,
            field_ref: id(&mut cursor, offset)?,
            value: reg(&mut cursor, offset)?,
        },
        0x37 => Instruction::StaticGet {
            dst: reg(&mut cursor, offset)?,
            field_ref: id(&mut cursor, offset)?,
        },
        0x38 => Instruction::StaticSet {
            field_ref: id(&mut cursor, offset)?,
            value: reg(&mut cursor, offset)?,
        },
        0x39 => Instruction::IsType {
            dst: reg(&mut cursor, offset)?,
            value: reg(&mut cursor, offset)?,
            type_ref: id(&mut cursor, offset)?,
        },
        0x3a => Instruction::CheckedCast {
            dst: reg(&mut cursor, offset)?,
            value: reg(&mut cursor, offset)?,
            type_ref: id(&mut cursor, offset)?,
        },
        0x40 => {
            let (dst, function_ref, args) = call(&mut cursor, limits, offset)?;
            Instruction::CallDirect {
                dst,
                function_ref,
                args,
            }
        }
        0x41 => {
            let (dst, function_ref, args) = call(&mut cursor, limits, offset)?;
            Instruction::CallVirtual {
                dst,
                function_ref,
                args,
            }
        }
        0x42 => {
            let (dst, function_ref, args) = call(&mut cursor, limits, offset)?;
            Instruction::CallInterface {
                dst,
                function_ref,
                args,
            }
        }
        0x50 => {
            let dst = reg(&mut cursor, offset)?;
            let function_ref = id(&mut cursor, offset)?;
            let args = args(&mut cursor, limits, offset)?;
            Instruction::CoroutineSpawn {
                dst,
                function_ref,
                args,
            }
        }
        0x51 => {
            let dst = optional_reg(&mut cursor, offset)?;
            let capability = id(&mut cursor, offset)?;
            let operation = id(&mut cursor, offset)?;
            let args = args(&mut cursor, limits, offset)?;
            Instruction::CapabilityCallSync {
                dst,
                capability,
                operation,
                args,
            }
        }
        0x60 => Instruction::StringLength {
            dst: reg(&mut cursor, offset)?,
            string: reg(&mut cursor, offset)?,
        },
        0x61 => {
            let (dst, string, index) = regs3(&mut cursor, offset)?;
            Instruction::StringGet { dst, string, index }
        }
        0x62 => {
            let (dst, lhs, rhs) = regs3(&mut cursor, offset)?;
            Instruction::StringEquals { dst, lhs, rhs }
        }
        0x63 => {
            let (dst, lhs, rhs) = regs3(&mut cursor, offset)?;
            Instruction::StringCompare { dst, lhs, rhs }
        }
        0x64 => Instruction::StringHash {
            dst: reg(&mut cursor, offset)?,
            string: reg(&mut cursor, offset)?,
        },
        0x65 => {
            let (dst, lhs, rhs) = regs3(&mut cursor, offset)?;
            Instruction::StringConcat { dst, lhs, rhs }
        }
        0x68 => Instruction::StringValueOf {
            form,
            dst: reg(&mut cursor, offset)?,
            source: reg(&mut cursor, offset)?,
        },
        0x66 => {
            let (dst, string, start, end) = regs4(&mut cursor, offset)?;
            Instruction::StringSubstring {
                dst,
                string,
                start,
                end,
            }
        }
        0x67 => {
            let (dst, array, start, end) = regs4(&mut cursor, offset)?;
            Instruction::StringFromCharArray {
                dst,
                array,
                start,
                end,
            }
        }
        0xe0 => Instruction::Jump {
            target: id(&mut cursor, offset)?,
        },
        0xe1 => Instruction::Branch {
            condition: reg(&mut cursor, offset)?,
            true_block: id(&mut cursor, offset)?,
            false_block: id(&mut cursor, offset)?,
        },
        0xe2 => {
            let key = reg(&mut cursor, offset)?;
            let default_block = id(&mut cursor, offset)?;
            let count = list_count(&mut cursor, limits, offset)?;
            let mut cases = Vec::new();
            cases
                .try_reserve_exact(count)
                .map_err(|_| error(Code::LimitExceeded, offset, "cannot reserve switch cases"))?;
            for _ in 0..count {
                let value = cursor
                    .read_i32()
                    .map_err(|diagnostic| remap(diagnostic, offset))?;
                let target = id(&mut cursor, offset)?;
                if cases
                    .last()
                    .is_some_and(|previous: &SwitchCase| previous.value >= value)
                {
                    return Err(error(
                        Code::BadInstruction,
                        offset,
                        "switch cases are not sorted and unique",
                    ));
                }
                cases.push(SwitchCase { value, target });
            }
            Instruction::SwitchI32 {
                key,
                default_block,
                cases: cases.into_boxed_slice(),
            }
        }
        0xe3 => Instruction::Return {
            value: optional_reg(&mut cursor, offset)?,
        },
        0xe4 => Instruction::Throw {
            exception: reg(&mut cursor, offset)?,
        },
        0xe5 => {
            let (dst, function_ref, args) = call(&mut cursor, limits, offset)?;
            let resume_block = id(&mut cursor, offset)?;
            Instruction::CallSuspend {
                dst,
                function_ref,
                args,
                resume_block,
            }
        }
        0xe6 => Instruction::Yield {
            resume_block: id(&mut cursor, offset)?,
        },
        0xe7 => Instruction::Sleep {
            duration: reg(&mut cursor, offset)?,
            resume_block: id(&mut cursor, offset)?,
        },
        0xe8 => Instruction::CoroutineJoin {
            dst: optional_reg(&mut cursor, offset)?,
            coroutine: reg(&mut cursor, offset)?,
            resume_block: id(&mut cursor, offset)?,
        },
        0xe9 => {
            let dst = optional_reg(&mut cursor, offset)?;
            let capability = id(&mut cursor, offset)?;
            let operation = id(&mut cursor, offset)?;
            let args = args(&mut cursor, limits, offset)?;
            let resume_block = id(&mut cursor, offset)?;
            Instruction::CapabilityCallAsync {
                dst,
                capability,
                operation,
                args,
                resume_block,
            }
        }
        0xff => Instruction::Unreachable,
        _ => return Err(error(Code::BadInstruction, offset, "unknown opcode")),
    };
    if cursor.position() != operands.len() {
        return Err(error(
            Code::BadInstruction,
            offset,
            "instruction operand length is not canonical",
        ));
    }
    Ok(instruction)
}

#[derive(Clone, Copy)]
enum Arithmetic {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    ShiftLeft,
    ShiftRight,
    ShiftUnsigned,
}

fn arithmetic(
    form: u8,
    cursor: &mut Cursor<'_>,
    offset: usize,
    operation: Arithmetic,
) -> Result<Instruction, DiagnosticSet> {
    let (dst, lhs, rhs) = regs3(cursor, offset)?;
    Ok(match operation {
        Arithmetic::Add => Instruction::Add {
            form,
            dst,
            lhs,
            rhs,
        },
        Arithmetic::Sub => Instruction::Sub {
            form,
            dst,
            lhs,
            rhs,
        },
        Arithmetic::Mul => Instruction::Mul {
            form,
            dst,
            lhs,
            rhs,
        },
        Arithmetic::Div => Instruction::Div {
            form,
            dst,
            lhs,
            rhs,
        },
        Arithmetic::Rem => Instruction::Rem {
            form,
            dst,
            lhs,
            rhs,
        },
        Arithmetic::BitAnd => Instruction::BitAnd {
            form,
            dst,
            lhs,
            rhs,
        },
        Arithmetic::BitOr => Instruction::BitOr {
            form,
            dst,
            lhs,
            rhs,
        },
        Arithmetic::BitXor => Instruction::BitXor {
            form,
            dst,
            lhs,
            rhs,
        },
        Arithmetic::ShiftLeft => Instruction::ShiftLeft {
            form,
            dst,
            lhs,
            rhs,
        },
        Arithmetic::ShiftRight => Instruction::ShiftRight {
            form,
            dst,
            lhs,
            rhs,
        },
        Arithmetic::ShiftUnsigned => Instruction::ShiftUnsigned {
            form,
            dst,
            lhs,
            rhs,
        },
    })
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

fn comparison(
    form: u8,
    cursor: &mut Cursor<'_>,
    offset: usize,
    operation: Comparison,
) -> Result<Instruction, DiagnosticSet> {
    let (dst, lhs, rhs) = regs3(cursor, offset)?;
    Ok(match operation {
        Comparison::Equal => Instruction::Equal {
            form,
            dst,
            lhs,
            rhs,
        },
        Comparison::NotEqual => Instruction::NotEqual {
            form,
            dst,
            lhs,
            rhs,
        },
        Comparison::Less => Instruction::Less {
            form,
            dst,
            lhs,
            rhs,
        },
        Comparison::LessEqual => Instruction::LessEqual {
            form,
            dst,
            lhs,
            rhs,
        },
        Comparison::Greater => Instruction::Greater {
            form,
            dst,
            lhs,
            rhs,
        },
        Comparison::GreaterEqual => Instruction::GreaterEqual {
            form,
            dst,
            lhs,
            rhs,
        },
    })
}

fn validate_form(opcode: u8, form: u8, offset: usize) -> Result<(), DiagnosticSet> {
    if format::valid_instruction_form(opcode, form) {
        Ok(())
    } else {
        Err(error(
            Code::BadInstruction,
            offset,
            "opcode form is invalid",
        ))
    }
}

fn call(
    cursor: &mut Cursor<'_>,
    limits: &ArtifactLimits,
    offset: usize,
) -> Result<(u16, u32, Box<[u16]>), DiagnosticSet> {
    Ok((
        optional_reg(cursor, offset)?,
        id(cursor, offset)?,
        args(cursor, limits, offset)?,
    ))
}

fn args(
    cursor: &mut Cursor<'_>,
    limits: &ArtifactLimits,
    offset: usize,
) -> Result<Box<[u16]>, DiagnosticSet> {
    let count = list_count(cursor, limits, offset)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| error(Code::LimitExceeded, offset, "cannot reserve arguments"))?;
    for _ in 0..count {
        values.push(reg(cursor, offset)?);
    }
    Ok(values.into_boxed_slice())
}

fn list_count(
    cursor: &mut Cursor<'_>,
    limits: &ArtifactLimits,
    offset: usize,
) -> Result<usize, DiagnosticSet> {
    let count = id(cursor, offset)? as usize;
    if count > limits.registers_per_function {
        Err(error(
            Code::LimitExceeded,
            offset,
            "instruction list limit exceeded",
        ))
    } else {
        Ok(count)
    }
}

fn regs3(cursor: &mut Cursor<'_>, offset: usize) -> Result<(u16, u16, u16), DiagnosticSet> {
    Ok((
        reg(cursor, offset)?,
        reg(cursor, offset)?,
        reg(cursor, offset)?,
    ))
}

fn regs4(cursor: &mut Cursor<'_>, offset: usize) -> Result<(u16, u16, u16, u16), DiagnosticSet> {
    Ok((
        reg(cursor, offset)?,
        reg(cursor, offset)?,
        reg(cursor, offset)?,
        reg(cursor, offset)?,
    ))
}

fn reg(cursor: &mut Cursor<'_>, offset: usize) -> Result<u16, DiagnosticSet> {
    let value = optional_reg(cursor, offset)?;
    if value == u16::MAX {
        Err(error(
            Code::BadInstruction,
            offset,
            "absent register sentinel is not allowed for this operand",
        ))
    } else {
        Ok(value)
    }
}

fn optional_reg(cursor: &mut Cursor<'_>, offset: usize) -> Result<u16, DiagnosticSet> {
    cursor
        .read_u16()
        .map_err(|diagnostic| remap(diagnostic, offset))
}

fn id(cursor: &mut Cursor<'_>, offset: usize) -> Result<u32, DiagnosticSet> {
    cursor
        .read_uleb32()
        .map_err(|diagnostic| remap(diagnostic, offset))
}

fn read_u8(cursor: &mut Cursor<'_>, offset: usize) -> Result<u8, DiagnosticSet> {
    cursor
        .read_u8()
        .map_err(|diagnostic| remap(diagnostic, offset))
}

fn read_u16(cursor: &mut Cursor<'_>, offset: usize) -> Result<u16, DiagnosticSet> {
    cursor
        .read_u16()
        .map_err(|diagnostic| remap(diagnostic, offset))
}

fn remap(mut diagnostic: Diagnostic, base: usize) -> DiagnosticSet {
    diagnostic.family = Family::Code;
    diagnostic.location.offset = diagnostic
        .location
        .offset
        .and_then(|value| value.checked_add(base as u64));
    let mut errors = DiagnosticSet::new(1);
    errors.push(diagnostic);
    errors
}

fn error(code: Code, offset: usize, detail: &'static str) -> DiagnosticSet {
    let mut errors = DiagnosticSet::new(1);
    errors.push(Diagnostic::at_offset(Family::Code, code, offset, detail));
    errors
}

impl Instruction {
    pub(crate) fn is_terminator(&self) -> bool {
        matches!(
            self,
            Self::Jump { .. }
                | Self::Branch { .. }
                | Self::SwitchI32 { .. }
                | Self::Return { .. }
                | Self::Throw { .. }
                | Self::CallSuspend { .. }
                | Self::Yield { .. }
                | Self::Sleep { .. }
                | Self::CoroutineJoin { .. }
                | Self::CapabilityCallAsync { .. }
                | Self::Unreachable
        )
    }

    pub(crate) fn fixed_cost(&self) -> Result<u32, Diagnostic> {
        let fixed = match self {
            Self::Mul { .. }
            | Self::Convert { .. }
            | Self::ArrayLength { .. }
            | Self::ArrayLoad { .. }
            | Self::ArrayStore { .. }
            | Self::FieldGet { .. }
            | Self::FieldSet { .. }
            | Self::StaticGet { .. }
            | Self::StaticSet { .. }
            | Self::IsType { .. }
            | Self::CheckedCast { .. }
            | Self::Throw { .. }
            | Self::Yield { .. } => 2,
            Self::Div { .. }
            | Self::Rem { .. }
            | Self::NewObject { .. }
            | Self::NewArray { .. }
            | Self::CoroutineJoin { .. } => 4,
            Self::Sleep { .. } => 3,
            Self::CallDirect { args, .. } => variable_cost(4, args.len())?,
            Self::CallVirtual { args, .. } | Self::CallSuspend { args, .. } => {
                variable_cost(5, args.len())?
            }
            Self::CallInterface { args, .. }
            | Self::CoroutineSpawn { args, .. }
            | Self::CapabilityCallAsync { args, .. } => variable_cost(6, args.len())?,
            Self::CapabilityCallSync { args, .. } => variable_cost(5, args.len())?,
            Self::SwitchI32 { cases, .. } => variable_cost(1, cases.len())?,
            _ => 1,
        };
        Ok(fixed)
    }
}

fn variable_cost(base: u32, count: usize) -> Result<u32, Diagnostic> {
    u32::try_from(count)
        .ok()
        .and_then(|count| base.checked_add(count))
        .ok_or_else(|| {
            Diagnostic::at_offset(Family::Cost, Code::BadCost, 0, "fixed cost overflows u32")
        })
}
