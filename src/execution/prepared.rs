/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 * You may obtain a copy at https://www.apache.org/licenses/LICENSE-2.0
 */

//! Private, verified execution plans. One operation retains one logical PC.
use super::{
    error::{AdmissionError, VmFault},
    frame::PreparedFrame,
    image::{ResolvedField, ResolvedFunction, ResolvedInstruction},
    machine::InstructionFailure,
    numeric,
    value::RuntimeValue,
};
use crate::artifact::PhysicalAtom;

// Zero denotes a lifecycle operation in the sparse descriptor table.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub(super) struct Operation {
    pub opcode: u32,
    pub dst: u32,
    pub lhs: u32,
    pub rhs: u32,
}

#[derive(Debug)]
#[repr(C, align(64))]
struct Line([Operation; 4]);

pub(super) const COPY32: u32 = 80;
pub(super) const COPY64: u32 = 81;
pub(super) const IMMEDIATE32: u32 = 82;
pub(super) const IMMEDIATE64: u32 = 83;
pub(super) const COPIES: u32 = 84;
pub(super) const NOP: u32 = 85;
pub(super) const JUMP: u32 = 86;
pub(super) const BRANCH: u32 = 87;
pub(super) const COPY_BOOL: u32 = 88;
pub(super) const COPY_CHAR: u32 = 89;
pub(super) const FIELD_GET: u32 = 200;
pub(super) const FIELD_SET: u32 = 208;
pub(super) const ARRAY_LOAD: u32 = 216;
pub(super) const ARRAY_STORE: u32 = 224;
pub(super) const ARRAY_LENGTH: u32 = 232;
pub(super) const NON_NULL: u32 = 1 << 16;

#[derive(Debug)]
pub(super) struct Copy {
    pub dst: u32,
    pub src: u32,
    pub width: u32,
}

// Physical component transfers are immutable and shared with the program.
#[derive(Clone, Copy, Debug)]
pub(super) struct ArgumentTransfer {
    pub src: u32,
    pub dst: u32,
    pub kind: u8,
    pub width: u8,
}

#[derive(Debug, Default)]
struct Plans {
    copies: Vec<Copy>,
    fields: Vec<ResolvedField>,
}

#[derive(Debug)]
pub(super) struct PreparedCode {
    #[cfg(feature = "baseline-jit")]
    pub(super) jit: std::sync::OnceLock<std::sync::Arc<super::jit::JitState>>,
    lines: Box<[Line]>,
    len: usize,
    cold: Box<[ResolvedInstruction]>,
    pub copies: Box<[Copy]>,
    pub fields: Box<[ResolvedField]>,
    pub arguments: Box<[ArgumentTransfer]>,
    #[cfg(any(test, feature = "execution-prototype"))]
    reference: Box<[ResolvedInstruction]>,
}

#[derive(Default)]
pub(super) struct CodeBuilder {
    operations: Vec<Operation>,
    cold: Vec<ResolvedInstruction>,
    plans: Plans,
    arguments: Vec<ArgumentTransfer>,
    #[cfg(any(test, feature = "execution-prototype"))]
    reference: Vec<ResolvedInstruction>,
}

impl CodeBuilder {
    pub(super) fn push_block(
        &mut self,
        instructions: Vec<ResolvedInstruction>,
        function: &ResolvedFunction,
        functions: &[ResolvedFunction],
        constants: &[RuntimeValue],
    ) -> Result<usize, AdmissionError> {
        let start = self.operations.len();
        let allocation = |_| AdmissionError::AllocationFailed;
        self.operations
            .try_reserve(instructions.len())
            .map_err(allocation)?;
        self.cold
            .try_reserve(instructions.len())
            .map_err(allocation)?;
        #[cfg(any(test, feature = "execution-prototype"))]
        {
            self.reference
                .try_reserve(instructions.len())
                .map_err(allocation)?;
            self.reference.extend(instructions.iter().cloned());
        }
        for instruction in instructions {
            let operation = match prepare_numeric(&instruction, function) {
                Some(operation) => Some(operation),
                None => prepare_extra(&instruction, function, constants, &mut self.plans)?,
            };
            let operation = match operation {
                Some(operation) => operation,
                None => {
                    let descriptor = u32::try_from(self.cold.len())
                        .map_err(|_| AdmissionError::StoragePlanOverflow)?;
                    let instruction = self.prepare_lifecycle(instruction, function, functions)?;
                    self.cold.push(instruction);
                    Operation {
                        dst: descriptor,
                        ..Operation::default()
                    }
                }
            };
            self.operations.push(operation);
        }
        Ok(start)
    }

