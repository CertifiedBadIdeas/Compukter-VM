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
    reason = "the direct DBT machine dispatcher consumes lowering in a later issue #498 task"
)]

use super::emitter::{Condition, EmitError, Gpr, Mem, X64Emitter};
use super::register_cache::RegisterCache;
use crate::rv32_dbt::abi::{DbtContext, DbtExitRecord, DbtExitTag};
use crate::rv32_dbt::block::{CompiledBlock, DbtBlockInput, DbtBlockMode};
use crate::rv32_dbt::{DbtFault, DbtFaultKind};
use crate::rv32im::{
    Branch, DecodedInstruction, ImmOp, Op, Rv32ArchitecturalState, Rv32ResolvedInstruction,
};

pub(crate) fn lower_block(
    input: &DbtBlockInput,
    code_capacity: usize,
) -> Result<CompiledBlock, DbtFault> {
    let mut out = X64Emitter::new(code_capacity)
        .map_err(|error| emit_fault(input.start_pc(), None, error))?;
    emit_prologue(&mut out).map_err(|error| emit_fault(input.start_pc(), None, error))?;
    let mut cache = RegisterCache::new();
    let mut terminal = None;
    let mut emitted_terminal = false;
    let bounded_limit = match input.mode() {
        DbtBlockMode::Fast => None,
        DbtBlockMode::Bounded { max_attempts } => Some(max_attempts as usize),
    };
    for (index, slot) in input.slots().iter().copied().enumerate() {
        let pc = input.start_pc().wrapping_add(index as u32 * 4);
        if bounded_limit == Some(index) {
            emit_exit(
                &mut cache,
                &mut out,
                DbtExitTag::BudgetExhausted,
                pc,
                index as u32,
                0,
                0,
            )
            .map_err(|error| emit_fault(pc, None, error))?;
            emitted_terminal = true;
            break;
        }
        let Rv32ResolvedInstruction::Valid { word, instruction } = slot else {
            terminal = Some((pc, 0, index as u32 + 1));
            break;
        };
        let attempted = index as u32 + 1;
        match instruction {
            DecodedInstruction::Branch {
                kind,
                rs1,
                rs2,
                offset,
            } => {
                emit_branch(
                    kind,
                    rs1,
                    rs2,
                    offset,
                    pc,
                    word,
                    attempted,
                    &input.slots()[index + 1..],
                    &mut cache,
                    &mut out,
                )
                .map_err(|error| emit_fault(pc, Some(word), error))?;
                emitted_terminal = true;
                break;
            }
            DecodedInstruction::Jal { rd, offset } => {
                emit_jal(rd, offset, pc, word, attempted, &mut cache, &mut out)
                    .map_err(|error| emit_fault(pc, Some(word), error))?;
                emitted_terminal = true;
                break;
            }
            DecodedInstruction::Jalr { rd, rs1, immediate } => {
                emit_jalr(
                    rd,
                    rs1,
                    immediate,
                    pc,
                    word,
                    attempted,
                    &input.slots()[index + 1..],
                    &mut cache,
                    &mut out,
                )
                .map_err(|error| emit_fault(pc, Some(word), error))?;
                emitted_terminal = true;
                break;
            }
            _ => {}
        }
        if !lower_instruction(
            instruction,
            pc,
            &input.slots()[index + 1..],
            &mut cache,
            &mut out,
        )
        .map_err(|error| emit_fault(pc, Some(word), error))?
        {
            terminal = Some((pc, word, index as u32 + 1));
            break;
        }
    }
    if !emitted_terminal {
        if let Some((pc, word, attempted)) = terminal {
            emit_exit(
                &mut cache,
                &mut out,
                DbtExitTag::SlowInstruction,
                pc,
                attempted,
                pc,
                word,
            )
            .map_err(|error| emit_fault(pc, Some(word), error))?;
        } else {
            let next_pc = input
                .start_pc()
                .wrapping_add(input.slots().len() as u32 * 4);
            emit_exit(
                &mut cache,
                &mut out,
                DbtExitTag::Completed,
                next_pc,
                input.slots().len() as u32,
                0,
                0,
            )
            .map_err(|error| emit_fault(input.start_pc(), None, error))?;
        }
    }
    let code = out
        .finish()
        .map_err(|error| emit_fault(input.start_pc(), None, error))?;
    CompiledBlock::new(input, code)
        .map_err(|message| fault(DbtFaultKind::Translation, input.start_pc(), None, message))
}

