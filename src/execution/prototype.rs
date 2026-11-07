/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 * You may obtain a copy at https://www.apache.org/licenses/LICENSE-2.0
 */

//! Experimental, derived execution representations; never an executable ABI.
//! Opcode zero is a reference-interpreter exit at the same logical instruction.
use super::{
    error::{AdmissionError, VmFault},
    frame::{FrameArena, FrameReservation},
    image::{ResolvedBlock, ResolvedFunction, ResolvedInstruction},
    machine::{Frame, InstructionFailure},
    numeric,
};
use crate::artifact::PhysicalAtom;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionMode {
    Packed,
    PackedBatch,
    PackedSimd,
    Offsets,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PreparationStats {
    pub instructions: usize,
    pub eligible: usize,
    pub code_bytes: usize,
    pub index_bytes: usize,
    pub reference_enum_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub(super) struct Operation {
    opcode: u32,
    dst: u32,
    lhs: u32,
    rhs: u32,
}

#[derive(Clone, Copy)]
#[repr(C, align(64))]
struct CacheLine<T, const N: usize>([T; N]);

struct Dense<T, const N: usize> {
    lines: Box<[CacheLine<T, N>]>,
    len: usize,
}

impl<T: Copy + Default, const N: usize> Dense<T, N> {
    fn new(values: &[T]) -> Result<Self, AdmissionError> {
        let mut lines = Vec::new();
        lines
            .try_reserve_exact(values.len().div_ceil(N))
            .map_err(|_| AdmissionError::AllocationFailed)?;
        for chunk in values.chunks(N) {
            let mut line = CacheLine([T::default(); N]);
            line.0[..chunk.len()].copy_from_slice(chunk);
            lines.push(line);
        }
        Ok(Self {
            lines: lines.into_boxed_slice(),
            len: values.len(),
        })
    }

    #[inline(always)]
    fn get(&self, index: usize) -> Option<T> {
        (index < self.len).then(|| self.lines[index / N].0[index % N])
    }

    fn bytes(&self) -> usize {
        self.lines.len() * core::mem::size_of::<CacheLine<T, N>>()
    }
}

enum Code {
    Packed(Dense<u32, 16>),
    Offsets(Dense<Operation, 4>),
}

pub(super) struct PreparedExecution {
    code: Code,
    block_starts: Box<[usize]>,
    stats: PreparationStats,
    batch_decode: bool,
    simd_decode: bool,
}

// Mutable decode state belongs to the instance, separately from immutable code.
pub(super) struct DecodeCache {
    block: usize,
    first: usize,
    operations: [Operation; 4],
}

pub(crate) fn decode_cache_bytes() -> usize {
    core::mem::size_of::<DecodeCache>()
}

impl Default for DecodeCache {
    fn default() -> Self {
        Self {
            block: usize::MAX,
            first: usize::MAX,
            operations: [Operation::default(); 4],
        }
    }
}

impl PreparedExecution {
    pub(super) fn new(
        blocks: &[ResolvedBlock],
        functions: &[ResolvedFunction],
        mode: ExecutionMode,
    ) -> Result<Self, AdmissionError> {
        let count = blocks
            .iter()
            .try_fold(0usize, |n, b| n.checked_add(b.instructions.len()))
            .ok_or(AdmissionError::StoragePlanOverflow)?;
        let mut starts = Vec::new();
        starts
            .try_reserve_exact(
                blocks
                    .len()
                    .checked_add(1)
                    .ok_or(AdmissionError::StoragePlanOverflow)?,
            )
            .map_err(|_| AdmissionError::AllocationFailed)?;
        let mut operations = Vec::new();
        operations
            .try_reserve_exact(count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        let mut eligible = 0;
        for block in blocks {
            starts.push(operations.len());
            let function = functions
                .get(block.function)
                .ok_or(AdmissionError::InvalidEntry)?;
            for instruction in &block.instructions {
                let operation = prepare(instruction, function, mode).unwrap_or_default();
                eligible += usize::from(operation.opcode != 0);
                operations.push(operation);
            }
        }
        starts.push(operations.len());
        let code = match mode {
            ExecutionMode::Packed | ExecutionMode::PackedBatch | ExecutionMode::PackedSimd => {
                let mut words = Vec::new();
                words
                    .try_reserve_exact(count)
                    .map_err(|_| AdmissionError::AllocationFailed)?;
                for op in &operations {
                    words.push(op.opcode | op.dst << 8 | op.lhs << 16 | op.rhs << 24);
                }
                Code::Packed(Dense::new(&words)?)
            }
            ExecutionMode::Offsets => Code::Offsets(Dense::new(&operations)?),
        };
        let code_bytes = match &code {
            Code::Packed(code) => code.bytes(),
            Code::Offsets(code) => code.bytes(),
        };
        let stats = PreparationStats {
            instructions: count,
            eligible,
            code_bytes,
            index_bytes: starts.len() * core::mem::size_of::<usize>(),
            // Enum slots only: does not count payload allocations or image metadata.
            reference_enum_bytes: count * core::mem::size_of::<ResolvedInstruction>(),
        };
        Ok(Self {
            code,
            block_starts: starts.into_boxed_slice(),
            stats,
            batch_decode: matches!(mode, ExecutionMode::PackedBatch | ExecutionMode::PackedSimd),
            simd_decode: mode == ExecutionMode::PackedSimd,
        })
    }

    pub(super) fn stats(&self) -> PreparationStats {
        self.stats
    }

    #[inline(always)]
    pub(super) fn operation(
        &self,
        block: usize,
        pc: usize,
        cache: &mut DecodeCache,
    ) -> Option<Operation> {
        let start = *self.block_starts.get(block)?;
        let end = *self.block_starts.get(block.checked_add(1)?)?;
        let index = start.checked_add(pc).filter(|index| *index < end)?;
        let op = match &self.code {
            Code::Packed(code) => {
                if self.batch_decode {
                    let first = pc / 4 * 4;
                    if cache.block != block || cache.first != first {
                        let mut words = [0u32; 4];
                        for (lane, word) in words.iter_mut().enumerate() {
                            let position = start.checked_add(first)?.checked_add(lane)?;
                            if position < end {
                                *word = code.get(position)?;
                            }
                        }
                        if self.simd_decode {
                            unpack_four(&words, &mut cache.operations);
                        } else {
                            unpack_scalar(&words, &mut cache.operations);
                        }
                        cache.block = block;
                        cache.first = first;
                    }
                    let op = cache.operations[pc - first];
                    return (op.opcode != 0).then_some(op);
                }
                let word = code.get(index)?;
                Operation {
                    opcode: word & 255,
                    dst: word >> 8 & 255,
                    lhs: word >> 16 & 255,
                    rhs: word >> 24,
                }
            }
            Code::Offsets(code) => code.get(index)?,
        };
        (op.opcode != 0).then_some(op)
    }

    #[inline]
    pub(super) fn execute(
        &self,
        mut op: Operation,
        arena: &mut FrameArena,
        frame: Frame,
        function: &ResolvedFunction,
    ) -> Result<(), InstructionFailure> {
        if matches!(self.code, Code::Packed(_)) {
            let unary = (21..=28).contains(&op.opcode);
            op.dst = register_offset(function, op.dst)
                .ok_or(InstructionFailure::Fault(VmFault::InvalidStoragePlan))?;
            op.lhs = register_offset(function, op.lhs)
                .ok_or(InstructionFailure::Fault(VmFault::InvalidStoragePlan))?;
            op.rhs = if unary {
                op.lhs
            } else {
                register_offset(function, op.rhs)
                    .ok_or(InstructionFailure::Fault(VmFault::InvalidStoragePlan))?
            };
        }
        execute(
            op,
            arena,
            FrameReservation {
                base: frame.base,
                byte_len: frame.byte_len,
            },
        )
    }
}

fn unpack_scalar(words: &[u32; 4], output: &mut [Operation; 4]) {
    for (word, op) in words.iter().zip(output) {
        *op = Operation {
            opcode: word & 255,
            dst: word >> 8 & 255,
            lhs: word >> 16 & 255,
            rhs: word >> 24,
        };
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn unpack_four(words: &[u32; 4], output: &mut [Operation; 4]) {
    unpack_scalar(words, output);
}

#[cfg(target_arch = "x86_64")]
fn unpack_four(words: &[u32; 4], output: &mut [Operation; 4]) {
    use std::arch::x86_64::*;
    const {
        assert!(core::mem::size_of::<Operation>() == 16);
    }
    // SAFETY: SSE2 is part of the x86-64 baseline. Both arrays contain exactly
    // 16 input / 64 output bytes, cannot alias, and unaligned loads/stores are used.
    // repr(C) Operation consists of four u32 fields and accepts every bit pattern.
    unsafe {
        let input = _mm_loadu_si128(words.as_ptr().cast());
        let mask = _mm_set1_epi32(255);
        let opcode = _mm_and_si128(input, mask);
        let dst = _mm_and_si128(_mm_srli_epi32::<8>(input), mask);
        let lhs = _mm_and_si128(_mm_srli_epi32::<16>(input), mask);
        let rhs = _mm_srli_epi32::<24>(input);
        let low_ab = _mm_unpacklo_epi32(opcode, dst);
        let high_ab = _mm_unpackhi_epi32(opcode, dst);
        let low_cd = _mm_unpacklo_epi32(lhs, rhs);
        let high_cd = _mm_unpackhi_epi32(lhs, rhs);
        let target = output.as_mut_ptr().cast();
        _mm_storeu_si128(target, _mm_unpacklo_epi64(low_ab, low_cd));
        _mm_storeu_si128(target.add(1), _mm_unpackhi_epi64(low_ab, low_cd));
        _mm_storeu_si128(target.add(2), _mm_unpacklo_epi64(high_ab, high_cd));
        _mm_storeu_si128(target.add(3), _mm_unpackhi_epi64(high_ab, high_cd));
    }
}

// Groups 0..12: add/sub/mul/div/rem/neg/move/eq/ne/lt/le/gt/ge,
// with concrete I32/I64/F32/F64 opcodes. Groups 13..18: unsigned comparisons.
fn register_offset(function: &ResolvedFunction, register: u32) -> Option<u32> {
    function
        .register_accesses
        .get(register as usize)?
        .prepared_offset()
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
        1 | 5 => PhysicalAtom::I32,
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

fn prepare(
    instruction: &ResolvedInstruction,
    function: &ResolvedFunction,
    mode: ExecutionMode,
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
        _ => return None,
    };
    let (group, kind) = match form {
        1..=4 => (group, form),
        8 | 9 if group <= 2 => (group, form - 7),
        8 | 9 if group >= 7 => (group + 6, form - 7),
        _ => return None,
    };
    let physical_dst = offset(function, u32::from(dst), if group >= 7 { 5 } else { kind })?;
    let physical_lhs = offset(function, u32::from(lhs), kind)?;
    let physical_rhs = offset(function, u32::from(rhs), kind)?;
    let opcode = group * 4 + u32::from(kind);
    match mode {
        ExecutionMode::Packed | ExecutionMode::PackedBatch | ExecutionMode::PackedSimd
            if [dst, lhs, rhs].iter().all(|r| *r <= 255) =>
        {
            Some(Operation {
                opcode,
                dst: u32::from(dst),
                lhs: u32::from(lhs),
                rhs: u32::from(rhs),
            })
        }
        ExecutionMode::Packed | ExecutionMode::PackedBatch | ExecutionMode::PackedSimd => None,
        ExecutionMode::Offsets => Some(Operation {
            opcode,
            dst: physical_dst,
            lhs: physical_lhs,
            rhs: physical_rhs,
        }),
    }
}

#[inline]
fn execute(
    op: Operation,
    arena: &mut FrameArena,
    frame: FrameReservation,
) -> Result<(), InstructionFailure> {
    macro_rules! read {
        ($ty:ty, $src:expr) => {
            <$ty>::from_le_bytes(
                arena
                    .read_prepared(frame, $src)
                    .map_err(InstructionFailure::Fault)?,
            )
        };
    }
    macro_rules! write {
        ($value:expr) => {
            arena
                .write_prepared(frame, op.dst, ($value).to_le_bytes())
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
        _ => Err(InstructionFailure::Fault(VmFault::UnsupportedInstruction)),
    }
}

#[cfg(test)]
mod tests;