    fn prepare_lifecycle(
        &mut self,
        instruction: ResolvedInstruction,
        caller: &ResolvedFunction,
        functions: &[ResolvedFunction],
    ) -> Result<ResolvedInstruction, AdmissionError> {
        use ResolvedInstruction::*;
        let (dst, target, args, resume_block) = match instruction {
            CallDirect { dst, target, args } => (dst, target, args, usize::MAX),
            CallSuspend {
                dst,
                target,
                args,
                resume_block,
            } => (dst, target, args, resume_block),
            Return { value } if value != u16::MAX && caller.result.kind != 8 => {
                let access = *caller
                    .register_accesses
                    .get(value as usize)
                    .ok_or(AdmissionError::InvalidEntry)?;
                return Ok(PreparedReturn { value, access });
            }
            other => return Ok(other),
        };
        let callee = functions.get(target).ok_or(AdmissionError::InvalidEntry)?;
        if callee.parameter_count != args.len() {
            return Err(AdmissionError::InvalidEntry);
        }
        let start =
            u32::try_from(self.arguments.len()).map_err(|_| AdmissionError::StoragePlanOverflow)?;
        for (parameter, source) in args.iter().enumerate() {
            let source_index = *source as usize;
            let kind = caller
                .registers
                .get(source_index)
                .ok_or(AdmissionError::InvalidEntry)?
                .kind;
            let source = caller
                .frame_layout
                .values
                .get(source_index)
                .ok_or(AdmissionError::InvalidEntry)?;
            let destination = callee
                .frame_layout
                .values
                .get(parameter)
                .ok_or(AdmissionError::InvalidEntry)?;
            if source.components.len() != destination.components.len() {
                return Err(AdmissionError::InvalidEntry);
            }
            self.arguments
                .try_reserve(source.components.len())
                .map_err(|_| AdmissionError::AllocationFailed)?;
            let argument_start = self.arguments.len();
            for (source, destination) in source.components.iter().zip(destination.components.iter())
            {
                if source.atom != destination.atom {
                    return Err(AdmissionError::InvalidEntry);
                }
                let width = source.atom.byte_size() as u8;
                // Call frames occupy disjoint reservations. Consecutive inline
                // components can therefore transfer as one checked byte span.
                if kind == 8 && self.arguments.len() > argument_start {
                    let previous = self
                        .arguments
                        .last_mut()
                        .ok_or(AdmissionError::InvalidEntry)?;
                    if previous.src.checked_add(u32::from(previous.width)) == Some(source.offset)
                        && previous.dst.checked_add(u32::from(previous.width))
                            == Some(destination.offset)
                    {
                        if let Some(width) = previous.width.checked_add(width) {
                            previous.width = width;
                            continue;
                        }
                    }
                }
                self.arguments.push(ArgumentTransfer {
                    src: source.offset,
                    dst: destination.offset,
                    kind,
                    width,
                });
            }
        }
        let end =
            u32::try_from(self.arguments.len()).map_err(|_| AdmissionError::StoragePlanOverflow)?;
        Ok(PreparedCall {
            dst,
            target,
            transfers_start: start,
            transfers_count: end - start,
            resume_block,
        })
    }

    pub(super) fn finish(self) -> Result<PreparedCode, AdmissionError> {
        let len = self.operations.len();
        let mut lines = Vec::new();
        lines
            .try_reserve_exact(len.div_ceil(4))
            .map_err(|_| AdmissionError::AllocationFailed)?;
        for operations in self.operations.chunks(4) {
            let mut line = Line([Operation::default(); 4]);
            line.0[..operations.len()].copy_from_slice(operations);
            lines.push(line);
        }
        Ok(PreparedCode {
            #[cfg(feature = "baseline-jit")]
            jit: std::sync::OnceLock::new(),
            lines: lines.into_boxed_slice(),
            len,
            cold: self.cold.into_boxed_slice(),
            copies: self.plans.copies.into_boxed_slice(),
            fields: self.plans.fields.into_boxed_slice(),
            arguments: self.arguments.into_boxed_slice(),
            #[cfg(any(test, feature = "execution-prototype"))]
            reference: self.reference.into_boxed_slice(),
        })
    }
}