#[allow(clippy::too_many_arguments)]
fn emit_branch(
    kind: Branch,
    rs1: usize,
    rs2: usize,
    offset: i32,
    pc: u32,
    word: u32,
    attempted: u32,
    remaining: &[Rv32ResolvedInstruction],
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    let lhs = cache.read(rs1, remaining, &[], out)?;
    let rhs = cache.read(rs2, remaining, &[lhs], out)?;
    out.cmp_r32_r32(lhs, rhs)?;
    let taken = out.new_label();
    let condition = match kind {
        Branch::Eq => Condition::Equal,
        Branch::Ne => Condition::NotEqual,
        Branch::Lt => Condition::Less,
        Branch::Ge => Condition::GreaterEqual,
        Branch::Ltu => Condition::Below,
        Branch::Geu => Condition::AboveEqual,
    };
    out.jcc(condition, taken)?;
    let mut fallthrough_cache = cache.clone();
    emit_exit(
        &mut fallthrough_cache,
        out,
        DbtExitTag::Completed,
        pc.wrapping_add(4),
        attempted,
        0,
        0,
    )?;
    out.bind(taken)?;
    let target = pc.wrapping_add_signed(offset);
    let mut taken_cache = cache.clone();
    if target & 3 == 0 {
        emit_exit(
            &mut taken_cache,
            out,
            DbtExitTag::Completed,
            target,
            attempted,
            0,
            0,
        )
    } else {
        emit_exit(
            &mut taken_cache,
            out,
            DbtExitTag::SlowInstruction,
            pc,
            attempted,
            pc,
            word,
        )
    }
}

fn emit_jal(
    rd: usize,
    offset: i32,
    pc: u32,
    word: u32,
    attempted: u32,
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    let target = pc.wrapping_add_signed(offset);
    if target & 3 != 0 {
        return emit_exit(
            cache,
            out,
            DbtExitTag::SlowInstruction,
            pc,
            attempted,
            pc,
            word,
        );
    }
    if let Some(dst) = cache.write(rd, &[], &[], out)? {
        out.mov_r32_imm32(dst, pc.wrapping_add(4))?;
    }
    emit_exit(cache, out, DbtExitTag::Completed, target, attempted, 0, 0)
}

#[allow(clippy::too_many_arguments)]
fn emit_jalr(
    rd: usize,
    rs1: usize,
    immediate: i32,
    pc: u32,
    word: u32,
    attempted: u32,
    remaining: &[Rv32ResolvedInstruction],
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    let base = cache.read(rs1, remaining, &[], out)?;
    out.mov_r32_r32(Gpr::Rax, base)?;
    out.add_r32_imm32(Gpr::Rax, immediate)?;
    out.and_r32_imm32(Gpr::Rax, -2)?;
    out.mov_r32_r32(Gpr::Rcx, Gpr::Rax)?;
    out.and_r32_imm32(Gpr::Rcx, 3)?;
    out.test_r32_r32(Gpr::Rcx, Gpr::Rcx)?;
    let aligned = out.new_label();
    out.jcc(Condition::Equal, aligned)?;
    let mut misaligned_cache = cache.clone();
    emit_exit(
        &mut misaligned_cache,
        out,
        DbtExitTag::SlowInstruction,
        pc,
        attempted,
        pc,
        word,
    )?;
    out.bind(aligned)?;
    if let Some(dst) = cache.write(rd, remaining, &[base], out)? {
        out.mov_r32_imm32(dst, pc.wrapping_add(4))?;
    }
    out.mov_r32_r32(Gpr::Rdx, Gpr::Rax)?;
    emit_exit_dynamic(cache, out, DbtExitTag::Completed, Gpr::Rdx, attempted, 0, 0)
}

fn emit_prologue(out: &mut X64Emitter) -> Result<(), EmitError> {
    for register in [Gpr::Rbx, Gpr::Rbp, Gpr::R12, Gpr::R13, Gpr::R14, Gpr::R15] {
        out.push(register)?;
    }
    out.mov_r64_r64(Gpr::R15, Gpr::Rdi)?;
    out.mov_r64_m64(
        Gpr::R14,
        Mem::base_disp(Gpr::R15, DbtContext::STATE_OFFSET as i32),
    )?;
    out.mov_r64_m64(
        Gpr::R13,
        Mem::base_disp(Gpr::R15, DbtContext::RAM_BASE_OFFSET as i32),
    )?;
    out.mov_r64_m64(
        Gpr::R12,
        Mem::base_disp(Gpr::R15, DbtContext::PAGE_PERMISSIONS_OFFSET as i32),
    )
}

fn lower_instruction(
    instruction: DecodedInstruction,
    pc: u32,
    remaining: &[Rv32ResolvedInstruction],
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
) -> Result<bool, EmitError> {
    match instruction {
        DecodedInstruction::Lui { rd, value } => {
            if let Some(dst) = cache.write(rd, remaining, &[], out)? {
                out.mov_r32_imm32(dst, value)?;
            }
        }
        DecodedInstruction::Auipc { rd, value } => {
            if let Some(dst) = cache.write(rd, remaining, &[], out)? {
                out.mov_r32_imm32(dst, pc.wrapping_add(value))?;
            }
        }
        DecodedInstruction::Immediate {
            op,
            rd,
            rs1,
            immediate,
        } => lower_immediate(op, rd, rs1, immediate, remaining, cache, out)?,
        DecodedInstruction::Register { op, rd, rs1, rs2 }
            if matches!(
                op,
                Op::Add
                    | Op::Sub
                    | Op::Sll
                    | Op::Slt
                    | Op::Sltu
                    | Op::Xor
                    | Op::Srl
                    | Op::Sra
                    | Op::Or
                    | Op::And
            ) =>
        {
            lower_register(op, rd, rs1, rs2, remaining, cache, out)?
        }
        DecodedInstruction::Register { op, rd, rs1, rs2 } => {
            lower_rv32m(op, rd, rs1, rs2, remaining, cache, out)?
        }
        DecodedInstruction::Fence => {}
        _ => return Ok(false),
    }
    Ok(true)
}

