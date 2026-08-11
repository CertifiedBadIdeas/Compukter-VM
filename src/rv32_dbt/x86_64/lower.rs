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
    DecodedInstruction, ImmOp, Op, Rv32ArchitecturalState, Rv32ResolvedInstruction,
};

pub(crate) fn lower_block(
    input: &DbtBlockInput,
    code_capacity: usize,
) -> Result<CompiledBlock, DbtFault> {
    if input.mode() != DbtBlockMode::Fast {
        return Err(fault(
            DbtFaultKind::Translation,
            input.start_pc(),
            None,
            "bounded lowering is introduced with exact budget control",
        ));
    }
    let mut out = X64Emitter::new(code_capacity)
        .map_err(|error| emit_fault(input.start_pc(), None, error))?;
    emit_prologue(&mut out).map_err(|error| emit_fault(input.start_pc(), None, error))?;
    let mut cache = RegisterCache::new();
    let mut terminal = None;
    for (index, slot) in input.slots().iter().copied().enumerate() {
        let pc = input.start_pc().wrapping_add(index as u32 * 4);
        let Rv32ResolvedInstruction::Valid { word, instruction } = slot else {
            terminal = Some((pc, 0, index as u32 + 1));
            break;
        };
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
    let code = out
        .finish()
        .map_err(|error| emit_fault(input.start_pc(), None, error))?;
    CompiledBlock::new(input, code)
        .map_err(|message| fault(DbtFaultKind::Translation, input.start_pc(), None, message))
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
            add, addi, and, andi, auipc, fence, fence_i, lui, or, ori, sll, slli, slt, slti, sltiu,
            sltu, sra, srai, srl, srli, sub, xor, xori,
        },
        Rv32ResolvedInstruction, Rv32imCpu,
    };

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
}