impl PreparedCode {
    /// Counts slots/plans, excluding variable payloads of lifecycle descriptors.
    #[cfg(feature = "guest-benchmark")]
    pub(super) fn statistics(&self) -> [usize; 6] {
        [
            self.len,
            self.len - self.cold.len(),
            self.cold.len(),
            self.lines.len() * core::mem::size_of::<Line>(),
            core::mem::size_of_val(self.cold.as_ref()),
            core::mem::size_of_val(self.copies.as_ref())
                + core::mem::size_of_val(self.fields.as_ref())
                + core::mem::size_of_val(self.arguments.as_ref()),
        ]
    }

    // Keep fetch borrowed so opcode dispatch need not materialize the full operation.
    #[inline(always)]
    pub(super) fn operation(&self, index: usize) -> Option<&Operation> {
        (index < self.len).then(|| &self.lines[index / 4].0[index % 4])
    }

    pub(super) fn get(&self, index: usize) -> Option<&ResolvedInstruction> {
        let operation = self.operation(index)?;
        (operation.opcode == 0)
            .then(|| self.cold.get(operation.dst as usize))
            .flatten()
    }

    #[inline]
    pub(super) fn get_for_execution(
        &self,
        index: usize,
        reference: bool,
    ) -> Option<&ResolvedInstruction> {
        #[cfg(any(test, feature = "execution-prototype"))]
        if reference {
            return self.reference.get(index);
        }
        let _ = reference;
        self.get(index)
    }

    #[cfg(feature = "execution-prototype")]
    pub(super) fn reference(&self, start: usize, len: usize) -> &[ResolvedInstruction] {
        &self.reference[start..start + len]
    }
}

fn offset(function: &ResolvedFunction, register: u32, kind: u8) -> Option<u32> {
    if function.registers.get(register as usize)?.kind != kind {
        return None;
    }
    let layout = function.frame_layout.values.get(register as usize)?;
    let [component] = layout.components.as_ref() else {
        return None;
    };
    let atom = match kind {
        1 | 5 | 6 => PhysicalAtom::I32,
        7 => PhysicalAtom::Ref32,
        2 => PhysicalAtom::I64,
        3 => PhysicalAtom::F32,
        4 => PhysicalAtom::F64,
        _ => return None,
    };
    if component.atom != atom
        || component.offset.checked_add(atom.byte_size())? > function.frame_layout.byte_len
    {
        return None;
    }
    Some(component.offset)
}