fn lower_immediate(
    op: ImmOp,
    rd: usize,
    rs1: usize,
    immediate: i32,
    remaining: &[Rv32ResolvedInstruction],
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    if rd == 0 {
        return Ok(());
    }
    let src = cache.read(rs1, remaining, &[], out)?;
    let dst = cache.write(rd, remaining, &[src], out)?.unwrap();
    match op {
        ImmOp::Slt | ImmOp::Sltu => {
            out.cmp_r32_imm32(src, immediate)?;
            out.setcc_r8(
                if op == ImmOp::Slt {
                    Condition::Less
                } else {
                    Condition::Below
                },
                Gpr::Rax,
            )?;
            out.movzx_r32_r8(dst, Gpr::Rax)?;
        }
        _ => {
            if dst != src {
                out.mov_r32_r32(dst, src)?;
            }
            match op {
                ImmOp::Add => out.add_r32_imm32(dst, immediate)?,
                ImmOp::Xor => out.xor_r32_imm32(dst, immediate)?,
                ImmOp::Or => out.or_r32_imm32(dst, immediate)?,
                ImmOp::And => out.and_r32_imm32(dst, immediate)?,
                ImmOp::Sll => out.shl_r32_imm8(dst, immediate as u8 & 31)?,
                ImmOp::Srl => out.shr_r32_imm8(dst, immediate as u8 & 31)?,
                ImmOp::Sra => out.sar_r32_imm8(dst, immediate as u8 & 31)?,
                ImmOp::Slt | ImmOp::Sltu => unreachable!(),
            }
        }
    }
    Ok(())
}

fn lower_register(
    op: Op,
    rd: usize,
    rs1: usize,
    rs2: usize,
    remaining: &[Rv32ResolvedInstruction],
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    if rd == 0 {
        return Ok(());
    }
    let lhs = cache.read(rs1, remaining, &[], out)?;
    let rhs = cache.read(rs2, remaining, &[lhs], out)?;
    let dst = cache.write(rd, remaining, &[lhs, rhs], out)?.unwrap();
    if matches!(op, Op::Slt | Op::Sltu) {
        out.cmp_r32_r32(lhs, rhs)?;
        out.setcc_r8(
            if op == Op::Slt {
                Condition::Less
            } else {
                Condition::Below
            },
            Gpr::Rax,
        )?;
        out.movzx_r32_r8(dst, Gpr::Rax)?;
        return Ok(());
    }
    if matches!(op, Op::Sll | Op::Srl | Op::Sra) {
        out.mov_r32_r32(Gpr::Rcx, rhs)?;
        if dst == rhs && dst != lhs {
            out.mov_r32_r32(Gpr::Rax, lhs)?;
            emit_cl_shift(op, Gpr::Rax, out)?;
            out.mov_r32_r32(dst, Gpr::Rax)?;
        } else {
            if dst != lhs {
                out.mov_r32_r32(dst, lhs)?;
            }
            emit_cl_shift(op, dst, out)?;
        }
        return Ok(());
    }
    if dst == rhs && dst != lhs {
        out.mov_r32_r32(Gpr::Rax, lhs)?;
        emit_binary(op, Gpr::Rax, rhs, out)?;
        out.mov_r32_r32(dst, Gpr::Rax)?;
    } else {
        if dst != lhs {
            out.mov_r32_r32(dst, lhs)?;
        }
        emit_binary(op, dst, rhs, out)?;
    }
    Ok(())
}

fn emit_binary(op: Op, dst: Gpr, rhs: Gpr, out: &mut X64Emitter) -> Result<(), EmitError> {
    match op {
        Op::Add => out.add_r32_r32(dst, rhs),
        Op::Sub => out.sub_r32_r32(dst, rhs),
        Op::Xor => out.xor_r32_r32(dst, rhs),
        Op::Or => out.or_r32_r32(dst, rhs),
        Op::And => out.and_r32_r32(dst, rhs),
        _ => Err(EmitError::InvalidOperand("non-binary RV32 operation")),
    }
}

fn emit_cl_shift(op: Op, dst: Gpr, out: &mut X64Emitter) -> Result<(), EmitError> {
    match op {
        Op::Sll => out.shl_r32_cl(dst),
        Op::Srl => out.shr_r32_cl(dst),
        Op::Sra => out.sar_r32_cl(dst),
        _ => Err(EmitError::InvalidOperand("non-shift RV32 operation")),
    }
}

