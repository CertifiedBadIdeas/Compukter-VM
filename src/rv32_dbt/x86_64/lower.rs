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
    reason = "the direct DBT machine dispatcher consumes lowering in a later issue #17 task"
)]

use super::emitter::{Condition, EmitError, Gpr, Mem, X64Emitter};
use super::register_cache::RegisterCache;
use crate::rv32_dbt::abi::{DbtContext, DbtExitRecord, DbtExitTag};
use crate::rv32_dbt::block::{
    DbtBlockInput, DbtBlockMode, DbtColdExitRelocation, DbtLinkKind, DbtStaticLink,
    TranslatedBlock, MAX_COLD_EXIT_RELOCATIONS, MAX_STATIC_LINKS,
};
use crate::rv32_dbt::{DbtFault, DbtFaultKind};
use crate::rv32im::{
    Branch, DecodedInstruction, ImmOp, Load, Op, Rv32ArchitecturalState, Rv32ResolvedInstruction,
    Store,
};

const EXECUTION_COUNTER: Gpr = Gpr::Rdi;

pub(crate) struct DbtTranslationWorkspace {
    emitter: X64Emitter,
}

struct StaticLinkCollector {
    links: [DbtStaticLink; MAX_STATIC_LINKS],
    len: usize,
}

struct ColdExitCollector {
    relocations: [DbtColdExitRelocation; MAX_COLD_EXIT_RELOCATIONS],
    len: usize,
}

impl ColdExitCollector {
    const fn new() -> Self {
        Self {
            relocations: [DbtColdExitRelocation {
                displacement_offset: 0,
            }; MAX_COLD_EXIT_RELOCATIONS],
            len: 0,
        }
    }

    fn push(&mut self, relocation: DbtColdExitRelocation) -> Result<(), EmitError> {
        let slot = self
            .relocations
            .get_mut(self.len)
            .ok_or(EmitError::InvalidOperand(
                "RV32 DBT block exceeded its cold exit capacity",
            ))?;
        *slot = relocation;
        self.len += 1;
        Ok(())
    }

    fn as_slice(&self) -> &[DbtColdExitRelocation] {
        &self.relocations[..self.len]
    }
}

impl StaticLinkCollector {
    const fn new() -> Self {
        Self {
            links: [DbtStaticLink::EMPTY; MAX_STATIC_LINKS],
            len: 0,
        }
    }

    fn push(&mut self, link: DbtStaticLink) -> Result<(), EmitError> {
        let slot = self
            .links
            .get_mut(self.len)
            .ok_or(EmitError::InvalidOperand(
                "RV32 DBT block exceeded its static link capacity",
            ))?;
        *slot = link;
        self.len += 1;
        Ok(())
    }

    fn as_slice(&self) -> &[DbtStaticLink] {
        &self.links[..self.len]
    }
}

impl DbtTranslationWorkspace {
    pub(crate) fn new(
        code_capacity: usize,
        max_block_instructions: usize,
    ) -> Result<Self, DbtFault> {
        let control_capacity = max_block_instructions.checked_mul(16).ok_or_else(|| {
            fault(
                DbtFaultKind::Capacity,
                0,
                None,
                "RV32 DBT control workspace capacity overflow",
            )
        })?;
        let emitter = X64Emitter::new(code_capacity, control_capacity)
            .map_err(|error| fault(DbtFaultKind::Capacity, 0, None, error.to_string()))?;
        Ok(Self { emitter })
    }