fn prepare_numeric(
    instruction: &ResolvedInstruction,
    function: &ResolvedFunction,
) -> Option<Operation> {
    use ResolvedInstruction::*;
    let (group, form, dst, lhs, rhs) = match instruction {
        Add {
            form,
            dst,
            lhs,
            rhs,
        } => (0, *form, *dst, *lhs, *rhs),
        Sub {
            form,
            dst,
            lhs,
            rhs,
        } => (1, *form, *dst, *lhs, *rhs),
        Mul {
            form,
            dst,
            lhs,
            rhs,
        } => (2, *form, *dst, *lhs, *rhs),
        Div {
            form,
            dst,
            lhs,
            rhs,
        } => (3, *form, *dst, *lhs, *rhs),
        Rem {
            form,
            dst,
            lhs,
            rhs,
        } => (4, *form, *dst, *lhs, *rhs),
        Neg { form, dst, src } => (5, *form, *dst, *src, *src),
        Move { dst, src } => (
            6,
            function.registers.get(*src as usize)?.kind,
            *dst,
            *src,
            *src,
        ),
        Equal {
            form,
            dst,
            lhs,
            rhs,
        } => (7, *form, *dst, *lhs, *rhs),
        NotEqual {
            form,
            dst,
            lhs,
            rhs,
        } => (8, *form, *dst, *lhs, *rhs),
        Less {
            form,
            dst,
            lhs,
            rhs,
        } => (9, *form, *dst, *lhs, *rhs),
        LessEqual {
            form,
            dst,
            lhs,
            rhs,
        } => (10, *form, *dst, *lhs, *rhs),
        Greater {
            form,
            dst,
            lhs,
            rhs,
        } => (11, *form, *dst, *lhs, *rhs),
        GreaterEqual {
            form,
            dst,
            lhs,
            rhs,
        } => (12, *form, *dst, *lhs, *rhs),
        BitAnd {
            form,
            dst,
            lhs,
            rhs,
        } => (30, *form, *dst, *lhs, *rhs),
        BitOr {
            form,
            dst,
            lhs,
            rhs,
        } => (31, *form, *dst, *lhs, *rhs),
        BitXor {
            form,
            dst,
            lhs,
            rhs,
        } => (32, *form, *dst, *lhs, *rhs),
        ShiftLeft {
            form,
            dst,
            lhs,
            rhs,
        } => (33, *form, *dst, *lhs, *rhs),
        ShiftRight {
            form,
            dst,
            lhs,
            rhs,
        } => (34, *form, *dst, *lhs, *rhs),
        ShiftUnsigned {
            form,
            dst,
            lhs,
            rhs,
        } => (35, *form, *dst, *lhs, *rhs),
        _ => return None,
    };
    let (group, kind) = match form {
        1..=4 => (group, form),
        8 | 9 if group <= 2 => (group, form - 7),
        8 | 9 if (7..=12).contains(&group) => (group + 6, form - 7),
        8 | 9 if group == 3 || group == 4 => (group + 23, form - 7),
        _ => return None,
    };
    let physical_dst = offset(
        function,
        u32::from(dst),
        if (7..=18).contains(&group) { 5 } else { kind },
    )?;
    let physical_lhs = offset(function, u32::from(lhs), kind)?;
    let physical_rhs = offset(function, u32::from(rhs), if group >= 33 { 1 } else { kind })?;
    let opcode = group * 4 + u32::from(kind);
    Some(Operation {
        opcode,
        dst: physical_dst,
        lhs: physical_lhs,
        rhs: physical_rhs,
    })
}

fn scalar(function: &ResolvedFunction, register: u16) -> Option<(u8, u32)> {
    let kind = function.registers.get(register as usize)?.kind;
    Some((kind, offset(function, u32::from(register), kind)?))
}