fn lower_rv32m(
    op: Op,
    rd: usize,
    rs1: usize,
    rs2: usize,
    remaining: &[Rv32ResolvedInstruction],
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    if rd == 0 {
        return Ok(());
    }
    if matches!(op, Op::Mul | Op::Mulh | Op::Mulhsu | Op::Mulhu) && (rs1 == 0 || rs2 == 0) {
        return write_rv32m_constant(rd, 0, remaining, cache, out);
    }
    if matches!(op, Op::Div | Op::Divu) && rs2 == 0 {
        return write_rv32m_constant(rd, u32::MAX, remaining, cache, out);
    }
    if matches!(op, Op::Rem | Op::Remu) && rs2 == 0 {
        if rs1 == 0 {
            return write_rv32m_constant(rd, 0, remaining, cache, out);
        }
        let lhs = cache.read(rs1, remaining, &[], out)?;
        let dst = cache.write(rd, remaining, &[lhs], out)?.unwrap();
        if dst != lhs {
            out.mov_r32_r32(dst, lhs)?;
        }
        return Ok(());
    }
    if matches!(op, Op::Div | Op::Divu | Op::Rem | Op::Remu) && rs1 == 0 {
        return write_rv32m_constant(rd, 0, remaining, cache, out);
    }

    let lhs = cache.read(rs1, remaining, &[], out)?;
    let rhs = cache.read(rs2, remaining, &[lhs], out)?;
    let dst = cache.write(rd, remaining, &[lhs, rhs], out)?.unwrap();
    match op {
        Op::Mul => {
            out.mov_r32_r32(Gpr::Rax, lhs)?;
            out.imul_r32_r32(Gpr::Rax, rhs)?;
        }
        Op::Mulh => {
            out.mov_r32_r32(Gpr::Rax, lhs)?;
            out.imul_r32(rhs)?;
            out.mov_r32_r32(Gpr::Rax, Gpr::Rdx)?;
        }
        Op::Mulhu => {
            out.mov_r32_r32(Gpr::Rax, lhs)?;
            out.mul_r32(rhs)?;
            out.mov_r32_r32(Gpr::Rax, Gpr::Rdx)?;
        }
        Op::Mulhsu => {
            out.mov_r32_r32(Gpr::Rcx, lhs)?;
            out.mov_r32_r32(Gpr::Rax, lhs)?;
            out.mul_r32(rhs)?;
            out.test_r32_r32(Gpr::Rcx, Gpr::Rcx)?;
            let nonnegative = out.new_label();
            out.jcc(Condition::GreaterEqual, nonnegative)?;
            out.sub_r32_r32(Gpr::Rdx, rhs)?;
            out.bind(nonnegative)?;
            out.mov_r32_r32(Gpr::Rax, Gpr::Rdx)?;
        }
        Op::Div | Op::Rem => emit_signed_division(op, lhs, rhs, out)?,
        Op::Divu | Op::Remu => emit_unsigned_division(op, lhs, rhs, out)?,
        _ => return Err(EmitError::InvalidOperand("non-RV32M operation")),
    }
    if dst != Gpr::Rax {
        out.mov_r32_r32(dst, Gpr::Rax)?;
    }
    Ok(())
}

fn write_rv32m_constant(
    rd: usize,
    value: u32,
    remaining: &[Rv32ResolvedInstruction],
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    let dst = cache.write(rd, remaining, &[], out)?.unwrap();
    out.mov_r32_imm32(dst, value)
}

fn emit_unsigned_division(
    op: Op,
    lhs: Gpr,
    rhs: Gpr,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    out.test_r32_r32(rhs, rhs)?;
    let nonzero = out.new_label();
    let done = out.new_label();
    out.jcc(Condition::NotEqual, nonzero)?;
    if op == Op::Divu {
        out.mov_r32_imm32(Gpr::Rax, u32::MAX)?;
    } else {
        out.mov_r32_r32(Gpr::Rax, lhs)?;
    }
    out.jmp(done)?;
    out.bind(nonzero)?;
    out.xor_r32_r32(Gpr::Rdx, Gpr::Rdx)?;
    out.mov_r32_r32(Gpr::Rax, lhs)?;
    out.div_r32(rhs)?;
    if op == Op::Remu {
        out.mov_r32_r32(Gpr::Rax, Gpr::Rdx)?;
    }
    out.bind(done)
}

