/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

#![allow(
    dead_code,
    reason = "RV32 lowering consumes the complete typed emitter in subsequent issue #17 tasks"
)]

use thiserror::Error;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gpr {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl Gpr {
    const fn low(self) -> u8 {
        self as u8 & 7
    }

    const fn high(self) -> bool {
        self as u8 & 8 != 0
    }

    const fn needs_byte_rex(self) -> bool {
        self.high() || self.low() >= 4
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scale {
    One,
    Two,
    Four,
    Eight,
}

impl Scale {
    const fn bits(self) -> u8 {
        match self {
            Self::One => 0,
            Self::Two => 1,
            Self::Four => 2,
            Self::Eight => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mem {
    base: Gpr,
    index: Option<(Gpr, Scale)>,
    disp: i32,
}

impl Mem {
    pub(crate) const fn base_disp(base: Gpr, disp: i32) -> Self {
        Self {
            base,
            index: None,
            disp,
        }
    }

    pub(crate) const fn base_index_disp(base: Gpr, index: Gpr, scale: Scale, disp: i32) -> Self {
        Self {
            base,
            index: Some((index, scale)),
            disp,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Condition {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Below,
    BelowEqual,
    Above,
    AboveEqual,
}

impl Condition {
    const fn code(self) -> u8 {
        match self {
            Self::Equal => 0x4,
            Self::NotEqual => 0x5,
            Self::Below => 0x2,
            Self::BelowEqual => 0x6,
            Self::Above => 0x7,
            Self::AboveEqual => 0x3,
            Self::Less => 0xc,
            Self::LessEqual => 0xe,
            Self::Greater => 0xf,
            Self::GreaterEqual => 0xd,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Label(u32);

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum EmitError {
    #[error("x86_64 emitter capacity must be positive")]
    InvalidCapacity,
    #[error("x86_64 emitter control capacity must be positive")]
    InvalidControlCapacity,
    #[error("x86_64 emitter exceeded its {limit}-byte capacity")]
    Capacity { limit: usize },
    #[error("x86_64 emitter exceeded its {limit}-entry control capacity")]
    ControlCapacity { limit: usize },
    #[error("x86_64 label {label} was not bound")]
    UnboundLabel { label: u32 },
    #[error("x86_64 label {label} was bound more than once")]
    DuplicateLabel { label: u32 },
    #[error("x86_64 branch displacement is outside rel32")]
    BranchRange,
    #[error("invalid x86_64 operand: {0}")]
    InvalidOperand(&'static str),
}

#[derive(Debug, Clone, Copy)]
struct Fixup {
    label: Label,
    displacement_offset: usize,
    instruction_end: usize,
}

pub(crate) struct X64Emitter {
    bytes: Vec<u8>,
    capacity: usize,
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
    control_capacity: usize,
}

impl X64Emitter {
    pub(crate) fn new(capacity: usize, control_capacity: usize) -> Result<Self, EmitError> {
        if capacity == 0 {
            return Err(EmitError::InvalidCapacity);
        }
        if control_capacity == 0 {
            return Err(EmitError::InvalidControlCapacity);
        }
        Ok(Self {
            bytes: Vec::with_capacity(capacity),
            capacity,
            labels: Vec::with_capacity(control_capacity),
            fixups: Vec::with_capacity(control_capacity),
            control_capacity,
        })
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn reset(&mut self) {
        self.bytes.clear();
        self.labels.clear();
        self.fixups.clear();
    }

    #[cfg(test)]
    pub(crate) fn buffer_capacities(&self) -> (usize, usize, usize) {
        (
            self.bytes.capacity(),
            self.labels.capacity(),
            self.fixups.capacity(),
        )
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.bytes
            .capacity()
            .saturating_add(
                self.labels
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Option<usize>>()),
            )
            .saturating_add(
                self.fixups
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Fixup>()),
            )
    }

    pub(crate) fn new_label(&mut self) -> Result<Label, EmitError> {
        if self.labels.len() == self.control_capacity {
            return Err(EmitError::ControlCapacity {
                limit: self.control_capacity,
            });
        }
        let label = Label(self.labels.len() as u32);
        self.labels.push(None);
        Ok(label)
    }

    pub(crate) fn bind(&mut self, label: Label) -> Result<(), EmitError> {
        let slot = self
            .labels
            .get_mut(label.0 as usize)
            .ok_or(EmitError::InvalidOperand("unknown label"))?;
        if slot.replace(self.bytes.len()).is_some() {
            return Err(EmitError::DuplicateLabel { label: label.0 });
        }
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<&[u8], EmitError> {
        for fixup in &self.fixups {
            let target = self
                .labels
                .get(fixup.label.0 as usize)
                .and_then(|offset| *offset)
                .ok_or(EmitError::UnboundLabel {
                    label: fixup.label.0,
                })?;
            let displacement = target as i64 - fixup.instruction_end as i64;
            let displacement = i32::try_from(displacement).map_err(|_| EmitError::BranchRange)?;
            self.bytes[fixup.displacement_offset..fixup.displacement_offset + 4]
                .copy_from_slice(&displacement.to_le_bytes());
        }
        Ok(&self.bytes)
    }

    pub(crate) fn push(&mut self, register: Gpr) -> Result<(), EmitError> {
        self.with_rollback(|out| {
            out.emit_rex(false, false, false, register.high(), false)?;
            out.emit_u8(0x50 + register.low())
        })
    }

    pub(crate) fn pop(&mut self, register: Gpr) -> Result<(), EmitError> {
        self.with_rollback(|out| {
            out.emit_rex(false, false, false, register.high(), false)?;
            out.emit_u8(0x58 + register.low())
        })
    }

    pub(crate) fn mov_r32_imm32(&mut self, dst: Gpr, value: u32) -> Result<(), EmitError> {
        self.with_rollback(|out| {
            out.emit_rex(false, false, false, dst.high(), false)?;
            out.emit_u8(0xb8 + dst.low())?;
            out.emit_bytes(&value.to_le_bytes())
        })
    }

    pub(crate) fn mov_r64_imm64(&mut self, dst: Gpr, value: u64) -> Result<(), EmitError> {
        self.with_rollback(|out| {
            out.emit_rex(true, false, false, dst.high(), false)?;
            out.emit_u8(0xb8 + dst.low())?;
            out.emit_bytes(&value.to_le_bytes())
        })
    }

    pub(crate) fn mov_r32_r32(&mut self, dst: Gpr, src: Gpr) -> Result<(), EmitError> {
        self.reg_reg(&[0x89], false, src, dst, false)
    }

    pub(crate) fn mov_r64_r64(&mut self, dst: Gpr, src: Gpr) -> Result<(), EmitError> {
        self.reg_reg(&[0x89], true, src, dst, false)
    }

    pub(crate) fn mov_r32_m32(&mut self, dst: Gpr, src: Mem) -> Result<(), EmitError> {
        self.reg_mem(&[0x8b], false, dst, src, false)
    }

    pub(crate) fn mov_r64_m64(&mut self, dst: Gpr, src: Mem) -> Result<(), EmitError> {
        self.reg_mem(&[0x8b], true, dst, src, false)
    }

    pub(crate) fn mov_m32_r32(&mut self, dst: Mem, src: Gpr) -> Result<(), EmitError> {
        self.reg_mem(&[0x89], false, src, dst, false)
    }

    pub(crate) fn mov_m16_r16(&mut self, dst: Mem, src: Gpr) -> Result<(), EmitError> {
        self.with_rollback(|out| {
            out.emit_u8(0x66)?;
            out.emit_reg_mem_body(&[0x89], false, src, dst, false)
        })
    }

    pub(crate) fn mov_m8_r8(&mut self, dst: Mem, src: Gpr) -> Result<(), EmitError> {
        self.reg_mem(&[0x88], false, src, dst, src.needs_byte_rex())
    }

    pub(crate) fn movzx_r32_m8(&mut self, dst: Gpr, src: Mem) -> Result<(), EmitError> {
        self.reg_mem(&[0x0f, 0xb6], false, dst, src, false)
    }

    pub(crate) fn movsx_r32_m8(&mut self, dst: Gpr, src: Mem) -> Result<(), EmitError> {
        self.reg_mem(&[0x0f, 0xbe], false, dst, src, false)
    }

    pub(crate) fn movzx_r32_m16(&mut self, dst: Gpr, src: Mem) -> Result<(), EmitError> {
        self.reg_mem(&[0x0f, 0xb7], false, dst, src, false)
    }

    pub(crate) fn movsx_r32_m16(&mut self, dst: Gpr, src: Mem) -> Result<(), EmitError> {
        self.reg_mem(&[0x0f, 0xbf], false, dst, src, false)
    }

    pub(crate) fn movzx_r32_r8(&mut self, dst: Gpr, src: Gpr) -> Result<(), EmitError> {
        self.reg_reg(&[0x0f, 0xb6], false, dst, src, src.needs_byte_rex())
    }

    pub(crate) fn lea_r64_mem(&mut self, dst: Gpr, src: Mem) -> Result<(), EmitError> {
        self.reg_mem(&[0x8d], true, dst, src, false)
    }

    pub(crate) fn add_r32_r32(&mut self, dst: Gpr, src: Gpr) -> Result<(), EmitError> {
        self.reg_reg(&[0x01], false, src, dst, false)
    }

    pub(crate) fn sub_r32_r32(&mut self, dst: Gpr, src: Gpr) -> Result<(), EmitError> {
        self.reg_reg(&[0x29], false, src, dst, false)
    }

    pub(crate) fn and_r32_r32(&mut self, dst: Gpr, src: Gpr) -> Result<(), EmitError> {
        self.reg_reg(&[0x21], false, src, dst, false)
    }

    pub(crate) fn or_r32_r32(&mut self, dst: Gpr, src: Gpr) -> Result<(), EmitError> {
        self.reg_reg(&[0x09], false, src, dst, false)
    }

    pub(crate) fn xor_r32_r32(&mut self, dst: Gpr, src: Gpr) -> Result<(), EmitError> {
        self.reg_reg(&[0x31], false, src, dst, false)
    }

    pub(crate) fn cmp_r32_r32(&mut self, lhs: Gpr, rhs: Gpr) -> Result<(), EmitError> {
        self.reg_reg(&[0x39], false, rhs, lhs, false)
    }

    pub(crate) fn test_r32_r32(&mut self, lhs: Gpr, rhs: Gpr) -> Result<(), EmitError> {
        self.reg_reg(&[0x85], false, rhs, lhs, false)
    }

    pub(crate) fn add_r32_imm32(&mut self, dst: Gpr, value: i32) -> Result<(), EmitError> {
        self.group_imm32(dst, 0, value)
    }

    pub(crate) fn sub_r32_imm32(&mut self, dst: Gpr, value: i32) -> Result<(), EmitError> {
        self.group_imm32(dst, 5, value)
    }

    pub(crate) fn and_r32_imm32(&mut self, dst: Gpr, value: i32) -> Result<(), EmitError> {
        self.group_imm32(dst, 4, value)
    }

    pub(crate) fn or_r32_imm32(&mut self, dst: Gpr, value: i32) -> Result<(), EmitError> {
        self.group_imm32(dst, 1, value)
    }

    pub(crate) fn xor_r32_imm32(&mut self, dst: Gpr, value: i32) -> Result<(), EmitError> {
        self.group_imm32(dst, 6, value)
    }

    pub(crate) fn cmp_r32_imm32(&mut self, dst: Gpr, value: i32) -> Result<(), EmitError> {
        self.group_imm32(dst, 7, value)
    }

    pub(crate) fn shl_r32_imm8(&mut self, dst: Gpr, value: u8) -> Result<(), EmitError> {
        self.group_shift_imm8(dst, 4, value)
    }

    pub(crate) fn shr_r32_imm8(&mut self, dst: Gpr, value: u8) -> Result<(), EmitError> {
        self.group_shift_imm8(dst, 5, value)
    }

    pub(crate) fn sar_r32_imm8(&mut self, dst: Gpr, value: u8) -> Result<(), EmitError> {
        self.group_shift_imm8(dst, 7, value)
    }

    pub(crate) fn shl_r32_cl(&mut self, dst: Gpr) -> Result<(), EmitError> {
        self.group_register(dst, &[0xd3], 4)
    }

    pub(crate) fn shr_r32_cl(&mut self, dst: Gpr) -> Result<(), EmitError> {
        self.group_register(dst, &[0xd3], 5)
    }

    pub(crate) fn sar_r32_cl(&mut self, dst: Gpr) -> Result<(), EmitError> {
        self.group_register(dst, &[0xd3], 7)
    }

    pub(crate) fn setcc_r8(&mut self, condition: Condition, dst: Gpr) -> Result<(), EmitError> {
        self.with_rollback(|out| {
            out.emit_rex(false, false, false, dst.high(), dst.needs_byte_rex())?;
            out.emit_bytes(&[0x0f, 0x90 + condition.code()])?;
            out.emit_modrm(3, 0, dst.low())
        })
    }

    pub(crate) fn cmovcc_r32_r32(
        &mut self,
        condition: Condition,
        dst: Gpr,
        src: Gpr,
    ) -> Result<(), EmitError> {
        self.reg_reg(&[0x0f, 0x40 + condition.code()], false, dst, src, false)
    }

    pub(crate) fn imul_r32_r32(&mut self, dst: Gpr, src: Gpr) -> Result<(), EmitError> {
        self.reg_reg(&[0x0f, 0xaf], false, dst, src, false)
    }

    pub(crate) fn mul_r32(&mut self, src: Gpr) -> Result<(), EmitError> {
        self.group_register(src, &[0xf7], 4)
    }

    pub(crate) fn imul_r32(&mut self, src: Gpr) -> Result<(), EmitError> {
        self.group_register(src, &[0xf7], 5)
    }

    pub(crate) fn div_r32(&mut self, src: Gpr) -> Result<(), EmitError> {
        self.group_register(src, &[0xf7], 6)
    }

    pub(crate) fn idiv_r32(&mut self, src: Gpr) -> Result<(), EmitError> {
        self.group_register(src, &[0xf7], 7)
    }

    pub(crate) fn cdq(&mut self) -> Result<(), EmitError> {
        self.with_rollback(|out| out.emit_u8(0x99))
    }

    pub(crate) fn jmp(&mut self, label: Label) -> Result<(), EmitError> {
        self.relative_branch(&[0xe9], label)
    }

    pub(crate) fn jcc(&mut self, condition: Condition, label: Label) -> Result<(), EmitError> {
        self.relative_branch(&[0x0f, 0x80 + condition.code()], label)
    }

    pub(crate) fn ret(&mut self) -> Result<(), EmitError> {
        self.with_rollback(|out| out.emit_u8(0xc3))
    }

    fn reg_reg(
        &mut self,
        opcode: &[u8],
        wide: bool,
        reg: Gpr,
        rm: Gpr,
        force_rex: bool,
    ) -> Result<(), EmitError> {
        self.with_rollback(|out| {
            out.emit_rex(wide, reg.high(), false, rm.high(), force_rex)?;
            out.emit_bytes(opcode)?;
            out.emit_modrm(3, reg.low(), rm.low())
        })
    }

    fn reg_mem(
        &mut self,
        opcode: &[u8],
        wide: bool,
        reg: Gpr,
        memory: Mem,
        force_rex: bool,
    ) -> Result<(), EmitError> {
        self.with_rollback(|out| out.emit_reg_mem_body(opcode, wide, reg, memory, force_rex))
    }

    fn emit_reg_mem_body(
        &mut self,
        opcode: &[u8],
        wide: bool,
        reg: Gpr,
        memory: Mem,
        force_rex: bool,
    ) -> Result<(), EmitError> {
        if memory.index.is_some_and(|(index, _)| index == Gpr::Rsp) {
            return Err(EmitError::InvalidOperand("RSP cannot be a SIB index"));
        }
        let index_high = memory.index.is_some_and(|(index, _)| index.high());
        self.emit_rex(wide, reg.high(), index_high, memory.base.high(), force_rex)?;
        self.emit_bytes(opcode)?;
        self.emit_memory_operand(reg.low(), memory)
    }

    fn group_imm32(&mut self, dst: Gpr, group: u8, value: i32) -> Result<(), EmitError> {
        self.with_rollback(|out| {
            out.emit_rex(false, false, false, dst.high(), false)?;
            out.emit_u8(0x81)?;
            out.emit_modrm(3, group, dst.low())?;
            out.emit_bytes(&value.to_le_bytes())
        })
    }

    fn group_shift_imm8(&mut self, dst: Gpr, group: u8, value: u8) -> Result<(), EmitError> {
        self.with_rollback(|out| {
            out.emit_rex(false, false, false, dst.high(), false)?;
            out.emit_u8(0xc1)?;
            out.emit_modrm(3, group, dst.low())?;
            out.emit_u8(value)
        })
    }

    fn group_register(&mut self, register: Gpr, opcode: &[u8], group: u8) -> Result<(), EmitError> {
        self.with_rollback(|out| {
            out.emit_rex(false, false, false, register.high(), false)?;
            out.emit_bytes(opcode)?;
            out.emit_modrm(3, group, register.low())
        })
    }

    fn relative_branch(&mut self, opcode: &[u8], label: Label) -> Result<(), EmitError> {
        if label.0 as usize >= self.labels.len() {
            return Err(EmitError::InvalidOperand("unknown label"));
        }
        self.with_rollback(|out| {
            if out.fixups.len() == out.control_capacity {
                return Err(EmitError::ControlCapacity {
                    limit: out.control_capacity,
                });
            }
            out.emit_bytes(opcode)?;
            let displacement_offset = out.bytes.len();
            out.emit_bytes(&[0; 4])?;
            out.fixups.push(Fixup {
                label,
                displacement_offset,
                instruction_end: out.bytes.len(),
            });
            Ok(())
        })
    }

    fn emit_memory_operand(&mut self, reg: u8, memory: Mem) -> Result<(), EmitError> {
        let base = memory.base.low();
        let needs_sib = memory.index.is_some() || base == 4;
        let (mode, displacement_size) = if memory.disp == 0 && base != 5 {
            (0, 0)
        } else if i8::try_from(memory.disp).is_ok() {
            (1, 1)
        } else {
            (2, 4)
        };
        self.emit_modrm(mode, reg, if needs_sib { 4 } else { base })?;
        if needs_sib {
            let (index, scale) = memory
                .index
                .map(|(index, scale)| (index.low(), scale.bits()))
                .unwrap_or((4, 0));
            self.emit_u8((scale << 6) | (index << 3) | base)?;
        }
        match displacement_size {
            0 => Ok(()),
            1 => self.emit_u8(memory.disp as i8 as u8),
            4 => self.emit_bytes(&memory.disp.to_le_bytes()),
            _ => unreachable!(),
        }
    }

    fn emit_rex(
        &mut self,
        wide: bool,
        reg_high: bool,
        index_high: bool,
        base_high: bool,
        force: bool,
    ) -> Result<(), EmitError> {
        let rex = 0x40
            | (u8::from(wide) << 3)
            | (u8::from(reg_high) << 2)
            | (u8::from(index_high) << 1)
            | u8::from(base_high);
        if rex != 0x40 || force {
            self.emit_u8(rex)?;
        }
        Ok(())
    }

    fn emit_modrm(&mut self, mode: u8, reg: u8, rm: u8) -> Result<(), EmitError> {
        self.emit_u8((mode << 6) | ((reg & 7) << 3) | (rm & 7))
    }

    fn emit_u8(&mut self, byte: u8) -> Result<(), EmitError> {
        self.ensure_capacity(1)?;
        self.bytes.push(byte);
        Ok(())
    }

    fn emit_bytes(&mut self, bytes: &[u8]) -> Result<(), EmitError> {
        self.ensure_capacity(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn ensure_capacity(&self, additional: usize) -> Result<(), EmitError> {
        if additional > self.capacity.saturating_sub(self.bytes.len()) {
            return Err(EmitError::Capacity {
                limit: self.capacity,
            });
        }
        Ok(())
    }

    fn with_rollback(
        &mut self,
        emit: impl FnOnce(&mut Self) -> Result<(), EmitError>,
    ) -> Result<(), EmitError> {
        let byte_len = self.bytes.len();
        let fixup_len = self.fixups.len();
        match emit(self) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.bytes.truncate(byte_len);
                self.fixups.truncate(fixup_len);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Condition, EmitError, Gpr, Mem, Scale, X64Emitter};

    #[test]
    fn reset_reuses_every_bounded_buffer() {
        let mut out = X64Emitter::new(128, 16).unwrap();
        let done = out.new_label().unwrap();
        out.jmp(done).unwrap();
        out.bind(done).unwrap();
        out.ret().unwrap();
        assert_eq!(out.finish().unwrap(), &[0xe9, 0, 0, 0, 0, 0xc3]);

        let capacities = out.buffer_capacities();
        out.reset();
        out.mov_r32_imm32(Gpr::Rax, 7).unwrap();
        out.ret().unwrap();
        assert_eq!(out.finish().unwrap(), &[0xb8, 7, 0, 0, 0, 0xc3]);
        assert_eq!(out.buffer_capacities(), capacities);
    }

    #[test]
    fn encodes_high_register_memory_and_forward_branch() {
        let mut out = X64Emitter::new(128, 16).unwrap();
        let done = out.new_label().unwrap();
        out.mov_r32_m32(Gpr::R8, Mem::base_disp(Gpr::R15, 12))
            .unwrap();
        out.test_r32_r32(Gpr::R8, Gpr::R8).unwrap();
        out.jcc(Condition::Equal, done).unwrap();
        out.add_r32_imm32(Gpr::R8, 7).unwrap();
        out.bind(done).unwrap();
        out.ret().unwrap();

        assert_eq!(
            out.finish().unwrap(),
            [
                0x45, 0x8b, 0x47, 0x0c, 0x45, 0x85, 0xc0, 0x0f, 0x84, 0x07, 0x00, 0x00, 0x00, 0x41,
                0x81, 0xc0, 0x07, 0x00, 0x00, 0x00, 0xc3,
            ]
        );
    }

    #[test]
    fn unresolved_label_is_rejected() {
        let mut out = X64Emitter::new(32, 16).unwrap();
        let label = out.new_label().unwrap();
        out.jmp(label).unwrap();

        assert!(matches!(
            out.finish(),
            Err(EmitError::UnboundLabel { label: 0 })
        ));
    }

    #[test]
    fn capacity_is_checked_before_mutation() {
        let mut out = X64Emitter::new(1, 16).unwrap();
        out.ret().unwrap();

        assert_eq!(out.bytes(), &[0xc3]);
        assert_eq!(out.ret(), Err(EmitError::Capacity { limit: 1 }));
        assert_eq!(out.bytes(), &[0xc3]);
    }

    #[test]
    fn encodes_extended_sib_and_signed_disp8() {
        let mut out = X64Emitter::new(16, 16).unwrap();
        out.mov_r32_m32(
            Gpr::R9,
            Mem::base_index_disp(Gpr::R12, Gpr::R10, Scale::Four, -16),
        )
        .unwrap();

        assert_eq!(out.finish().unwrap(), [0x47, 0x8b, 0x4c, 0x94, 0xf0]);
    }

    #[test]
    fn emits_rex_for_sil_byte_register() {
        let mut out = X64Emitter::new(16, 16).unwrap();
        out.mov_m8_r8(Mem::base_disp(Gpr::Rax, 0), Gpr::Rsi)
            .unwrap();

        assert_eq!(out.finish().unwrap(), [0x40, 0x88, 0x30]);
    }

    #[test]
    fn patches_backward_branch() {
        let mut out = X64Emitter::new(16, 16).unwrap();
        let loop_head = out.new_label().unwrap();
        out.bind(loop_head).unwrap();
        out.ret().unwrap();
        out.jmp(loop_head).unwrap();

        assert_eq!(out.finish().unwrap(), [0xc3, 0xe9, 0xfa, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn duplicate_label_and_invalid_sib_are_rejected_without_output() {
        let mut out = X64Emitter::new(16, 16).unwrap();
        let label = out.new_label().unwrap();
        out.bind(label).unwrap();
        assert_eq!(out.bind(label), Err(EmitError::DuplicateLabel { label: 0 }));
        assert_eq!(
            out.mov_r32_m32(
                Gpr::Rax,
                Mem::base_index_disp(Gpr::Rbx, Gpr::Rsp, Scale::One, 0),
            ),
            Err(EmitError::InvalidOperand("RSP cannot be a SIB index"))
        );
        assert!(out.bytes().is_empty());
    }

    #[test]
    fn encodes_move_stack_and_addressing_surface() {
        let mut out = X64Emitter::new(128, 16).unwrap();
        out.push(Gpr::Rax).unwrap();
        out.push(Gpr::R12).unwrap();
        out.pop(Gpr::R12).unwrap();
        out.mov_r32_imm32(Gpr::Rax, 0x1234_5678).unwrap();
        out.mov_r64_imm64(Gpr::R9, 0x0102_0304_0506_0708).unwrap();
        out.mov_r64_r64(Gpr::R9, Gpr::R10).unwrap();
        out.mov_r64_m64(Gpr::Rax, Mem::base_disp(Gpr::Rbp, 0))
            .unwrap();
        out.mov_m16_r16(Mem::base_disp(Gpr::R12, 8), Gpr::R9)
            .unwrap();
        out.movzx_r32_m8(Gpr::Rax, Mem::base_disp(Gpr::Rax, 0))
            .unwrap();
        out.movsx_r32_m16(Gpr::R8, Mem::base_disp(Gpr::R9, 4))
            .unwrap();
        out.lea_r64_mem(
            Gpr::R10,
            Mem::base_index_disp(Gpr::R11, Gpr::Rsi, Scale::Eight, 0x1234),
        )
        .unwrap();

        assert_eq!(
            out.finish().unwrap(),
            [
                0x50, 0x41, 0x54, 0x41, 0x5c, 0xb8, 0x78, 0x56, 0x34, 0x12, 0x49, 0xb9, 0x08, 0x07,
                0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x4d, 0x89, 0xd1, 0x48, 0x8b, 0x45, 0x00, 0x66,
                0x45, 0x89, 0x4c, 0x24, 0x08, 0x0f, 0xb6, 0x00, 0x45, 0x0f, 0xbf, 0x41, 0x04, 0x4d,
                0x8d, 0x94, 0xf3, 0x34, 0x12, 0x00, 0x00,
            ]
        );
    }

    #[test]
    fn encodes_arithmetic_shift_condition_and_multiply_surface() {
        let mut out = X64Emitter::new(128, 16).unwrap();
        out.add_r32_r32(Gpr::Rax, Gpr::Rcx).unwrap();
        out.sub_r32_r32(Gpr::R8, Gpr::R9).unwrap();
        out.and_r32_r32(Gpr::Rbx, Gpr::Rdx).unwrap();
        out.or_r32_r32(Gpr::Rsi, Gpr::Rdi).unwrap();
        out.xor_r32_r32(Gpr::R10, Gpr::R11).unwrap();
        out.cmp_r32_imm32(Gpr::Rax, -7).unwrap();
        out.shl_r32_imm8(Gpr::R8, 5).unwrap();
        out.shr_r32_cl(Gpr::Rbx).unwrap();
        out.sar_r32_cl(Gpr::R9).unwrap();
        out.test_r32_r32(Gpr::Rax, Gpr::Rbx).unwrap();
        out.setcc_r8(Condition::Less, Gpr::Rsi).unwrap();
        out.cmovcc_r32_r32(Condition::AboveEqual, Gpr::R8, Gpr::R9)
            .unwrap();
        out.imul_r32_r32(Gpr::R10, Gpr::R11).unwrap();
        out.mul_r32(Gpr::R8).unwrap();
        out.imul_r32(Gpr::R9).unwrap();
        out.div_r32(Gpr::R10).unwrap();
        out.idiv_r32(Gpr::R11).unwrap();
        out.cdq().unwrap();

        assert_eq!(
            out.finish().unwrap(),
            [
                0x01, 0xc8, 0x45, 0x29, 0xc8, 0x21, 0xd3, 0x09, 0xfe, 0x45, 0x31, 0xda, 0x81, 0xf8,
                0xf9, 0xff, 0xff, 0xff, 0x41, 0xc1, 0xe0, 0x05, 0xd3, 0xeb, 0x41, 0xd3, 0xf9, 0x85,
                0xd8, 0x40, 0x0f, 0x9c, 0xc6, 0x45, 0x0f, 0x43, 0xc1, 0x45, 0x0f, 0xaf, 0xd3, 0x41,
                0xf7, 0xe0, 0x41, 0xf7, 0xe9, 0x41, 0xf7, 0xf2, 0x41, 0xf7, 0xfb, 0x99,
            ]
        );
    }
}