fn prepare_extra(
    instruction: &ResolvedInstruction,
    function: &ResolvedFunction,
    constants: &[RuntimeValue],
    plans: &mut Plans,
) -> Result<Option<Operation>, AdmissionError> {
    use ResolvedInstruction::*;
    let op = |opcode, dst, lhs, rhs| Operation {
        opcode,
        dst,
        lhs,
        rhs,
    };
    let simple = || -> Option<Operation> {
        Some(match instruction {
            Nop => op(NOP, 0, 0, 0),
            Jump { target } => op(JUMP, u32::try_from(*target).ok()?, 0, 0),
            Branch {
                condition,
                true_block,
                false_block,
            } => op(
                BRANCH,
                offset(function, u32::from(*condition), 5)?,
                u32::try_from(*true_block).ok()?,
                u32::try_from(*false_block).ok()?,
            ),
            Move { dst, src } => {
                let (kind, src) = scalar(function, *src)?;
                let (_, dst) = scalar(function, *dst)?;
                op(
                    match kind {
                        2 | 4 => COPY64,
                        5 => COPY_BOOL,
                        6 => COPY_CHAR,
                        _ => COPY32,
                    },
                    dst,
                    src,
                    0,
                )
            }
            InlineComponent {
                dst,
                src,
                component,
            } => {
                let source = function
                    .frame_layout
                    .values
                    .get(*src as usize)?
                    .components
                    .get(*component as usize)?;
                let destination = function
                    .frame_layout
                    .values
                    .get(*dst as usize)?
                    .components
                    .first()?;
                if source.atom != destination.atom {
                    return None;
                }
                op(
                    if source.atom.byte_size() == 8 {
                        COPY64
                    } else {
                        COPY32
                    },
                    destination.offset,
                    source.offset,
                    0,
                )
            }
            Const { dst, constant } => {
                let (_, dst) = scalar(function, *dst)?;
                let (bits, wide) = match constants.get(*constant)? {
                    RuntimeValue::I32(v) => (*v as u32 as u64, false),
                    RuntimeValue::I64(v) => (*v as u64, true),
                    RuntimeValue::F32(v) => (u64::from(*v), false),
                    RuntimeValue::F64(v) => (*v, true),
                    RuntimeValue::Bool(v) => (u64::from(*v), false),
                    RuntimeValue::Char(v) => (u64::from(*v), false),
                    RuntimeValue::Null => (0, false),
                    RuntimeValue::Reference(v) => (u64::from(v.to_bits()), false),
                };
                op(
                    if wide { IMMEDIATE64 } else { IMMEDIATE32 },
                    dst,
                    bits as u32,
                    (bits >> 32) as u32,
                )
            }
            Null { dst } => op(IMMEDIATE32, scalar(function, *dst)?.1, 0, 0),
            ArrayLength { dst, array } => op(
                ARRAY_LENGTH,
                offset(function, u32::from(*dst), 1)?,
                offset(function, u32::from(*array), 7)?,
                0,
            ),
            ArrayLoad { dst, array, index } => {
                let (kind, destination) = scalar(function, *dst)?;
                let nullable = function.registers.get(*dst as usize)?.nullable;
                op(
                    (ARRAY_LOAD + u32::from(kind))
                        | if kind == 7 && !nullable { NON_NULL } else { 0 },
                    destination,
                    offset(function, u32::from(*array), 7)?,
                    offset(function, u32::from(*index), 1)?,
                )
            }
            ArrayStore {
                array,
                index,
                value,
            } => {
                let (kind, source) = scalar(function, *value)?;
                op(
                    ARRAY_STORE + u32::from(kind),
                    source,
                    offset(function, u32::from(*array), 7)?,
                    offset(function, u32::from(*index), 1)?,
                )
            }
            _ => return None,
        })
    };
    if let Some(operation) = simple() {
        return Ok(Some(operation));
    }
    match instruction {
        InlineConstruct { dst, components } => {
            let destination = function
                .frame_layout
                .values
                .get(*dst as usize)
                .ok_or(AdmissionError::InvalidEntry)?;
            if destination.components.len() != components.len() {
                return Err(AdmissionError::InvalidEntry);
            }
            let start = u32::try_from(plans.copies.len())
                .map_err(|_| AdmissionError::StoragePlanOverflow)?;
            plans
                .copies
                .try_reserve(components.len())
                .map_err(|_| AdmissionError::AllocationFailed)?;
            for (source, destination) in components.iter().zip(destination.components.iter()) {
                let source = function
                    .frame_layout
                    .values
                    .get(*source as usize)
                    .and_then(|v| v.components.first())
                    .ok_or(AdmissionError::InvalidEntry)?;
                if source.atom != destination.atom {
                    return Err(AdmissionError::InvalidEntry);
                }
                plans.copies.push(Copy {
                    dst: destination.offset,
                    src: source.offset,
                    width: source.atom.byte_size(),
                });
            }
            let end = u32::try_from(plans.copies.len())
                .map_err(|_| AdmissionError::StoragePlanOverflow)?;
            Ok(Some(op(COPIES, start, end, 0)))
        }
        Move { dst, src }
            if function
                .registers
                .get(*src as usize)
                .is_some_and(|v| v.kind == 8) =>
        {
            let source = function
                .frame_layout
                .values
                .get(*src as usize)
                .ok_or(AdmissionError::InvalidEntry)?;
            let destination = function
                .frame_layout
                .values
                .get(*dst as usize)
                .ok_or(AdmissionError::InvalidEntry)?;
            if source.components.len() != destination.components.len() {
                return Err(AdmissionError::InvalidEntry);
            }
            let start = u32::try_from(plans.copies.len())
                .map_err(|_| AdmissionError::StoragePlanOverflow)?;
            plans
                .copies
                .try_reserve(source.components.len())
                .map_err(|_| AdmissionError::AllocationFailed)?;
            for (source, destination) in source.components.iter().zip(destination.components.iter())
            {
                if source.atom != destination.atom {
                    return Err(AdmissionError::InvalidEntry);
                }
                plans.copies.push(Copy {
                    dst: destination.offset,
                    src: source.offset,
                    width: source.atom.byte_size(),
                });
            }
            let end = u32::try_from(plans.copies.len())
                .map_err(|_| AdmissionError::StoragePlanOverflow)?;
            Ok(Some(op(COPIES, start, end, 0)))
        }
        FieldGet {
            dst,
            receiver,
            field,
        }
        | FieldSet {
            receiver,
            field,
            value: dst,
        } => {
            let Some((kind, value)) = scalar(function, *dst) else {
                return Ok(None);
            };
            let Some(receiver) = offset(function, u32::from(*receiver), 7) else {
                return Ok(None);
            };
            let id = u32::try_from(plans.fields.len())
                .map_err(|_| AdmissionError::StoragePlanOverflow)?;
            plans
                .fields
                .try_reserve(1)
                .map_err(|_| AdmissionError::AllocationFailed)?;
            plans.fields.push(*field);
            Ok(Some(op(
                if matches!(instruction, FieldGet { .. }) {
                    FIELD_GET
                } else {
                    FIELD_SET
                } + u32::from(kind),
                value,
                receiver,
                id,
            )))
        }
        _ => Ok(None),
    }
}