fn emit_signed_division(op: Op, lhs: Gpr, rhs: Gpr, out: &mut X64Emitter) -> Result<(), EmitError> {
    out.test_r32_r32(rhs, rhs)?;
    let nonzero = out.new_label();
    let regular = out.new_label();
    let done = out.new_label();
    out.jcc(Condition::NotEqual, nonzero)?;
    if op == Op::Div {
        out.mov_r32_imm32(Gpr::Rax, u32::MAX)?;
    } else {
        out.mov_r32_r32(Gpr::Rax, lhs)?;
    }
    out.jmp(done)?;
    out.bind(nonzero)?;
    out.cmp_r32_imm32(lhs, i32::MIN)?;
    out.jcc(Condition::NotEqual, regular)?;
    out.cmp_r32_imm32(rhs, -1)?;
    out.jcc(Condition::NotEqual, regular)?;
    out.mov_r32_imm32(Gpr::Rax, if op == Op::Div { i32::MIN as u32 } else { 0 })?;
    out.jmp(done)?;
    out.bind(regular)?;
    out.mov_r32_r32(Gpr::Rax, lhs)?;
    out.cdq()?;
    out.idiv_r32(rhs)?;
    if op == Op::Rem {
        out.mov_r32_r32(Gpr::Rax, Gpr::Rdx)?;
    }
    out.bind(done)
}

fn emit_exit(
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
    tag: DbtExitTag,
    next_pc: u32,
    attempted: u32,
    instruction_pc: u32,
    instruction_word: u32,
) -> Result<(), EmitError> {
    cache.flush(out)?;
    write_u32(out, Gpr::R14, Rv32ArchitecturalState::register_offset(0), 0)?;
    write_u32(out, Gpr::R14, Rv32ArchitecturalState::PC_OFFSET, next_pc)?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::NEXT_PC_OFFSET,
        next_pc,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::ATTEMPTED_OFFSET,
        attempted,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::INSTRUCTION_PC_OFFSET,
        instruction_pc,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::INSTRUCTION_WORD_OFFSET,
        instruction_word,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::ADDRESS_OFFSET,
        0,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::ACCESS_SIZE_OFFSET,
        0,
    )?;
    emit_epilogue(out, tag)
}

fn emit_exit_dynamic(
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
    tag: DbtExitTag,
    next_pc: Gpr,
    attempted: u32,
    instruction_pc: u32,
    instruction_word: u32,
) -> Result<(), EmitError> {
    if next_pc == Gpr::Rax {
        return Err(EmitError::InvalidOperand(
            "dynamic DBT exit PC cannot use RAX scratch",
        ));
    }
    cache.flush(out)?;
    write_u32(out, Gpr::R14, Rv32ArchitecturalState::register_offset(0), 0)?;
    out.mov_m32_r32(
        Mem::base_disp(Gpr::R14, Rv32ArchitecturalState::PC_OFFSET as i32),
        next_pc,
    )?;
    out.mov_m32_r32(
        Mem::base_disp(
            Gpr::R15,
            (DbtContext::EXIT_OFFSET + DbtExitRecord::NEXT_PC_OFFSET) as i32,
        ),
        next_pc,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::ATTEMPTED_OFFSET,
        attempted,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::INSTRUCTION_PC_OFFSET,
        instruction_pc,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::INSTRUCTION_WORD_OFFSET,
        instruction_word,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::ADDRESS_OFFSET,
        0,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::ACCESS_SIZE_OFFSET,
        0,
    )?;
    emit_epilogue(out, tag)
}

fn emit_epilogue(out: &mut X64Emitter, tag: DbtExitTag) -> Result<(), EmitError> {
    for register in [Gpr::R15, Gpr::R14, Gpr::R13, Gpr::R12, Gpr::Rbp, Gpr::Rbx] {
        out.pop(register)?;
    }
    out.mov_r32_imm32(Gpr::Rax, tag as u32)?;
    out.ret()
}

fn write_u32(out: &mut X64Emitter, base: Gpr, offset: usize, value: u32) -> Result<(), EmitError> {
    out.mov_r32_imm32(Gpr::Rax, value)?;
    out.mov_m32_r32(Mem::base_disp(base, offset as i32), Gpr::Rax)
}

fn emit_fault(pc: u32, word: Option<u32>, error: EmitError) -> DbtFault {
    let kind = if matches!(error, EmitError::Capacity { .. }) {
        DbtFaultKind::Capacity
    } else {
        DbtFaultKind::Translation
    };
    fault(kind, pc, word, error.to_string())
}

fn fault(kind: DbtFaultKind, pc: u32, word: Option<u32>, message: impl Into<String>) -> DbtFault {
    DbtFault::new(kind, pc, word, message)
}

#[cfg(test)]
mod tests {
    use super::lower_block;
    use crate::memory::MachineMemory;
    use crate::rv32_dbt::abi::{DbtContext, DbtEntry, DbtExitRecord, DbtExitTag};
    use crate::rv32_dbt::block::{DbtBlockInput, DbtBlockMode};
    use crate::rv32_dbt::executable::ExecutableScratch;
    use crate::rv32im::{
        decode_product_word,
        encoding::{
            add, addi, and, andi, auipc, beq, bge, bgeu, blt, bltu, bne, div, divu, fence, fence_i,
            jal, jalr, lui, mul, mulh, mulhsu, mulhu, or, ori, rem, remu, sll, slli, slt, slti,
            sltiu, sltu, sra, srai, srl, srli, sub, xor, xori,
        },
        Rv32ResolvedInstruction, Rv32imCpu,
    };