    pub(crate) fn lower<'a>(
        &'a mut self,
        input: &DbtBlockInput<'_>,
        ram_len: u32,
    ) -> Result<TranslatedBlock<'a>, DbtFault> {
        if ram_len == 0 {
            return Err(fault(
                DbtFaultKind::Capacity,
                input.start_pc(),
                None,
                "RV32 DBT RAM length must be positive",
            ));
        }
        self.emitter.reset();
        let mut out = &mut self.emitter;
        let chainable = matches!(input.mode(), DbtBlockMode::ChainableThroughput);
        emit_prologue(&mut out, chainable)
            .map_err(|error| emit_fault(input.start_pc(), None, error))?;
        let mut static_links = StaticLinkCollector::new();
        let mut cold_exits = ColdExitCollector::new();
        let mut cache = if chainable {
            RegisterCache::chainable()
        } else {
            RegisterCache::direct()
        };
        let chain_entry_offset = if chainable {
            emit_fast_entry_guard(
                &mut cache,
                &mut out,
                input.start_pc(),
                input.slots().len() as u32,
                &mut cold_exits,
            )
            .map_err(|error| emit_fault(input.start_pc(), None, error))?
        } else {
            u32::try_from(out.bytes().len()).map_err(|_| {
                fault(
                    DbtFaultKind::Capacity,
                    input.start_pc(),
                    None,
                    "RV32 DBT chain entry offset exceeds u32",
                )
            })?
        };
        let mut terminal = None;
        let mut emitted_terminal = false;
        let mut lowered_load_sites = 0_u32;
        let mut lowered_store_sites = 0_u32;
        let bounded_limit = match input.mode() {
            DbtBlockMode::DirectFast | DbtBlockMode::ChainableThroughput => None,
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
                let Rv32ResolvedInstruction::Invalid { word } = slot else {
                    unreachable!()
                };
                terminal = Some((pc, word, index as u32 + 1));
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
                        &input.slots()[index..],
                        &mut cache,
                        &mut out,
                        chainable,
                        &mut static_links,
                        &mut cold_exits,
                    )
                    .map_err(|error| emit_fault(pc, Some(word), error))?;
                    emitted_terminal = true;
                    break;
                }
                DecodedInstruction::Jal { rd, offset } => {
                    emit_jal(
                        rd,
                        offset,
                        pc,
                        word,
                        attempted,
                        &mut cache,
                        &mut out,
                        chainable,
                        &mut static_links,
                        &mut cold_exits,
                    )
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
                        &input.slots()[index..],
                        &mut cache,
                        &mut out,
                    )
                    .map_err(|error| emit_fault(pc, Some(word), error))?;
                    emitted_terminal = true;
                    break;
                }
                DecodedInstruction::Load {
                    kind,
                    rd,
                    rs1,
                    immediate,
                } => {
                    lowered_load_sites += 1;
                    lower_load(
                        kind,
                        rd,
                        rs1,
                        immediate,
                        pc,
                        word,
                        attempted,
                        ram_len,
                        &input.slots()[index..],
                        &mut cache,
                        &mut out,
                    )
                    .map_err(|error| emit_fault(pc, Some(word), error))?;
                    continue;
                }
                DecodedInstruction::Store {
                    kind,
                    rs1,
                    rs2,
                    immediate,
                } => {
                    lowered_store_sites += 1;
                    lower_store(
                        kind,
                        rs1,
                        rs2,
                        immediate,
                        pc,
                        word,
                        attempted,
                        ram_len,
                        &input.slots()[index..],
                        &mut cache,
                        &mut out,
                    )
                    .map_err(|error| emit_fault(pc, Some(word), error))?;
                    continue;
                }
                _ => {}
            }
            if !lower_instruction(
                instruction,
                pc,
                &input.slots()[index..],
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
                emit_completed_exit(
                    &mut cache,
                    &mut out,
                    next_pc,
                    input.slots().len() as u32,
                    DbtLinkKind::Fallthrough,
                    chainable,
                    &mut static_links,
                    &mut cold_exits,
                )
                .map_err(|error| emit_fault(input.start_pc(), None, error))?;
            }
        }
        let code = out
            .finish()
            .map_err(|error| emit_fault(input.start_pc(), None, error))?;
        TranslatedBlock::new(
            input,
            code,
            lowered_load_sites,
            lowered_store_sites,
            chain_entry_offset,
            static_links.as_slice(),
            cold_exits.as_slice(),
        )
        .map_err(|message| fault(DbtFaultKind::Translation, input.start_pc(), None, message))
    }

    #[cfg(test)]
    fn buffer_capacities(&self) -> (usize, usize, usize) {
        self.emitter.buffer_capacities()
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.emitter.retained_bytes()
    }
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
    chainable: bool,
    static_links: &mut StaticLinkCollector,
    cold_exits: &mut ColdExitCollector,
) -> Result<(), EmitError> {
    let lhs = cache.read(rs1, remaining, &[], out)?;
    let rhs = cache.read(rs2, remaining, &[lhs], out)?;
    out.cmp_r32_r32(lhs, rhs)?;
    let taken = out.new_label()?;
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
    emit_completed_exit(
        &mut fallthrough_cache,
        out,
        pc.wrapping_add(4),
        attempted,
        DbtLinkKind::BranchNotTaken,
        chainable,
        static_links,
        cold_exits,
    )?;
    out.bind(taken)?;
    let target = pc.wrapping_add_signed(offset);
    let mut taken_cache = cache.clone();
    if target & 3 == 0 {
        emit_completed_exit(
            &mut taken_cache,
            out,
            target,
            attempted,
            DbtLinkKind::BranchTaken,
            chainable,
            static_links,
            cold_exits,
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
    chainable: bool,
    static_links: &mut StaticLinkCollector,
    cold_exits: &mut ColdExitCollector,
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
    emit_completed_exit(
        cache,
        out,
        target,
        attempted,
        DbtLinkKind::Jal,
        chainable,
        static_links,
        cold_exits,
    )
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
    let aligned = out.new_label()?;
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

fn emit_prologue(out: &mut X64Emitter, chainable: bool) -> Result<(), EmitError> {
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
    )?;
    if chainable {
        out.mov_r32_m32(
            EXECUTION_COUNTER,
            Mem::base_disp(Gpr::R15, DbtContext::REMAINING_BUDGET_OFFSET as i32),
        )?;
        out.neg_r64(EXECUTION_COUNTER)?;
    }
    Ok(())
}

fn emit_fast_entry_guard(
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
    start_pc: u32,
    _instruction_count: u32,
    cold_exits: &mut ColdExitCollector,
) -> Result<u32, EmitError> {
    let guard = out.new_label()?;
    let body = out.new_label()?;
    out.jmp(guard)?;
    let chain_entry_offset =
        u32::try_from(out.bytes().len()).map_err(|_| EmitError::BranchRange)?;
    #[cfg(feature = "dbt-chain-stats")]
    add_context_u32(out, DbtContext::CHAIN_TRANSITIONS_OFFSET, 1)?;
    out.bind(guard)?;
    out.test_r64_r64(EXECUTION_COUNTER, EXECUTION_COUNTER)?;
    out.jcc(Condition::Less, body)?;
    cache.flush(out)?;
    emit_completed_trampoline(out, start_pc, cold_exits)?;
    out.bind(body)?;
    Ok(chain_entry_offset)
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

#[allow(clippy::too_many_arguments)]
fn lower_load(
    kind: Load,
    rd: usize,
    rs1: usize,
    immediate: i32,
    pc: u32,
    word: u32,
    attempted: u32,
    ram_len: u32,
    remaining: &[Rv32ResolvedInstruction],
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    let width = match kind {
        Load::Byte | Load::ByteU => 1,
        Load::Half | Load::HalfU => 2,
        Load::Word => 4,
    };
    let base = cache.read(rs1, remaining, &[], out)?;
    out.mov_r32_r32(Gpr::Rdx, base)?;
    if immediate != 0 {
        out.add_r32_imm32(Gpr::Rdx, immediate)?;
    }
    let mut slow_cache = cache.clone();
    let slow = out.new_label()?;
    let done = out.new_label()?;
    emit_ram_checks(Gpr::Rdx, width, 0b001, ram_len, slow, out)?;

    let dst = cache.write(rd, remaining, &[], out)?.unwrap_or(Gpr::Rax);
    let memory = Mem::base_index_disp(Gpr::R13, Gpr::Rdx, super::emitter::Scale::One, 0);
    match kind {
        Load::Byte => out.movsx_r32_m8(dst, memory)?,
        Load::Half => out.movsx_r32_m16(dst, memory)?,
        Load::Word => out.mov_r32_m32(dst, memory)?,
        Load::ByteU => out.movzx_r32_m8(dst, memory)?,
        Load::HalfU => out.movzx_r32_m16(dst, memory)?,
    }
    out.jmp(done)?;

    out.bind(slow)?;
    emit_memory_exit(&mut slow_cache, out, pc, word, attempted, Gpr::Rdx, width)?;
    out.bind(done)
}

#[allow(clippy::too_many_arguments)]
fn lower_store(
    kind: Store,
    rs1: usize,
    rs2: usize,
    immediate: i32,
    pc: u32,
    word: u32,
    attempted: u32,
    ram_len: u32,
    remaining: &[Rv32ResolvedInstruction],
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    let width = match kind {
        Store::Byte => 1,
        Store::Half => 2,
        Store::Word => 4,
    };
    let base = cache.read(rs1, remaining, &[], out)?;
    out.mov_r32_r32(Gpr::Rdx, base)?;
    if immediate != 0 {
        out.add_r32_imm32(Gpr::Rdx, immediate)?;
    }
    let mut slow_cache = cache.clone();
    let slow = out.new_label()?;
    let done = out.new_label()?;
    emit_ram_checks(Gpr::Rdx, width, 0b010, ram_len, slow, out)?;

    let value = cache.read(rs2, remaining, &[Gpr::Rdx], out)?;
    let memory = Mem::base_index_disp(Gpr::R13, Gpr::Rdx, super::emitter::Scale::One, 0);
    match kind {
        Store::Byte => out.mov_m8_r8(memory, value)?,
        Store::Half => out.mov_m16_r16(memory, value)?,
        Store::Word => out.mov_m32_r32(memory, value)?,
    }
    emit_store_reservation_invalidation(Gpr::Rdx, width, out)?;
    out.jmp(done)?;

    out.bind(slow)?;
    emit_memory_exit(&mut slow_cache, out, pc, word, attempted, Gpr::Rdx, width)?;
    out.bind(done)
}

fn emit_store_reservation_invalidation(
    address: Gpr,
    width: u32,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    let keep = out.new_label()?;
    out.mov_r32_m32(
        Gpr::Rcx,
        Mem::base_disp(Gpr::R15, DbtContext::RESERVATION_VALID_OFFSET as i32),
    )?;
    out.test_r32_r32(Gpr::Rcx, Gpr::Rcx)?;
    out.jcc(Condition::Equal, keep)?;
    out.mov_r32_m32(
        Gpr::Rax,
        Mem::base_disp(Gpr::R15, DbtContext::RESERVATION_ADDRESS_OFFSET as i32),
    )?;
    out.mov_r32_r32(Gpr::Rcx, Gpr::Rax)?;
    out.add_r32_imm32(Gpr::Rcx, 4)?;
    out.cmp_r32_r32(address, Gpr::Rcx)?;
    out.jcc(Condition::AboveEqual, keep)?;
    out.mov_r32_r32(Gpr::Rcx, address)?;
    out.add_r32_imm32(Gpr::Rcx, width as i32)?;
    out.cmp_r32_r32(Gpr::Rax, Gpr::Rcx)?;
    out.jcc(Condition::AboveEqual, keep)?;
    out.mov_r32_imm32(Gpr::Rax, 0)?;
    out.mov_m32_r32(
        Mem::base_disp(Gpr::R15, DbtContext::RESERVATION_VALID_OFFSET as i32),
        Gpr::Rax,
    )?;
    out.bind(keep)
}

fn emit_ram_checks(
    address: Gpr,
    width: u32,
    permission: u8,
    ram_len: u32,
    slow: super::emitter::Label,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    if width > 1 {
        out.mov_r32_r32(Gpr::Rax, address)?;
        out.and_r32_imm32(Gpr::Rax, width as i32 - 1)?;
        out.jcc(Condition::NotEqual, slow)?;
    }
    let Some(inclusive_limit) = ram_len.checked_sub(width) else {
        return out.jmp(slow);
    };
    out.cmp_r32_imm32(address, inclusive_limit as i32)?;
    out.jcc(Condition::Above, slow)?;

    out.mov_r32_r32(Gpr::Rax, address)?;
    out.shr_r32_imm8(Gpr::Rax, 12)?;
    out.test_m8_imm8(
        Mem::base_index_disp(Gpr::R12, Gpr::Rax, super::emitter::Scale::One, 0),
        permission,
    )?;
    out.jcc(Condition::Equal, slow)
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
    if dst == rhs && matches!(op, Op::Add | Op::Xor | Op::Or | Op::And) {
        emit_binary(op, dst, lhs, out)?;
    } else if dst == rhs && dst != lhs {
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
            let nonnegative = out.new_label()?;
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
    let nonzero = out.new_label()?;
    let done = out.new_label()?;
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
    let nonzero = out.new_label()?;
    let regular = out.new_label()?;
    let done = out.new_label()?;
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

fn emit_memory_exit(
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
    instruction_pc: u32,
    instruction_word: u32,
    attempted: u32,
    address: Gpr,
    access_size: u32,
) -> Result<(), EmitError> {
    cache.flush(out)?;
    write_u32(out, Gpr::R14, Rv32ArchitecturalState::register_offset(0), 0)?;
    write_u32(
        out,
        Gpr::R14,
        Rv32ArchitecturalState::PC_OFFSET,
        instruction_pc,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::NEXT_PC_OFFSET,
        instruction_pc,
    )?;
    write_cumulative_attempted(cache, out, attempted)?;
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
    out.mov_m32_r32(
        Mem::base_disp(
            Gpr::R15,
            (DbtContext::EXIT_OFFSET + DbtExitRecord::ADDRESS_OFFSET) as i32,
        ),
        address,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::ACCESS_SIZE_OFFSET,
        access_size,
    )?;
    emit_epilogue(out, DbtExitTag::MemoryAccess)
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
    write_cumulative_attempted(cache, out, attempted)?;
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

fn emit_completed_exit(
    cache: &mut RegisterCache,
    out: &mut X64Emitter,
    next_pc: u32,
    attempted: u32,
    kind: DbtLinkKind,
    chainable: bool,
    static_links: &mut StaticLinkCollector,
    cold_exits: &mut ColdExitCollector,
) -> Result<(), EmitError> {
    if chainable {
        cache.flush(out)?;
        out.add_r64_imm32(EXECUTION_COUNTER, attempted as i32)?;
        let jump = out.patchable_jump()?;
        static_links.push(DbtStaticLink {
            target_pc: next_pc,
            displacement_offset: jump.displacement_offset(),
            reset_target_offset: jump.reset_target_offset(),
            kind,
        })?;
        emit_completed_trampoline(out, next_pc, cold_exits)?;
        return Ok(());
    }
    emit_exit(
        cache,
        out,
        DbtExitTag::Completed,
        next_pc,
        if chainable { 0 } else { attempted },
        0,
        0,
    )
}

fn emit_completed_trampoline(
    out: &mut X64Emitter,
    next_pc: u32,
    cold_exits: &mut ColdExitCollector,
) -> Result<(), EmitError> {
    out.mov_r32_imm32(Gpr::Rdx, next_pc)?;
    cold_exits.push(DbtColdExitRelocation {
        displacement_offset: out.external_jump()?,
    })
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
    write_cumulative_attempted(cache, out, attempted)?;
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

#[cfg(feature = "dbt-chain-stats")]
fn add_context_u32(out: &mut X64Emitter, offset: usize, value: u32) -> Result<(), EmitError> {
    out.mov_r32_m32(Gpr::Rax, Mem::base_disp(Gpr::R15, offset as i32))?;
    out.add_r32_imm32(Gpr::Rax, value as i32)?;
    out.mov_m32_r32(Mem::base_disp(Gpr::R15, offset as i32), Gpr::Rax)
}

fn write_cumulative_attempted(
    cache: &RegisterCache,
    out: &mut X64Emitter,
    local_attempted: u32,
) -> Result<(), EmitError> {
    if cache.is_chainable() {
        out.mov_r32_m32(
            Gpr::Rax,
            Mem::base_disp(Gpr::R15, DbtContext::REMAINING_BUDGET_OFFSET as i32),
        )?;
        out.add_r64_r64(Gpr::Rax, EXECUTION_COUNTER)?;
    } else {
        out.mov_r32_imm32(Gpr::Rax, 0)?;
    }
    if local_attempted != 0 {
        out.add_r32_imm32(Gpr::Rax, local_attempted as i32)?;
    }
    out.mov_m32_r32(
        Mem::base_disp(
            Gpr::R15,
            (DbtContext::EXIT_OFFSET + DbtExitRecord::ATTEMPTED_OFFSET) as i32,
        ),
        Gpr::Rax,
    )
}

fn emit_fault(pc: u32, word: Option<u32>, error: EmitError) -> DbtFault {
    let kind = if matches!(
        error,
        EmitError::Capacity { .. } | EmitError::ControlCapacity { .. }
    ) {
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
    use super::{emit_fault, write_u32, DbtTranslationWorkspace};
    use crate::memory::MachineMemory;
    use crate::rv32_dbt::abi::{DbtContext, DbtEntry, DbtExitRecord, DbtExitTag};
    use crate::rv32_dbt::block::{DbtBlockInput, DbtBlockMode, DbtLinkKind};
    use crate::rv32_dbt::executable::ExecutableScratch;
    use crate::rv32_dbt::x86_64::emitter::{EmitError, Gpr, Mem, X64Emitter};
    use crate::rv32_dbt::DbtFaultKind;
    use crate::rv32im::{
        decode_product_word,
        encoding::{
            add, addi, and, andi, auipc, beq, bge, bgeu, blt, bltu, bne, div, divu, fence, fence_i,
            jal, jalr, lb, lbu, lh, lhu, lui, lw, mul, mulh, mulhsu, mulhu, or, ori, rem, remu, sb,
            sh, sll, slli, slt, slti, sltiu, sltu, sra, srai, srl, srli, sub, sw, xor, xori,
        },
        Rv32ArchitecturalState, Rv32ResolvedInstruction, Rv32imCpu,
    };

    #[test]
    fn translation_workspace_rejects_an_empty_specialized_ram_extent() {
        let slots = slots(&[lw(1, 2, 0)]);
        let input = DbtBlockInput::new(0x1000, &slots, DbtBlockMode::DirectFast).unwrap();
        let mut workspace = DbtTranslationWorkspace::new(4096, 1).unwrap();
        let result = workspace.lower(&input, 0);

        assert!(matches!(
            result,
            Err(error) if error.kind() == DbtFaultKind::Capacity
        ));
    }

    fn write_u32_pattern(base: Gpr, offset: usize, value: u32) -> Vec<u8> {
        let mut emitter = X64Emitter::new(32, 1).unwrap();
        write_u32(&mut emitter, base, offset, value).unwrap();
        emitter.finish().unwrap().to_vec()
    }

    fn pattern_count(code: &[u8], pattern: &[u8]) -> usize {
        code.windows(pattern.len())
            .filter(|window| *window == pattern)
            .count()
    }

    fn context_u32_load_pattern(dst: Gpr, offset: usize) -> Vec<u8> {
        let mut emitter = X64Emitter::new(16, 1).unwrap();
        emitter
            .mov_r32_m32(dst, Mem::base_disp(Gpr::R15, offset as i32))
            .unwrap();
        emitter.finish().unwrap().to_vec()
    }

    fn slots(words: &[u32]) -> Vec<Rv32ResolvedInstruction> {
        words
            .iter()
            .copied()
            .map(|word| Rv32ResolvedInstruction::Valid {
                word,
                instruction: decode_product_word(word).unwrap(),
            })
            .collect()
    }

    fn execute(
        input: &DbtBlockInput<'_>,
        registers: &[(usize, u32)],
    ) -> (Rv32imCpu, DbtExitTag, DbtExitRecord, Vec<u8>) {
        let mut workspace = DbtTranslationWorkspace::new(64 * 1024, input.slots().len()).unwrap();
        let compiled = workspace.lower(input, 4096).unwrap();
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
            chain_transitions: 0,
            exit: DbtExitRecord::default(),
        };
        let entry: DbtEntry = unsafe { std::mem::transmute(scratch.entry_address().unwrap()) };
        let tag = DbtExitTag::try_from(unsafe { entry(&mut context) }).unwrap();
        let exit = context.exit;
        (cpu, tag, exit, code)
    }

    fn execute_memory(
        word: u32,
        registers: &[(usize, u32)],
        ram: &mut [u8],
        permissions: &[u8],
    ) -> (Rv32imCpu, DbtExitTag, DbtExitRecord) {
        let (cpu, tag, exit, _) =
            execute_memory_with_reservation(word, registers, ram, permissions, None);
        (cpu, tag, exit)
    }

    fn execute_memory_with_reservation(
        word: u32,
        registers: &[(usize, u32)],
        ram: &mut [u8],
        permissions: &[u8],
        reservation: Option<u32>,
    ) -> (Rv32imCpu, DbtExitTag, DbtExitRecord, u32) {
        let slots = slots(&[word]);
        let input = DbtBlockInput::new(0x1000, &slots, DbtBlockMode::DirectFast).unwrap();
        let mut workspace = DbtTranslationWorkspace::new(4096, 1).unwrap();
        let block = workspace.lower(&input, ram.len() as u32).unwrap();
        let mut scratch = ExecutableScratch::new(4096).unwrap();
        scratch.publish(block.code()).unwrap();
        let mut cpu = Rv32imCpu::new(0x1000);
        for &(register, value) in registers {
            cpu.set_register(register, value).unwrap();
        }
        let mut context = DbtContext {
            state: cpu.architectural_state_mut(),
            ram_base: ram.as_mut_ptr(),
            ram_len: ram.len() as u32,
            page_permissions: permissions.as_ptr(),
            page_count: permissions.len() as u32,
            remaining_budget: 1,
            reservation_valid: u32::from(reservation.is_some()),
            reservation_address: reservation.unwrap_or(0),
            chain_transitions: 0,
            exit: DbtExitRecord::default(),
        };
        let entry: DbtEntry = unsafe { std::mem::transmute(scratch.entry_address().unwrap()) };
        let tag = DbtExitTag::try_from(unsafe { entry(&mut context) }).unwrap();
        let exit = context.exit;
        let reservation_valid = context.reservation_valid;
        drop(context);
        (cpu, tag, exit, reservation_valid)
    }

    #[test]
    fn native_ram_loads_preserve_width_and_sign_extension() {
        let cases = [
            (lb(3, 1, 0), vec![0x80], 0xffff_ff80),
            (lbu(3, 1, 0), vec![0x80], 0x80),
            (lh(3, 1, 0), vec![0x00, 0x80], 0xffff_8000),
            (lhu(3, 1, 0), vec![0x00, 0x80], 0x8000),
            (lw(3, 1, 0), vec![0x21, 0x43, 0x65, 0x87], 0x8765_4321),
        ];
        for (word, bytes, expected) in cases {
            let mut ram = vec![0; 4096];
            ram[64..64 + bytes.len()].copy_from_slice(&bytes);

            let (cpu, tag, exit) = execute_memory(word, &[(1, 64), (3, 7)], &mut ram, &[0b001]);

            assert_eq!(tag, DbtExitTag::Completed, "word={word:#010x}");
            assert_eq!(cpu.register(3), expected, "word={word:#010x}");
            assert_eq!(cpu.pc(), 0x1004);
            assert_eq!(exit.attempted, 1);
        }
    }

    #[test]
    fn native_ram_stores_preserve_width_and_neighbors() {
        let cases = [
            (sb(1, 2, 0), 1_usize),
            (sh(1, 2, 0), 2_usize),
            (sw(1, 2, 0), 4_usize),
        ];
        for (word, width) in cases {
            let mut ram = vec![0xa5; 4096];

            let (cpu, tag, exit) =
                execute_memory(word, &[(1, 64), (2, 0x8765_4321)], &mut ram, &[0b011]);

            assert_eq!(tag, DbtExitTag::Completed, "word={word:#010x}");
            assert_eq!(
                &ram[64..64 + width],
                &0x8765_4321_u32.to_le_bytes()[..width]
            );
            assert_eq!(ram[63], 0xa5);
            assert_eq!(ram[64 + width], 0xa5);
            assert_eq!(cpu.pc(), 0x1004);
            assert_eq!(exit.attempted, 1);
        }
    }

    #[test]
    fn native_ram_rejects_access_before_guest_visible_mutation() {
        let cases = [
            (lw(3, 1, 0), 65_u32, vec![0b001], 4_u32),
            (lw(3, 1, 0), 4096_u32, vec![0b001], 4_u32),
            (lw(3, 1, 0), 64_u32, vec![0b010], 4_u32),
            (sw(1, 2, 0), 64_u32, vec![0b001], 4_u32),
        ];
        for (word, address, permissions, width) in cases {
            let mut ram = vec![0xa5; 4096];
            let before = ram.clone();

            let (cpu, tag, exit) = execute_memory(
                word,
                &[(1, address), (2, 0x1122_3344), (3, 0xfeed_face)],
                &mut ram,
                &permissions,
            );

            assert_eq!(tag, DbtExitTag::MemoryAccess, "word={word:#010x}");
            assert_eq!(cpu.register(3), 0xfeed_face);
            assert_eq!(ram, before);
            assert_eq!(cpu.pc(), 0x1000);
            assert_eq!(exit.attempted, 1);
            assert_eq!(exit.instruction_pc, 0x1000);
            assert_eq!(exit.instruction_word, word);
            assert_eq!(exit.address, address);
            assert_eq!(exit.access_size, width);
        }
    }

    #[test]
    fn native_ram_accepts_aligned_accesses_ending_at_the_ram_boundary() {
        let load_cases = [
            (lbu(3, 1, 0), 4095_u32, 0xa5_u32),
            (lhu(3, 1, 0), 4094_u32, 0xa5a5_u32),
            (lw(3, 1, 0), 4092_u32, 0xa5a5_a5a5_u32),
        ];
        for (word, address, expected) in load_cases {
            let mut ram = vec![0xa5; 4096];
            let (cpu, tag, _) = execute_memory(word, &[(1, address)], &mut ram, &[0b001]);

            assert_eq!(tag, DbtExitTag::Completed, "word={word:#010x}");
            assert_eq!(cpu.register(3), expected, "word={word:#010x}");
        }

        for (word, address, width) in [
            (sb(1, 2, 0), 4095_u32, 1_usize),
            (sh(1, 2, 0), 4094_u32, 2_usize),
            (sw(1, 2, 0), 4092_u32, 4_usize),
        ] {
            let mut ram = vec![0xa5; 4096];
            let (_, tag, _) =
                execute_memory(word, &[(1, address), (2, 0x8765_4321)], &mut ram, &[0b011]);

            assert_eq!(tag, DbtExitTag::Completed, "word={word:#010x}");
            assert_eq!(
                &ram[address as usize..],
                &0x8765_4321_u32.to_le_bytes()[..width],
                "word={word:#010x}",
            );
        }
    }

    #[test]
    fn native_ram_honors_the_byte_bound_on_a_partial_final_page() {
        let mut ram = vec![0xa5; 4097];
        ram[4096] = 0x7e;
        let permissions = [0b011, 0b011];

        let (cpu, tag, _) = execute_memory(lbu(3, 1, 0), &[(1, 4096)], &mut ram, &permissions);
        assert_eq!(tag, DbtExitTag::Completed);
        assert_eq!(cpu.register(3), 0x7e);

        for (word, width) in [
            (lhu(3, 1, 0), 2_u32),
            (lw(3, 1, 0), 4_u32),
            (sh(1, 2, 0), 2_u32),
            (sw(1, 2, 0), 4_u32),
        ] {
            let before = ram.clone();
            let (cpu, tag, exit) = execute_memory(
                word,
                &[(1, 4096), (2, 0x1122_3344), (3, 0xfeed_face)],
                &mut ram,
                &permissions,
            );

            assert_eq!(tag, DbtExitTag::MemoryAccess, "word={word:#010x}");
            assert_eq!(cpu.register(3), 0xfeed_face, "word={word:#010x}");
            assert_eq!(ram, before, "word={word:#010x}");
            assert_eq!(exit.address, 4096, "word={word:#010x}");
            assert_eq!(exit.access_size, width, "word={word:#010x}");
        }
    }

    #[test]
    fn native_ram_store_invalidates_only_an_overlapping_reservation() {
        for (reservation, expected_valid) in [(66, 0), (128, 1)] {
            let mut ram = vec![0; 4096];

            let (_, tag, _, reservation_valid) = execute_memory_with_reservation(
                sw(1, 2, 0),
                &[(1, 64), (2, 0x1122_3344)],
                &mut ram,
                &[0b011],
                Some(reservation),
            );

            assert_eq!(tag, DbtExitTag::Completed);
            assert_eq!(reservation_valid, expected_valid);
        }
    }

    #[test]
    fn translated_block_reports_static_native_ram_sites() {
        let slots = slots(&[lw(3, 1, 0), sw(1, 2, 0), addi(4, 4, 1)]);
        let input = DbtBlockInput::new(0x1000, &slots, DbtBlockMode::DirectFast).unwrap();
        let mut workspace = DbtTranslationWorkspace::new(4096, slots.len()).unwrap();

        let block = workspace.lower(&input, 4096).unwrap();

        assert_eq!(block.lowered_load_sites(), 1);
        assert_eq!(block.lowered_store_sites(), 1);
    }

    #[test]
    fn native_ram_checks_specialize_the_vm_extent() {
        fn translate(ram_len: u32) -> Vec<u8> {
            let slots = slots(&[lw(3, 1, 0), sw(1, 2, 0)]);
            let input = DbtBlockInput::new(0x1000, &slots, DbtBlockMode::DirectFast).unwrap();
            let mut workspace = DbtTranslationWorkspace::new(4096, slots.len()).unwrap();
            workspace.lower(&input, ram_len).unwrap().code().to_vec()
        }

        let short = translate(4096);
        let long = translate(8192);
        let ram_len_load = context_u32_load_pattern(Gpr::Rcx, DbtContext::RAM_LEN_OFFSET);
        let page_count_load = context_u32_load_pattern(Gpr::Rcx, DbtContext::PAGE_COUNT_OFFSET);

        assert_eq!(pattern_count(&short, &ram_len_load), 0);
        assert_eq!(pattern_count(&short, &page_count_load), 0);
        assert_ne!(short, long);
    }

    #[test]
    fn static_chain_metadata_describes_only_fast_fixed_successors() {
        let cases = [
            (
                0x1000,
                slots(&[addi(1, 1, 1)]),
                vec![(DbtLinkKind::Fallthrough, 0x1004)],
            ),
            (
                0x2000,
                slots(&[beq(1, 2, 12)]),
                vec![
                    (DbtLinkKind::BranchNotTaken, 0x2004),
                    (DbtLinkKind::BranchTaken, 0x200c),
                ],
            ),
            (
                0x3000,
                slots(&[jal(1, 16)]),
                vec![(DbtLinkKind::Jal, 0x3010)],
            ),
            (0x4000, slots(&[jalr(1, 2, 0)]), vec![]),
        ];

        let mut workspace = DbtTranslationWorkspace::new(4096, 4).unwrap();
        for (start_pc, slots, expected) in cases {
            let input =
                DbtBlockInput::new(start_pc, &slots, DbtBlockMode::ChainableThroughput).unwrap();
            let block = workspace.lower(&input, 4096).unwrap();

            assert!(block.chain_entry_offset() > 0);
            assert!((block.chain_entry_offset() as usize) < block.code().len());
            assert_eq!(
                block
                    .static_links()
                    .iter()
                    .map(|link| (link.kind, link.target_pc))
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(block.cold_exit_relocations().len(), expected.len() + 1);
            for link in block.static_links() {
                assert!((link.displacement_offset as usize + 4) <= block.code().len());
                assert!((link.reset_target_offset as usize) < block.code().len());
            }
        }

        let slots = slots(&[addi(1, 1, 1), addi(2, 2, 1)]);
        let input =
            DbtBlockInput::new(0x5000, &slots, DbtBlockMode::Bounded { max_attempts: 1 }).unwrap();
        let block = workspace.lower(&input, 4096).unwrap();
        assert!(block.static_links().is_empty());
        assert!(block.cold_exit_relocations().is_empty());
    }

    #[test]
    fn linked_fallthrough_materializes_x0_and_pc_only_on_real_exits() {
        let start_pc = 0x1000;
        let words = slots(&[addi(1, 1, 1)]);
        let input =
            DbtBlockInput::new(start_pc, &words, DbtBlockMode::ChainableThroughput).unwrap();
        let mut workspace = DbtTranslationWorkspace::new(4096, words.len()).unwrap();

        let block = workspace.lower(&input, 4096).unwrap();
        let x0_write = write_u32_pattern(Gpr::R14, Rv32ArchitecturalState::register_offset(0), 0);
        let pc_write = write_u32_pattern(Gpr::R14, Rv32ArchitecturalState::PC_OFFSET, start_pc + 4);

        // Completed materialization belongs to the shared support stub, not
        // the successful linked path or its local cold trampoline.
        assert_eq!(pattern_count(block.code(), &x0_write), 0);
        assert_eq!(pattern_count(block.code(), &pc_write), 0);
    }

    #[test]
    fn control_workspace_exhaustion_is_a_capacity_fault() {
        let fault = emit_fault(0x1000, None, EmitError::ControlCapacity { limit: 8 });

        assert_eq!(fault.kind(), DbtFaultKind::Capacity);
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
        let input = DbtBlockInput::new(start_pc, &slots, DbtBlockMode::DirectFast).unwrap();

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

        let mut workspace = DbtTranslationWorkspace::new(64 * 1024, slots.len()).unwrap();
        let compiled = workspace.lower(&input, 4096).unwrap();
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
            chain_transitions: 0,
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
    fn commutative_destination_alias_avoids_the_rax_roundtrip() {
        fn translated_len(word: u32) -> usize {
            let slots = slots(&[word]);
            let input = DbtBlockInput::new(0x1000, &slots, DbtBlockMode::DirectFast).unwrap();
            let mut workspace = DbtTranslationWorkspace::new(4096, slots.len()).unwrap();
            workspace.lower(&input, 4096).unwrap().code().len()
        }

        assert!(translated_len(add(2, 1, 2)) < translated_len(sub(2, 1, 2)));

        let words = slots(&[add(2, 1, 2)]);
        let input = DbtBlockInput::new(0x1000, &words, DbtBlockMode::DirectFast).unwrap();
        let (cpu, tag, exit, _) = execute(&input, &[(1, 19), (2, 23)]);
        assert_eq!(tag, DbtExitTag::Completed);
        assert_eq!(exit.attempted, 1);
        assert_eq!(cpu.register(1), 19);
        assert_eq!(cpu.register(2), 42);
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
            .collect::<Vec<_>>();
        let input = DbtBlockInput::new(start_pc, &slots, DbtBlockMode::DirectFast).unwrap();
        let mut workspace = DbtTranslationWorkspace::new(4096, slots.len()).unwrap();
        let compiled = workspace.lower(&input, 4096).unwrap();
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
            chain_transitions: 0,
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
            let slots = slots(&[word]);
            let block = DbtBlockInput::new(0x3000, &slots, DbtBlockMode::DirectFast).unwrap();

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
        let jal_slots = slots(&[jal(5, 12)]);
        let jal_block = DbtBlockInput::new(0x4000, &jal_slots, DbtBlockMode::DirectFast).unwrap();
        let (cpu, tag, exit, _) = execute(&jal_block, &[]);
        assert_eq!(tag, DbtExitTag::Completed);
        assert_eq!(cpu.register(5), 0x4004);
        assert_eq!(cpu.pc(), 0x400c);
        assert_eq!(exit.attempted, 1);

        let jalr_slots = slots(&[jalr(6, 1, 5)]);
        let jalr_block = DbtBlockInput::new(0x5000, &jalr_slots, DbtBlockMode::DirectFast).unwrap();
        let (cpu, tag, _, _) = execute(&jalr_block, &[(1, 0x6000)]);
        assert_eq!(tag, DbtExitTag::Completed);
        assert_eq!(cpu.register(6), 0x5004);
        assert_eq!(cpu.pc(), 0x6004);

        let misaligned_jal_slots = slots(&[jal(7, 2)]);
        let misaligned_jal =
            DbtBlockInput::new(0x7000, &misaligned_jal_slots, DbtBlockMode::DirectFast).unwrap();
        let (cpu, tag, exit, _) = execute(&misaligned_jal, &[(7, 0xaaaa_5555)]);
        assert_eq!(tag, DbtExitTag::SlowInstruction);
        assert_eq!(cpu.register(7), 0xaaaa_5555);
        assert_eq!(cpu.pc(), 0x7000);
        assert_eq!(exit.instruction_pc, 0x7000);

        let misaligned_jalr_slots = slots(&[jalr(8, 1, 0)]);
        let misaligned_jalr =
            DbtBlockInput::new(0x8000, &misaligned_jalr_slots, DbtBlockMode::DirectFast).unwrap();
        let (cpu, tag, _, _) = execute(&misaligned_jalr, &[(1, 0x9003), (8, 0x1234)]);
        assert_eq!(tag, DbtExitTag::SlowInstruction);
        assert_eq!(cpu.register(8), 0x1234);
        assert_eq!(cpu.pc(), 0x8000);
    }

    #[test]
    fn final_branch_counts_the_native_prefix_and_itself() {
        let words = [addi(3, 3, 1), bne(1, 2, 8)];
        let slots = slots(&words);
        let block = DbtBlockInput::new(0xa000, &slots, DbtBlockMode::DirectFast).unwrap();

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
                let slots = slots(&words);
                let block = DbtBlockInput::new(
                    0xb000,
                    &slots,
                    DbtBlockMode::Bounded {
                        max_attempts: limit,
                    },
                )
                .unwrap();
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
        let branch_slots = slots(&branch_words);
        let branch_block = DbtBlockInput::new(
            0xc000,
            &branch_slots,
            DbtBlockMode::Bounded { max_attempts: 2 },
        )
        .unwrap();
        let (cpu, tag, exit, _) = execute(&branch_block, &[(1, 1), (2, 2), (3, 9), (4, 20)]);
        assert_eq!(tag, DbtExitTag::Completed);
        assert_eq!(exit.attempted, 2);
        assert_eq!(cpu.pc(), 0xc00c);
        assert_eq!(cpu.register(3), 10);
        assert_eq!(cpu.register(4), 20);

        let slow_words = [addi(3, 3, 1), fence_i()];
        let slow_slots = slots(&slow_words);
        let slow_block = DbtBlockInput::new(
            0xd000,
            &slow_slots,
            DbtBlockMode::Bounded { max_attempts: 1 },
        )
        .unwrap();
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
                let slots = slots(&[word]);
                let block = DbtBlockInput::new(0xe000, &slots, DbtBlockMode::DirectFast).unwrap();
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

    #[test]
    fn one_workspace_lowers_distinct_blocks_sequentially() {
        let add_slots = slots(&[addi(3, 1, 7)]);
        let add_input = DbtBlockInput::new(0x1000, &add_slots, DbtBlockMode::DirectFast).unwrap();
        let mul_slots = slots(&[mul(3, 1, 2)]);
        let mul_input = DbtBlockInput::new(0x2000, &mul_slots, DbtBlockMode::DirectFast).unwrap();
        let mut workspace = DbtTranslationWorkspace::new(4096, 8).unwrap();
        let mut scratch = ExecutableScratch::new(4096).unwrap();
        let capacities = workspace.buffer_capacities();

        let add_block = workspace.lower(&add_input, 4096).unwrap();
        scratch.publish(add_block.code()).unwrap();
        let (add_cpu, add_tag, _, _) = execute_published(&scratch, 0x1000, &[(1, 5)], 1);
        assert_eq!(add_tag, DbtExitTag::Completed);
        assert_eq!(add_cpu.register(3), 12);

        let mul_block = workspace.lower(&mul_input, 4096).unwrap();
        scratch.publish(mul_block.code()).unwrap();
        let (mul_cpu, mul_tag, _, _) = execute_published(&scratch, 0x2000, &[(1, 6), (2, 7)], 1);
        assert_eq!(mul_tag, DbtExitTag::Completed);
        assert_eq!(mul_cpu.register(3), 42);

        assert_eq!(workspace.buffer_capacities(), capacities);
    }

    fn execute_published(
        scratch: &ExecutableScratch,
        start_pc: u32,
        registers: &[(usize, u32)],
        remaining_budget: u32,
    ) -> (Rv32imCpu, DbtExitTag, DbtExitRecord, u32) {
        let mut cpu = Rv32imCpu::new(start_pc);
        for &(register, value) in registers {
            cpu.set_register(register, value).unwrap();
        }
        let mut context = DbtContext {
            state: cpu.architectural_state_mut(),
            ram_base: std::ptr::null_mut(),
            ram_len: 0,
            page_permissions: std::ptr::null(),
            page_count: 0,
            remaining_budget,
            reservation_valid: 0,
            reservation_address: 0,
            chain_transitions: 0,
            exit: DbtExitRecord::default(),
        };
        let entry: DbtEntry = unsafe { std::mem::transmute(scratch.entry_address().unwrap()) };
        let raw_tag = unsafe { entry(&mut context) };
        let tag = DbtExitTag::try_from(raw_tag).unwrap();
        cpu.commit_instructions(context.exit.attempted);
        (cpu, tag, context.exit, raw_tag)
    }
}