pub(super) enum RunEnd {
    Boundary,
    Branch(usize),
    Failure(InstructionFailure),
}

pub(super) struct SimpleRun {
    pub instruction: usize,
    pub attempts: u64,
    pub end: RunEnd,
}

pub(super) struct RunBudget {
    pub limit: u64,
    pub headroom: u64,
}

// No allocation, safepoint or external effect occurs while the frame is borrowed.
// Flush the logical PC and attempts before the machine handles any boundary/failure.
pub(super) fn execute_run(
    code: &PreparedCode,
    frame: &mut PreparedFrame<'_>,
    code_start: usize,
    mut instruction: usize,
    block_len: usize,
    budget: RunBudget,
    mut heap: impl FnMut(Operation, &mut PreparedFrame<'_>) -> Result<(), InstructionFailure>,
) -> SimpleRun {
    let mut attempts = 0;
    let end = loop {
        if instruction >= block_len || attempts >= budget.limit {
            break RunEnd::Boundary;
        }
        let Some(op) = code.operation(code_start + instruction) else {
            break RunEnd::Failure(InstructionFailure::Fault(VmFault::InvalidStoragePlan));
        };
        if op.opcode == 0 {
            break RunEnd::Boundary;
        }
        if attempts == budget.headroom {
            break RunEnd::Failure(InstructionFailure::Fault(VmFault::AccountingOverflow));
        }
        attempts += 1;
        let result = if op.opcode < COPY32 || (105..=142).contains(&op.opcode) {
            execute_numeric(*op, frame).map(|()| None)
        } else if op.opcode < FIELD_GET {
            execute_frame(*op, frame, &code.copies)
        } else {
            heap(*op, frame).map(|()| None)
        };
        match result {
            Ok(None) => instruction += 1,
            Ok(Some(target)) => break RunEnd::Branch(target),
            Err(failure) => break RunEnd::Failure(failure),
        }
    };
    SimpleRun {
        instruction,
        attempts,
        end,
    }
}

#[inline]
pub(super) fn execute_numeric(
    op: Operation,
    frame: &mut PreparedFrame<'_>,
) -> Result<(), InstructionFailure> {
    macro_rules! read {
        ($ty:ty, $src:expr) => {
            <$ty>::from_le_bytes(frame.read($src).map_err(InstructionFailure::Fault)?)
        };
    }
    macro_rules! write {
        ($value:expr) => {
            frame
                .write(op.dst, ($value).to_le_bytes())
                .map_err(InstructionFailure::Fault)
        };
    }
    macro_rules! binary {
        ($ty:ty, $fun:path) => {{
            let lhs = read!($ty, op.lhs);
            let rhs = read!($ty, op.rhs);
            write!($fun(lhs, rhs))
        }};
    }
    macro_rules! fallible {
        ($ty:ty, $fun:path) => {{
            let lhs = read!($ty, op.lhs);
            let rhs = read!($ty, op.rhs);
            write!($fun(lhs, rhs).map_err(InstructionFailure::Trap)?)
        }};
    }
    macro_rules! comparison {
        ($ty:ty, $fun:expr) => {{
            let lhs = read!($ty, op.lhs);
            let rhs = read!($ty, op.rhs);
            write!(i32::from(($fun)(lhs, rhs)))
        }};
    }
    match op.opcode {
        1 => binary!(i32, numeric::add_i32),
        2 => binary!(i64, numeric::add_i64),
        3 => binary!(f32, numeric::add_f32),
        4 => binary!(f64, numeric::add_f64),
        5 => binary!(i32, numeric::sub_i32),
        6 => binary!(i64, numeric::sub_i64),
        7 => binary!(f32, numeric::sub_f32),
        8 => binary!(f64, numeric::sub_f64),
        9 => binary!(i32, numeric::mul_i32),
        10 => binary!(i64, numeric::mul_i64),
        11 => binary!(f32, numeric::mul_f32),
        12 => binary!(f64, numeric::mul_f64),
        13 => fallible!(i32, numeric::div_i32),
        14 => fallible!(i64, numeric::div_i64),
        15 => binary!(f32, numeric::div_f32),
        16 => binary!(f64, numeric::div_f64),
        17 => fallible!(i32, numeric::rem_i32),
        18 => fallible!(i64, numeric::rem_i64),
        19 => binary!(f32, numeric::rem_f32),
        20 => binary!(f64, numeric::rem_f64),
        21 => write!(numeric::neg_i32(read!(i32, op.lhs))),
        22 => write!(numeric::neg_i64(read!(i64, op.lhs))),
        23 => write!(numeric::neg_f32(read!(f32, op.lhs))),
        24 => write!(numeric::neg_f64(read!(f64, op.lhs))),
        25 => write!(read!(i32, op.lhs)),
        26 => write!(read!(i64, op.lhs)),
        27 => write!(read!(f32, op.lhs)),
        28 => write!(read!(f64, op.lhs)),
        29 => comparison!(i32, |a, b| a == b),
        30 => comparison!(i64, |a, b| a == b),
        31 => comparison!(f32, numeric::eq_f32),
        32 => comparison!(f64, numeric::eq_f64),
        33 => comparison!(i32, |a, b| a != b),
        34 => comparison!(i64, |a, b| a != b),
        35 => comparison!(f32, numeric::ne_f32),
        36 => comparison!(f64, numeric::ne_f64),
        37 => comparison!(i32, |a, b| a < b),
        38 => comparison!(i64, |a, b| a < b),
        39 => comparison!(f32, numeric::lt_f32),
        40 => comparison!(f64, numeric::lt_f64),
        41 => comparison!(i32, |a, b| a <= b),
        42 => comparison!(i64, |a, b| a <= b),
        43 => comparison!(f32, numeric::le_f32),
        44 => comparison!(f64, numeric::le_f64),
        45 => comparison!(i32, |a, b| a > b),
        46 => comparison!(i64, |a, b| a > b),
        47 => comparison!(f32, numeric::gt_f32),
        48 => comparison!(f64, numeric::gt_f64),
        49 => comparison!(i32, |a, b| a >= b),
        50 => comparison!(i64, |a, b| a >= b),
        51 => comparison!(f32, numeric::ge_f32),
        52 => comparison!(f64, numeric::ge_f64),
        53 => comparison!(u32, |a, b| a == b),
        54 => comparison!(u64, |a, b| a == b),
        57 => comparison!(u32, |a, b| a != b),
        58 => comparison!(u64, |a, b| a != b),
        61 => comparison!(u32, |a, b| a < b),
        62 => comparison!(u64, |a, b| a < b),
        65 => comparison!(u32, |a, b| a <= b),
        66 => comparison!(u64, |a, b| a <= b),
        69 => comparison!(u32, |a, b| a > b),
        70 => comparison!(u64, |a, b| a > b),
        73 => comparison!(u32, |a, b| a >= b),
        74 => comparison!(u64, |a, b| a >= b),
        105 => fallible!(u32, div_u32),
        106 => fallible!(u64, div_u64),
        109 => fallible!(u32, rem_u32),
        110 => fallible!(u64, rem_u64),
        121 => binary!(i32, and_i32),
        122 => binary!(i64, and_i64),
        125 => binary!(i32, or_i32),
        126 => binary!(i64, or_i64),
        129 => binary!(i32, xor_i32),
        130 => binary!(i64, xor_i64),
        133 => write!(numeric::shl_i32(read!(i32, op.lhs), read!(i32, op.rhs))),
        134 => write!(numeric::shl_i64(read!(i64, op.lhs), read!(i32, op.rhs))),
        137 => write!(numeric::shr_i32(read!(i32, op.lhs), read!(i32, op.rhs))),
        138 => write!(numeric::shr_i64(read!(i64, op.lhs), read!(i32, op.rhs))),
        141 => write!(numeric::ushr_i32(read!(i32, op.lhs), read!(i32, op.rhs))),
        142 => write!(numeric::ushr_i64(read!(i64, op.lhs), read!(i32, op.rhs))),
        _ => Err(InstructionFailure::Fault(VmFault::UnsupportedInstruction)),
    }
}

#[inline]
pub(super) fn execute_frame(
    op: Operation,
    frame: &mut PreparedFrame<'_>,
    copies: &[Copy],
) -> Result<Option<usize>, InstructionFailure> {
    macro_rules! read {
        ($ty:ty, $src:expr) => {
            <$ty>::from_le_bytes(frame.read($src).map_err(InstructionFailure::Fault)?)
        };
    }
    macro_rules! write {
        ($value:expr) => {
            frame
                .write(op.dst, ($value).to_le_bytes())
                .map_err(InstructionFailure::Fault)
        };
    }
    match op.opcode {
        NOP => Ok(()),
        COPY32 => write!(read!(u32, op.lhs)),
        COPY64 => write!(read!(u64, op.lhs)),
        COPY_BOOL => {
            let value = read!(u32, op.lhs);
            if value > 1 {
                return Err(InstructionFailure::Fault(VmFault::InvalidValueType));
            }
            write!(value)
        }
        COPY_CHAR => {
            let value = read!(u32, op.lhs);
            if value > u32::from(u16::MAX) {
                return Err(InstructionFailure::Fault(VmFault::InvalidValueType));
            }
            write!(value)
        }
        IMMEDIATE32 => write!(op.lhs),
        IMMEDIATE64 => write!(u64::from(op.lhs) | u64::from(op.rhs) << 32),
        COPIES => {
            let copies = copies
                .get(op.dst as usize..op.lhs as usize)
                .ok_or(InstructionFailure::Fault(VmFault::InvalidStoragePlan))?;
            // Keep the existing sequential component semantics when locations overlap.
            for copy in copies {
                match copy.width {
                    4 => {
                        let value = read!(u32, copy.src);
                        frame
                            .write(copy.dst, value.to_le_bytes())
                            .map_err(InstructionFailure::Fault)?;
                    }
                    8 => {
                        let value = read!(u64, copy.src);
                        frame
                            .write(copy.dst, value.to_le_bytes())
                            .map_err(InstructionFailure::Fault)?;
                    }
                    _ => return Err(InstructionFailure::Fault(VmFault::InvalidStoragePlan)),
                }
            }
            Ok(())
        }
        JUMP => return Ok(Some(op.dst as usize)),
        BRANCH => {
            return Ok(Some(match read!(u32, op.dst) {
                0 => op.rhs as usize,
                1 => op.lhs as usize,
                _ => return Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
            }))
        }
        _ => Err(InstructionFailure::Fault(VmFault::UnsupportedInstruction)),
    }?;
    Ok(None)
}

#[cfg(test)]
pub(super) mod tests;

macro_rules! unsigned_division {
    ($div:ident,$rem:ident,$ty:ty) => {
        fn $div(a: $ty, b: $ty) -> Result<$ty, super::error::GuestTrap> {
            a.checked_div(b)
                .ok_or(super::error::GuestTrap::DivisionByZero)
        }
        fn $rem(a: $ty, b: $ty) -> Result<$ty, super::error::GuestTrap> {
            a.checked_rem(b)
                .ok_or(super::error::GuestTrap::DivisionByZero)
        }
    };
}
unsigned_division!(div_u32, rem_u32, u32);
unsigned_division!(div_u64, rem_u64, u64);
macro_rules! bit_ops {
    ($and:ident,$or:ident,$xor:ident,$ty:ty) => {
        fn $and(a: $ty, b: $ty) -> $ty {
            a & b
        }
        fn $or(a: $ty, b: $ty) -> $ty {
            a | b
        }
        fn $xor(a: $ty, b: $ty) -> $ty {
            a ^ b
        }
    };
}
bit_ops!(and_i32, or_i32, xor_i32, i32);
bit_ops!(and_i64, or_i64, xor_i64, i64);