    fn input(start_pc: u32, words: &[u32], mode: DbtBlockMode) -> DbtBlockInput {
        DbtBlockInput::new(
            start_pc,
            words
                .iter()
                .copied()
                .map(|word| Rv32ResolvedInstruction::Valid {
                    word,
                    instruction: decode_product_word(word).unwrap(),
                })
                .collect(),
            mode,
        )
        .unwrap()
    }

    fn execute(
        input: &DbtBlockInput,
        registers: &[(usize, u32)],
    ) -> (Rv32imCpu, DbtExitTag, DbtExitRecord, Vec<u8>) {
        let compiled = lower_block(input, 64 * 1024).unwrap();
        let code = compiled.code().to_vec();
        let mut scratch = ExecutableScratch::new(64 * 1024).unwrap();
        scratch.publish(&code).unwrap();
        let mut cpu = Rv32imCpu::new(input.start_pc());
        for &(register, value) in registers {
            cpu.set_register(register, value).unwrap();
        }
        let mut context = DbtContext {
            state: cpu.architectural_state_mut(),
            ram_base: std::ptr::null_mut(),
            ram_len: 0,
            page_permissions: std::ptr::null(),
            page_count: 0,
            remaining_budget: input.slots().len() as u32,
            reservation_valid: 0,
            reservation_address: 0,
            exit: DbtExitRecord::default(),
        };
        let entry: DbtEntry = unsafe { std::mem::transmute(scratch.entry_address().unwrap()) };
        let tag = DbtExitTag::try_from(unsafe { entry(&mut context) }).unwrap();
        let exit = context.exit;
        (cpu, tag, exit, code)
    }

    #[test]
    fn generated_rv32i_arithmetic_matches_canonical_execution() {
        let start_pc = 0x1000;
        let words = vec![
            lui(5, 0x81234),
            auipc(6, 0x7),
            addi(7, 1, -17),
            slti(8, 2, -3),
            sltiu(9, 3, -1),
            xori(10, 4, -2048),
            ori(11, 5, 0x155),
            andi(12, 6, -16),
            slli(13, 7, 31),
            srli(14, 4, 5),
            srai(15, 4, 7),
            add(16, 7, 8),
            sub(17, 8, 7),
            sll(18, 3, 2),
            slt(19, 2, 3),
            sltu(20, 2, 3),
            xor(21, 10, 11),
            srl(22, 4, 2),
            sra(23, 4, 2),
            or(24, 11, 12),
            and(25, 11, 12),
            sub(2, 1, 2),
            sra(3, 3, 2),
            sll(4, 0, 2),
            add(5, 1, 0),
            addi(6, 6, -1),
            add(0, 1, 2),
            fence(),
        ];
        let slots = words
            .iter()
            .copied()
            .map(|word| Rv32ResolvedInstruction::Valid {
                word,
                instruction: decode_product_word(word).unwrap(),
            })
            .collect::<Vec<_>>();
        let input = DbtBlockInput::new(start_pc, slots, DbtBlockMode::Fast).unwrap();

        let mut expected = Rv32imCpu::new(start_pc);
        let mut actual = Rv32imCpu::new(start_pc);
        for (register, value) in [
            (1, 0x8000_0011),
            (2, 0xffff_fffd),
            (3, 0x7fff_ffff),
            (4, 0x8765_4321),
        ] {
            expected.set_register(register, value).unwrap();
            actual.set_register(register, value).unwrap();
        }
        let mut bus = MachineMemory::zeroed(4).unwrap();
        for (index, slot) in input.slots().iter().copied().enumerate() {
            let Rv32ResolvedInstruction::Valid { instruction, .. } = slot else {
                unreachable!()
            };
            expected
                .execute_decoded(&mut bus, start_pc + index as u32 * 4, instruction)
                .unwrap();
            expected.commit_instruction();
        }

        let compiled = lower_block(&input, 64 * 1024).unwrap();
        let mut scratch = ExecutableScratch::new(64 * 1024).unwrap();
        scratch.publish(compiled.code()).unwrap();
        let mut context = DbtContext {
            state: actual.architectural_state_mut(),
            ram_base: std::ptr::null_mut(),
            ram_len: 0,
            page_permissions: std::ptr::null(),
            page_count: 0,
            remaining_budget: words.len() as u32,
            reservation_valid: 0,
            reservation_address: 0,
            exit: DbtExitRecord::default(),
        };
        let entry: DbtEntry = unsafe { std::mem::transmute(scratch.entry_address().unwrap()) };
        let raw_exit = unsafe { entry(&mut context) };
        actual.commit_instructions(context.exit.attempted);

        assert_eq!(DbtExitTag::try_from(raw_exit), Ok(DbtExitTag::Completed));
        assert_eq!(context.exit.next_pc, start_pc + words.len() as u32 * 4);
        assert_eq!(context.exit.attempted, words.len() as u32);
        assert_eq!(actual.architectural_state(), expected.architectural_state());
        assert_eq!(actual.register(0), 0);
    }

    #[test]
    fn fence_i_returns_a_typed_slow_exit_after_flushing_native_prefix() {
        let start_pc = 0x2000;
        let words = [addi(5, 1, 2), fence_i()];
        let slots = words
            .into_iter()
            .map(|word| Rv32ResolvedInstruction::Valid {
                word,
                instruction: decode_product_word(word).unwrap(),
            })
            .collect();
        let input = DbtBlockInput::new(start_pc, slots, DbtBlockMode::Fast).unwrap();
        let compiled = lower_block(&input, 4096).unwrap();
        let mut scratch = ExecutableScratch::new(4096).unwrap();
        scratch.publish(compiled.code()).unwrap();
        let mut cpu = Rv32imCpu::new(start_pc);
        cpu.set_register(1, 10).unwrap();
        let mut context = DbtContext {
            state: cpu.architectural_state_mut(),
            ram_base: std::ptr::null_mut(),
            ram_len: 0,
            page_permissions: std::ptr::null(),
            page_count: 0,
            remaining_budget: 2,
            reservation_valid: 0,
            reservation_address: 0,
            exit: DbtExitRecord::default(),
        };
        let entry: DbtEntry = unsafe { std::mem::transmute(scratch.entry_address().unwrap()) };

        let raw_exit = unsafe { entry(&mut context) };

        assert_eq!(
            DbtExitTag::try_from(raw_exit),
            Ok(DbtExitTag::SlowInstruction)
        );
        assert_eq!(cpu.register(5), 12);
        assert_eq!(cpu.pc(), start_pc + 4);
        assert_eq!(context.exit.attempted, 2);
        assert_eq!(context.exit.instruction_pc, start_pc + 4);
        assert_eq!(context.exit.instruction_word, fence_i());
    }

    #[test]
    fn every_branch_condition_selects_taken_and_fallthrough_pc() {
        type BranchEncoder = fn(u8, u8, i32) -> u32;
        let cases: [(BranchEncoder, (u32, u32), (u32, u32)); 6] = [
            (beq, (5, 5), (5, 6)),
            (bne, (5, 6), (5, 5)),
            (blt, (u32::MAX, 0), (0, u32::MAX)),
            (bge, (0, u32::MAX), (u32::MAX, 0)),
            (bltu, (0, u32::MAX), (u32::MAX, 0)),
            (bgeu, (u32::MAX, 0), (0, u32::MAX)),
        ];
        for (encode, taken, not_taken) in cases {
            let word = encode(1, 2, 8);
            let block = input(0x3000, &[word], DbtBlockMode::Fast);

            let (cpu, tag, exit, _) = execute(&block, &[(1, taken.0), (2, taken.1)]);
            assert_eq!(tag, DbtExitTag::Completed);
            assert_eq!(cpu.pc(), 0x3008);
            assert_eq!(exit.next_pc, 0x3008);
            assert_eq!(exit.attempted, 1);

            let (cpu, tag, exit, _) = execute(&block, &[(1, not_taken.0), (2, not_taken.1)]);
            assert_eq!(tag, DbtExitTag::Completed);
            assert_eq!(cpu.pc(), 0x3004);
            assert_eq!(exit.next_pc, 0x3004);
            assert_eq!(exit.attempted, 1);
        }
    }

    #[test]
    fn jal_and_jalr_commit_links_and_targets_but_misalignment_exits_slow() {
        let jal_block = input(0x4000, &[jal(5, 12)], DbtBlockMode::Fast);
        let (cpu, tag, exit, _) = execute(&jal_block, &[]);
        assert_eq!(tag, DbtExitTag::Completed);
        assert_eq!(cpu.register(5), 0x4004);
        assert_eq!(cpu.pc(), 0x400c);
        assert_eq!(exit.attempted, 1);

        let jalr_block = input(0x5000, &[jalr(6, 1, 5)], DbtBlockMode::Fast);
        let (cpu, tag, _, _) = execute(&jalr_block, &[(1, 0x6000)]);
        assert_eq!(tag, DbtExitTag::Completed);
        assert_eq!(cpu.register(6), 0x5004);
        assert_eq!(cpu.pc(), 0x6004);

        let misaligned_jal = input(0x7000, &[jal(7, 2)], DbtBlockMode::Fast);
        let (cpu, tag, exit, _) = execute(&misaligned_jal, &[(7, 0xaaaa_5555)]);
        assert_eq!(tag, DbtExitTag::SlowInstruction);
        assert_eq!(cpu.register(7), 0xaaaa_5555);
        assert_eq!(cpu.pc(), 0x7000);
        assert_eq!(exit.instruction_pc, 0x7000);

        let misaligned_jalr = input(0x8000, &[jalr(8, 1, 0)], DbtBlockMode::Fast);
        let (cpu, tag, _, _) = execute(&misaligned_jalr, &[(1, 0x9003), (8, 0x1234)]);
        assert_eq!(tag, DbtExitTag::SlowInstruction);
        assert_eq!(cpu.register(8), 0x1234);
        assert_eq!(cpu.pc(), 0x8000);
    }

    #[test]
    fn final_branch_counts_the_native_prefix_and_itself() {
        let words = [addi(3, 3, 1), bne(1, 2, 8)];
        let block = input(0xa000, &words, DbtBlockMode::Fast);

        let (cpu, tag, exit, _) = execute(&block, &[(1, 1), (2, 2), (3, 9)]);

        assert_eq!(tag, DbtExitTag::Completed);
        assert_eq!(cpu.register(3), 10);
        assert_eq!(cpu.pc(), 0xa00c);
        assert_eq!(exit.attempted, 2);
    }

    #[test]
    fn every_bounded_prefix_stops_after_exactly_its_limit() {
        for length in 2_u8..=16 {
            let words = (1..=length)
                .map(|register| addi(register, register, 1))
                .collect::<Vec<_>>();
            for limit in 1_u32..u32::from(length) {
                let block = input(
                    0xb000,
                    &words,
                    DbtBlockMode::Bounded {
                        max_attempts: limit,
                    },
                );
                let initial = (1..=length)
                    .map(|register| (register as usize, 100 + u32::from(register)))
                    .collect::<Vec<_>>();

                let (cpu, tag, exit, _) = execute(&block, &initial);

                assert_eq!(tag, DbtExitTag::BudgetExhausted);
                assert_eq!(cpu.pc(), 0xb000 + limit * 4);
                assert_eq!(exit.attempted, limit);
                for register in 1..=length {
                    let expected = 100 + u32::from(register) + u32::from(register <= limit as u8);
                    assert_eq!(cpu.register(register as usize), expected);
                }
            }
        }
    }

    #[test]
    fn control_at_budget_edge_wins_and_slow_instruction_beyond_edge_is_untouched() {
        let branch_words = [addi(3, 3, 1), bne(1, 2, 8), addi(4, 4, 1)];
        let branch_block = input(
            0xc000,
            &branch_words,
            DbtBlockMode::Bounded { max_attempts: 2 },
        );
        let (cpu, tag, exit, _) = execute(&branch_block, &[(1, 1), (2, 2), (3, 9), (4, 20)]);
        assert_eq!(tag, DbtExitTag::Completed);
        assert_eq!(exit.attempted, 2);
        assert_eq!(cpu.pc(), 0xc00c);
        assert_eq!(cpu.register(3), 10);
        assert_eq!(cpu.register(4), 20);

        let slow_words = [addi(3, 3, 1), fence_i()];
        let slow_block = input(
            0xd000,
            &slow_words,
            DbtBlockMode::Bounded { max_attempts: 1 },
        );
        let (cpu, tag, exit, _) = execute(&slow_block, &[(3, 9)]);
        assert_eq!(tag, DbtExitTag::BudgetExhausted);
        assert_eq!(exit.attempted, 1);
        assert_eq!(cpu.pc(), 0xd004);
        assert_eq!(cpu.register(3), 10);
    }

    #[test]
    fn every_rv32m_operation_matches_canonical_corner_and_random_pairs() {
        type Rv32mEncoder = fn(u8, u8, u8) -> u32;
        let operations: [Rv32mEncoder; 8] = [mul, mulh, mulhsu, mulhu, div, divu, rem, remu];
        let mut pairs = vec![
            (0, 0),
            (0, 1),
            (1, 0),
            (1, 1),
            (u32::MAX, 0),
            (u32::MAX, 1),
            (i32::MIN as u32, u32::MAX),
            (i32::MIN as u32, 1),
            (i32::MAX as u32, u32::MAX),
            (i32::MAX as u32, i32::MIN as u32),
        ];
        let mut random = 0x7a31_d09f_u32;
        for _ in 0..64 {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            let lhs = random;
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            pairs.push((lhs, random));
        }

        for encode in operations {
            for &(lhs, rhs) in &pairs {
                let word = encode(3, 1, 2);
                let block = input(0xe000, &[word], DbtBlockMode::Fast);
                let (actual, tag, exit, _) =
                    execute(&block, &[(1, lhs), (2, rhs), (3, 0xfeed_face)]);
                let mut expected = Rv32imCpu::new(0xe000);
                expected.set_register(1, lhs).unwrap();
                expected.set_register(2, rhs).unwrap();
                expected.set_register(3, 0xfeed_face).unwrap();
                let mut bus = MachineMemory::zeroed(4).unwrap();
                expected
                    .execute_decoded(&mut bus, 0xe000, decode_product_word(word).unwrap())
                    .unwrap();

                assert_eq!(
                    tag,
                    DbtExitTag::Completed,
                    "word={word:#010x} lhs={lhs:#010x} rhs={rhs:#010x}"
                );
                assert_eq!(exit.attempted, 1);
                assert_eq!(actual.pc(), expected.pc());
                assert_eq!(
                    actual.register(3),
                    expected.register(3),
                    "word={word:#010x} lhs={lhs:#010x} rhs={rhs:#010x}"
                );
            }
        }
    }
}
