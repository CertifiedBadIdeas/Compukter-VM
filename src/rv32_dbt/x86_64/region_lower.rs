/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

use super::emitter::{Condition, EmitError, Gpr, Mem, X64Emitter};
use super::lower::emit_store_reservation_invalidation;
use super::region_alloc::{HostLocation, RegionAllocation};
use crate::rv32_dbt::abi::{DbtContext, DbtExitRecord, DbtExitTag};
use crate::rv32_dbt::block::{
    DbtBlockInput, DbtColdExitRelocation, DbtLinkKind, DbtStaticLink, TranslatedBlock,
};
use crate::rv32_dbt::region::{
    LoopRegion, RegionBinaryOp, RegionMemoryEffectKind, RegionStep, RegionValueKind, ValueId,
};
use crate::rv32_dbt::{DbtFault, DbtFaultKind};
use crate::rv32im::{Branch, Load, Rv32ArchitecturalState, Store};

const EXECUTION_COUNTER: Gpr = Gpr::Rdi;

pub(crate) struct RegionTranslationWorkspace {
    emitter: X64Emitter,
}

impl RegionTranslationWorkspace {
    pub(crate) fn new(code_capacity: usize) -> Result<Self, DbtFault> {
        let emitter = X64Emitter::new(code_capacity, 128).map_err(|error| emit_fault(0, error))?;
        Ok(Self { emitter })
    }

    pub(crate) fn lower<'a>(
        &'a mut self,
        input: &DbtBlockInput<'_>,
        region: &LoopRegion<'_>,
        allocation: &RegionAllocation,
        ram_len: u32,
    ) -> Result<TranslatedBlock<'a>, DbtFault> {
        if ram_len == 0 {
            return Err(DbtFault::new(
                DbtFaultKind::Capacity,
                input.start_pc(),
                None,
                "Tier 1 RAM length must be positive",
            ));
        }
        if input.instruction_count() != region.instruction_count() {
            return Err(DbtFault::new(
                DbtFaultKind::Translation,
                input.start_pc(),
                None,
                "Tier 1 region and DBT input instruction counts differ",
            ));
        }
        self.emitter.reset();
        let out = &mut self.emitter;
        emit_prologue(out).map_err(|error| emit_fault(input.start_pc(), error))?;
        let external_entry = out
            .new_label()
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        out.jmp(external_entry)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        let chain_entry_offset = u32::try_from(out.bytes().len()).map_err(|_| {
            DbtFault::new(
                DbtFaultKind::Capacity,
                input.start_pc(),
                None,
                "Tier 1 chain entry exceeds u32",
            )
        })?;
        out.bind(external_entry)
            .map_err(|error| emit_fault(input.start_pc(), error))?;

        let carried = carried_guest_count(region);
        let frame_slots = usize::from(allocation.spill_slots()) + carried;
        let frame_bytes = (frame_slots * 8).next_multiple_of(16);
        if frame_bytes != 0 {
            out.add_r64_imm32(Gpr::Rsp, -(frame_bytes as i32))
                .map_err(|error| emit_fault(input.start_pc(), error))?;
        }
        let budget_exit = out
            .new_label()
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        out.test_r64_r64(EXECUTION_COUNTER, EXECUTION_COUNTER)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        out.jcc(Condition::GreaterEqual, budget_exit)
            .map_err(|error| emit_fault(input.start_pc(), error))?;

        emit_entry_parameters(region, allocation, out)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        let loop_entry = out
            .new_label()
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        out.bind(loop_entry)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        emit_steps(region, allocation, out, ram_len, frame_bytes)
            .map_err(|error| emit_fault(input.start_pc(), error))?;

        let branch = region.branch().ok_or_else(|| {
            DbtFault::new(
                DbtFaultKind::Translation,
                input.start_pc(),
                None,
                "Tier 1 region has no loop branch",
            )
        })?;
        load_value(allocation, branch.lhs, Gpr::Rax, out)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        load_value(allocation, branch.rhs, Gpr::Rdx, out)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        out.cmp_r32_r32(Gpr::Rax, Gpr::Rdx)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        let taken = out
            .new_label()
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        out.jcc(branch_condition(branch.kind), taken)
            .map_err(|error| emit_fault(input.start_pc(), error))?;

        out.add_r64_imm32(EXECUTION_COUNTER, region.instruction_count() as i32)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        let fallthrough_pc = input
            .start_pc()
            .wrapping_add(region.instruction_count() as u32 * 4);
        let (fallthrough_link, fallthrough_cold_exit) =
            emit_linked_exit(region, allocation, out, frame_bytes, fallthrough_pc)
                .map_err(|error| emit_fault(input.start_pc(), error))?;

        out.bind(taken)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        emit_loop_reconciliation(region, allocation, out)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        out.add_r64_imm32(EXECUTION_COUNTER, region.instruction_count() as i32)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        out.jcc(Condition::Sign, loop_entry)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        emit_exit(
            region,
            allocation,
            out,
            frame_bytes,
            DbtExitTag::Completed,
            input.start_pc(),
            true,
            true,
        )
        .map_err(|error| emit_fault(input.start_pc(), error))?;

        out.bind(budget_exit)
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        emit_exit(
            region,
            allocation,
            out,
            frame_bytes,
            DbtExitTag::Completed,
            input.start_pc(),
            false,
            false,
        )
        .map_err(|error| emit_fault(input.start_pc(), error))?;

        let code = out
            .finish()
            .map_err(|error| emit_fault(input.start_pc(), error))?;
        TranslatedBlock::new(
            input,
            code,
            0,
            0,
            chain_entry_offset,
            std::slice::from_ref(&fallthrough_link),
            std::slice::from_ref(&fallthrough_cold_exit),
        )
        .map(TranslatedBlock::with_local_self_backedge)
        .map_err(|message| {
            DbtFault::new(DbtFaultKind::Translation, input.start_pc(), None, message)
        })
    }
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
    )?;
    out.mov_r32_m32(
        EXECUTION_COUNTER,
        Mem::base_disp(Gpr::R15, DbtContext::REMAINING_BUDGET_OFFSET as i32),
    )?;
    out.neg_r64(EXECUTION_COUNTER)
}

fn emit_entry_parameters(
    region: &LoopRegion<'_>,
    allocation: &RegionAllocation,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    for index in 0..region.value_count() {
        let Some((value, RegionValueKind::Parameter { guest })) = region.value_at(index) else {
            continue;
        };
        match allocation.location(value) {
            HostLocation::Register(host) => out.mov_r32_m32(
                host,
                Mem::base_disp(
                    Gpr::R14,
                    Rv32ArchitecturalState::register_offset(usize::from(guest)) as i32,
                ),
            )?,
            HostLocation::Spill(slot) => {
                out.mov_r32_m32(
                    Gpr::Rax,
                    Mem::base_disp(
                        Gpr::R14,
                        Rv32ArchitecturalState::register_offset(usize::from(guest)) as i32,
                    ),
                )?;
                out.mov_m32_r32(spill_mem(slot), Gpr::Rax)?;
            }
            HostLocation::Constant(_) | HostLocation::Empty => {}
        }
    }
    Ok(())
}

fn emit_steps(
    region: &LoopRegion<'_>,
    allocation: &RegionAllocation,
    out: &mut X64Emitter,
    ram_len: u32,
    frame_bytes: usize,
) -> Result<(), EmitError> {
    for index in 0..region.step_count() {
        match region.step(index) {
            RegionStep::Empty => {}
            RegionStep::Value(value) => emit_value(region, allocation, value, out)?,
            RegionStep::Load(effect) => emit_load(
                region,
                allocation,
                usize::from(effect),
                out,
                ram_len,
                frame_bytes,
            )?,
            RegionStep::Store(effect) => emit_store(
                region,
                allocation,
                usize::from(effect),
                out,
                ram_len,
                frame_bytes,
            )?,
        }
    }
    Ok(())
}

fn emit_value(
    region: &LoopRegion<'_>,
    allocation: &RegionAllocation,
    value: ValueId,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    if !region.is_value_live(value) {
        return Ok(());
    }
    let RegionValueKind::Binary { op, lhs, rhs } = region.value_kind(value) else {
        return Ok(());
    };
    load_value(allocation, lhs, Gpr::Rax, out)?;
    emit_binary(op, allocation, rhs, out)?;
    store_value(allocation, value, Gpr::Rax, out)
}

fn emit_load(
    region: &LoopRegion<'_>,
    allocation: &RegionAllocation,
    effect_index: usize,
    out: &mut X64Emitter,
    ram_len: u32,
    frame_bytes: usize,
) -> Result<(), EmitError> {
    let effect = region.memory_effect(effect_index);
    let RegionMemoryEffectKind::Load {
        kind,
        address,
        output,
    } = effect.kind()
    else {
        return Err(EmitError::InvalidOperand(
            "Tier 1 load step is not a load effect",
        ));
    };
    load_value(allocation, address, Gpr::Rdx, out)?;
    let width = load_width(kind);
    let slow = emit_ram_checks(Gpr::Rdx, width, 0b001, ram_len, out)?;
    let memory = Mem::base_index_disp(Gpr::R13, Gpr::Rdx, super::emitter::Scale::One, 0);
    match kind {
        Load::Byte => out.movsx_r32_m8(Gpr::Rax, memory)?,
        Load::Half => out.movsx_r32_m16(Gpr::Rax, memory)?,
        Load::Word => out.mov_r32_m32(Gpr::Rax, memory)?,
        Load::ByteU => out.movzx_r32_m8(Gpr::Rax, memory)?,
        Load::HalfU => out.movzx_r32_m16(Gpr::Rax, memory)?,
    }
    store_value(allocation, output, Gpr::Rax, out)?;
    emit_ram_slow_path(
        region,
        allocation,
        effect_index,
        slow,
        Gpr::Rdx,
        width,
        frame_bytes,
        out,
    )
}

fn emit_store(
    region: &LoopRegion<'_>,
    allocation: &RegionAllocation,
    effect_index: usize,
    out: &mut X64Emitter,
    ram_len: u32,
    frame_bytes: usize,
) -> Result<(), EmitError> {
    let effect = region.memory_effect(effect_index);
    let RegionMemoryEffectKind::Store {
        kind,
        address,
        value,
    } = effect.kind()
    else {
        return Err(EmitError::InvalidOperand(
            "Tier 1 store step is not a store effect",
        ));
    };
    load_value(allocation, address, Gpr::Rdx, out)?;
    let width = store_width(kind);
    let slow = emit_ram_checks(Gpr::Rdx, width, 0b010, ram_len, out)?;
    load_value(allocation, value, Gpr::Rax, out)?;
    let memory = Mem::base_index_disp(Gpr::R13, Gpr::Rdx, super::emitter::Scale::One, 0);
    match kind {
        Store::Byte => out.mov_m8_r8(memory, Gpr::Rax)?,
        Store::Half => out.mov_m16_r16(memory, Gpr::Rax)?,
        Store::Word => out.mov_m32_r32(memory, Gpr::Rax)?,
    }
    // Tier 1 may allocate a live region value to RCX, while the shared
    // reservation helper uses RCX as scratch. Preserve it across the helper;
    // RAX is dead after the store and can remain scratch.
    out.push(Gpr::Rcx)?;
    emit_store_reservation_invalidation(Gpr::Rdx, width, out)?;
    out.pop(Gpr::Rcx)?;
    emit_ram_slow_path(
        region,
        allocation,
        effect_index,
        slow,
        Gpr::Rdx,
        width,
        frame_bytes,
        out,
    )
}

fn emit_ram_checks(
    address: Gpr,
    width: u32,
    permission: u8,
    ram_len: u32,
    out: &mut X64Emitter,
) -> Result<super::emitter::Label, EmitError> {
    let slow = out.new_label()?;
    if width > 1 {
        out.mov_r32_r32(Gpr::Rax, address)?;
        out.and_r32_imm32(Gpr::Rax, width as i32 - 1)?;
        out.test_r32_r32(Gpr::Rax, Gpr::Rax)?;
        out.jcc(Condition::NotEqual, slow)?;
    }
    if ram_len < width {
        out.jmp(slow)?;
        return Ok(slow);
    }
    out.cmp_r32_imm32(address, ram_len.wrapping_sub(width) as i32)?;
    out.jcc(Condition::Above, slow)?;
    out.mov_r32_r32(Gpr::Rax, address)?;
    out.shr_r32_imm8(Gpr::Rax, 12)?;
    out.test_m8_imm8(
        Mem::base_index_disp(Gpr::R12, Gpr::Rax, super::emitter::Scale::One, 0),
        permission,
    )?;
    out.jcc(Condition::Equal, slow)?;
    Ok(slow)
}

fn emit_ram_slow_path(
    region: &LoopRegion<'_>,
    allocation: &RegionAllocation,
    effect_index: usize,
    slow: super::emitter::Label,
    address: Gpr,
    width: u32,
    frame_bytes: usize,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    let done = out.new_label()?;
    out.jmp(done)?;
    out.bind(slow)?;
    emit_memory_exit(
        region,
        allocation,
        effect_index,
        address,
        width,
        frame_bytes,
        out,
    )?;
    out.bind(done)
}

fn emit_memory_exit(
    region: &LoopRegion<'_>,
    allocation: &RegionAllocation,
    effect_index: usize,
    address: Gpr,
    width: u32,
    frame_bytes: usize,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    let effect = region.memory_effect(effect_index);
    for guest in 1..32 {
        let Some(value) = region.memory_snapshot(effect_index, guest) else {
            continue;
        };
        load_value(allocation, value, Gpr::Rax, out)?;
        out.mov_m32_r32(
            Mem::base_disp(
                Gpr::R14,
                Rv32ArchitecturalState::register_offset(guest) as i32,
            ),
            Gpr::Rax,
        )?;
    }
    write_u32(out, Gpr::R14, Rv32ArchitecturalState::register_offset(0), 0)?;
    write_u32(
        out,
        Gpr::R14,
        Rv32ArchitecturalState::PC_OFFSET,
        effect.pc(),
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::NEXT_PC_OFFSET,
        effect.pc(),
    )?;
    out.mov_r32_m32(
        Gpr::Rax,
        Mem::base_disp(Gpr::R15, DbtContext::REMAINING_BUDGET_OFFSET as i32),
    )?;
    out.add_r64_r64(Gpr::Rax, EXECUTION_COUNTER)?;
    out.add_r32_imm32(Gpr::Rax, i32::from(effect.attempted_index()) + 1)?;
    out.mov_m32_r32(
        Mem::base_disp(
            Gpr::R15,
            (DbtContext::EXIT_OFFSET + DbtExitRecord::ATTEMPTED_OFFSET) as i32,
        ),
        Gpr::Rax,
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::INSTRUCTION_PC_OFFSET,
        effect.pc(),
    )?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::INSTRUCTION_WORD_OFFSET,
        effect.word(),
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
        width,
    )?;
    emit_epilogue(out, frame_bytes, DbtExitTag::MemoryAccess)
}

const fn load_width(kind: Load) -> u32 {
    match kind {
        Load::Byte | Load::ByteU => 1,
        Load::Half | Load::HalfU => 2,
        Load::Word => 4,
    }
}

const fn store_width(kind: Store) -> u32 {
    match kind {
        Store::Byte => 1,
        Store::Half => 2,
        Store::Word => 4,
    }
}

fn emit_binary(
    op: RegionBinaryOp,
    allocation: &RegionAllocation,
    rhs: ValueId,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    if let HostLocation::Constant(value) = allocation.location(rhs) {
        return match op {
            RegionBinaryOp::Add => out.add_r32_imm32(Gpr::Rax, value as i32),
            RegionBinaryOp::Sub => out.sub_r32_imm32(Gpr::Rax, value as i32),
            RegionBinaryOp::Xor => out.xor_r32_imm32(Gpr::Rax, value as i32),
            RegionBinaryOp::Or => out.or_r32_imm32(Gpr::Rax, value as i32),
            RegionBinaryOp::And => out.and_r32_imm32(Gpr::Rax, value as i32),
            RegionBinaryOp::ShiftLeft => out.shl_r32_imm8(Gpr::Rax, value as u8 & 31),
            RegionBinaryOp::ShiftRight => out.shr_r32_imm8(Gpr::Rax, value as u8 & 31),
            RegionBinaryOp::ShiftRightArithmetic => out.sar_r32_imm8(Gpr::Rax, value as u8 & 31),
            RegionBinaryOp::Multiply => {
                out.mov_r32_imm32(Gpr::Rdx, value)?;
                out.imul_r32_r32(Gpr::Rax, Gpr::Rdx)
            }
            RegionBinaryOp::SetLessThan | RegionBinaryOp::SetLessThanUnsigned => {
                out.cmp_r32_imm32(Gpr::Rax, value as i32)?;
                out.mov_r32_imm32(Gpr::Rax, 0)?;
                out.setcc_r8(
                    if op == RegionBinaryOp::SetLessThan {
                        Condition::Less
                    } else {
                        Condition::Below
                    },
                    Gpr::Rax,
                )
            }
        };
    }
    if matches!(
        op,
        RegionBinaryOp::ShiftLeft
            | RegionBinaryOp::ShiftRight
            | RegionBinaryOp::ShiftRightArithmetic
    ) {
        out.push(Gpr::Rcx)?;
        load_value(allocation, rhs, Gpr::Rcx, out)?;
        match op {
            RegionBinaryOp::ShiftLeft => out.shl_r32_cl(Gpr::Rax)?,
            RegionBinaryOp::ShiftRight => out.shr_r32_cl(Gpr::Rax)?,
            RegionBinaryOp::ShiftRightArithmetic => out.sar_r32_cl(Gpr::Rax)?,
            _ => unreachable!(),
        }
        return out.pop(Gpr::Rcx);
    }
    load_value(allocation, rhs, Gpr::Rdx, out)?;
    match op {
        RegionBinaryOp::Add => out.add_r32_r32(Gpr::Rax, Gpr::Rdx),
        RegionBinaryOp::Sub => out.sub_r32_r32(Gpr::Rax, Gpr::Rdx),
        RegionBinaryOp::Xor => out.xor_r32_r32(Gpr::Rax, Gpr::Rdx),
        RegionBinaryOp::Or => out.or_r32_r32(Gpr::Rax, Gpr::Rdx),
        RegionBinaryOp::And => out.and_r32_r32(Gpr::Rax, Gpr::Rdx),
        RegionBinaryOp::Multiply => out.imul_r32_r32(Gpr::Rax, Gpr::Rdx),
        RegionBinaryOp::SetLessThan | RegionBinaryOp::SetLessThanUnsigned => {
            out.cmp_r32_r32(Gpr::Rax, Gpr::Rdx)?;
            out.mov_r32_imm32(Gpr::Rax, 0)?;
            out.setcc_r8(
                if op == RegionBinaryOp::SetLessThan {
                    Condition::Less
                } else {
                    Condition::Below
                },
                Gpr::Rax,
            )
        }
        RegionBinaryOp::ShiftLeft
        | RegionBinaryOp::ShiftRight
        | RegionBinaryOp::ShiftRightArithmetic => unreachable!(),
    }
}

fn load_value(
    allocation: &RegionAllocation,
    value: ValueId,
    dst: Gpr,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    match allocation.location(value) {
        HostLocation::Constant(value) => out.mov_r32_imm32(dst, value),
        HostLocation::Register(src) if src == dst => Ok(()),
        HostLocation::Register(src) => out.mov_r32_r32(dst, src),
        HostLocation::Spill(slot) => out.mov_r32_m32(dst, spill_mem(slot)),
        HostLocation::Empty => Err(EmitError::InvalidOperand(
            "Tier 1 attempted to load an unallocated value",
        )),
    }
}

fn store_value(
    allocation: &RegionAllocation,
    value: ValueId,
    src: Gpr,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    match allocation.location(value) {
        HostLocation::Register(dst) if dst == src => Ok(()),
        HostLocation::Register(dst) => out.mov_r32_r32(dst, src),
        HostLocation::Spill(slot) => out.mov_m32_r32(spill_mem(slot), src),
        HostLocation::Constant(_) | HostLocation::Empty => Err(EmitError::InvalidOperand(
            "Tier 1 attempted to store into a non-writable value",
        )),
    }
}

fn emit_loop_reconciliation(
    region: &LoopRegion<'_>,
    allocation: &RegionAllocation,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    let phi_base = usize::from(allocation.spill_slots());
    let mut phi = 0;
    for guest in 1..32 {
        let (Some(entry), Some(output)) = (region.entry_value(guest), region.output_value(guest))
        else {
            continue;
        };
        if entry == output || !region.is_value_live(entry) {
            continue;
        }
        load_value(allocation, output, Gpr::Rax, out)?;
        out.mov_m32_r32(stack_slot(phi_base + phi), Gpr::Rax)?;
        phi += 1;
    }
    phi = 0;
    for guest in 1..32 {
        let (Some(entry), Some(output)) = (region.entry_value(guest), region.output_value(guest))
        else {
            continue;
        };
        if entry == output || !region.is_value_live(entry) {
            continue;
        }
        out.mov_r32_m32(Gpr::Rax, stack_slot(phi_base + phi))?;
        store_value(allocation, entry, Gpr::Rax, out)?;
        phi += 1;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_exit(
    region: &LoopRegion<'_>,
    allocation: &RegionAllocation,
    out: &mut X64Emitter,
    frame_bytes: usize,
    tag: DbtExitTag,
    next_pc: u32,
    materialize: bool,
    reconciled: bool,
) -> Result<(), EmitError> {
    if materialize {
        emit_materialized_outputs(region, allocation, out, reconciled)?;
    }
    write_u32(out, Gpr::R14, Rv32ArchitecturalState::register_offset(0), 0)?;
    write_u32(out, Gpr::R14, Rv32ArchitecturalState::PC_OFFSET, next_pc)?;
    write_u32(
        out,
        Gpr::R15,
        DbtContext::EXIT_OFFSET + DbtExitRecord::NEXT_PC_OFFSET,
        next_pc,
    )?;
    out.mov_r32_m32(
        Gpr::Rax,
        Mem::base_disp(Gpr::R15, DbtContext::REMAINING_BUDGET_OFFSET as i32),
    )?;
    out.add_r64_r64(Gpr::Rax, EXECUTION_COUNTER)?;
    out.mov_m32_r32(
        Mem::base_disp(
            Gpr::R15,
            (DbtContext::EXIT_OFFSET + DbtExitRecord::ATTEMPTED_OFFSET) as i32,
        ),
        Gpr::Rax,
    )?;
    for offset in [
        DbtExitRecord::INSTRUCTION_PC_OFFSET,
        DbtExitRecord::INSTRUCTION_WORD_OFFSET,
        DbtExitRecord::ADDRESS_OFFSET,
        DbtExitRecord::ACCESS_SIZE_OFFSET,
    ] {
        write_u32(out, Gpr::R15, DbtContext::EXIT_OFFSET + offset, 0)?;
    }
    emit_epilogue(out, frame_bytes, tag)
}

fn emit_materialized_outputs(
    region: &LoopRegion<'_>,
    allocation: &RegionAllocation,
    out: &mut X64Emitter,
    reconciled: bool,
) -> Result<(), EmitError> {
    for guest in 1..32 {
        let Some(output) = region.output_value(guest) else {
            continue;
        };
        if region.entry_value(guest) == Some(output) {
            continue;
        }
        let value = if reconciled
            && region
                .entry_value(guest)
                .is_some_and(|entry| region.is_value_live(entry))
        {
            region.entry_value(guest).unwrap_or(output)
        } else {
            output
        };
        load_value(allocation, value, Gpr::Rax, out)?;
        out.mov_m32_r32(
            Mem::base_disp(
                Gpr::R14,
                Rv32ArchitecturalState::register_offset(guest) as i32,
            ),
            Gpr::Rax,
        )?;
    }
    Ok(())
}

fn emit_linked_exit(
    region: &LoopRegion<'_>,
    allocation: &RegionAllocation,
    out: &mut X64Emitter,
    frame_bytes: usize,
    next_pc: u32,
) -> Result<(DbtStaticLink, DbtColdExitRelocation), EmitError> {
    emit_materialized_outputs(region, allocation, out, false)?;
    if frame_bytes != 0 {
        out.add_r64_imm32(Gpr::Rsp, frame_bytes as i32)?;
    }
    let jump = out.patchable_jump()?;
    let link = DbtStaticLink {
        target_pc: next_pc,
        displacement_offset: jump.displacement_offset(),
        reset_target_offset: jump.reset_target_offset(),
        kind: DbtLinkKind::BranchNotTaken,
    };
    out.mov_r32_imm32(Gpr::Rdx, next_pc)?;
    let cold_exit = DbtColdExitRelocation {
        displacement_offset: out.external_jump()?,
    };
    Ok((link, cold_exit))
}

fn emit_epilogue(
    out: &mut X64Emitter,
    frame_bytes: usize,
    tag: DbtExitTag,
) -> Result<(), EmitError> {
    if frame_bytes != 0 {
        out.add_r64_imm32(Gpr::Rsp, frame_bytes as i32)?;
    }
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

const fn spill_mem(slot: u8) -> Mem {
    stack_slot(slot as usize)
}

const fn stack_slot(slot: usize) -> Mem {
    Mem::base_disp(Gpr::Rsp, (slot * 8) as i32)
}

fn carried_guest_count(region: &LoopRegion<'_>) -> usize {
    (1..32)
        .filter(|guest| {
            matches!(
                (region.entry_value(*guest), region.output_value(*guest)),
                (Some(entry), Some(output)) if entry != output && region.is_value_live(entry)
            )
        })
        .count()
}

const fn branch_condition(branch: Branch) -> Condition {
    match branch {
        Branch::Eq => Condition::Equal,
        Branch::Ne => Condition::NotEqual,
        Branch::Lt => Condition::Less,
        Branch::Ge => Condition::GreaterEqual,
        Branch::Ltu => Condition::Below,
        Branch::Geu => Condition::AboveEqual,
    }
}

fn emit_fault(pc: u32, error: EmitError) -> DbtFault {
    let kind = if matches!(
        error,
        EmitError::Capacity { .. } | EmitError::ControlCapacity { .. }
    ) {
        DbtFaultKind::Capacity
    } else {
        DbtFaultKind::Translation
    };
    DbtFault::new(kind, pc, None, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::RegionTranslationWorkspace;
    use crate::rv32_dbt::abi::{DbtContext, DbtEntry, DbtExitRecord, DbtExitTag};
    use crate::rv32_dbt::block::{DbtBlockInput, DbtBlockMode, DbtLinkKind};
    use crate::rv32_dbt::code_cache::{DbtCacheKey, DirectDbtCodeCache};
    use crate::rv32_dbt::ir::DbtIrBlock;
    use crate::rv32_dbt::region::{LoopRegionWorkspace, RegionBuildOutcome};
    use crate::rv32_dbt::x86_64::lower::DbtTranslationWorkspace;
    use crate::rv32_dbt::x86_64::region_alloc::{allocate_region, RegionAllocationWorkspace};
    use crate::rv32im::{
        encoding::{addi, bne, lw, sw},
        Rv32imCpu,
    };

    #[derive(Debug, PartialEq, Eq)]
    struct Observation {
        x5: u32,
        pc: u32,
        tag: DbtExitTag,
        exit: DbtExitRecord,
        remaining_budget: u32,
    }

    fn execute(entry: *const u8, budget: u32) -> Observation {
        let mut cpu = Rv32imCpu::new(0x1000);
        cpu.set_register(5, 0).unwrap();
        cpu.set_register(6, 4).unwrap();
        let mut context = DbtContext {
            state: cpu.architectural_state_mut(),
            ram_base: std::ptr::null_mut(),
            ram_len: 4096,
            page_permissions: std::ptr::null(),
            page_count: 1,
            remaining_budget: budget,
            reservation_valid: 0,
            reservation_address: 0,
            chain_transitions: 0,
            #[cfg(feature = "dbt-execution-profile")]
            profile_exit_kind: Default::default(),
            exit: DbtExitRecord::default(),
        };
        let entry: DbtEntry = unsafe { std::mem::transmute(entry) };
        let tag = DbtExitTag::try_from(unsafe { entry(&mut context) }).unwrap();
        let exit = context.exit;
        let remaining_budget = context.remaining_budget;
        drop(context);
        Observation {
            x5: cpu.register(5),
            pc: cpu.pc(),
            tag,
            exit,
            remaining_budget,
        }
    }

    #[test]
    fn arithmetic_region_matches_tier0_across_soft_budget_edges() {
        let words = [addi(5, 5, 1), bne(5, 6, -4)];
        let mut ir = DbtIrBlock::new(16).unwrap();
        for word in words {
            ir.lift_word(word).unwrap();
        }
        ir.analyze_future_values();
        let input = DbtBlockInput::new_ir(0x1000, &ir, DbtBlockMode::ChainableThroughput).unwrap();
        let mut region_workspace = LoopRegionWorkspace::new();
        let RegionBuildOutcome::Built(region) = region_workspace.build_optimized(0x1000, &ir)
        else {
            panic!("expected an optimized region")
        };
        let mut allocation_workspace = RegionAllocationWorkspace::new();
        let allocation = allocate_region(&region, &mut allocation_workspace).unwrap();
        let mut tier0_workspace = DbtTranslationWorkspace::new(4096, 16).unwrap();
        let tier0 = tier0_workspace.lower(&input, 4096).unwrap();
        let mut tier0_cache = DirectDbtCodeCache::new(16, 64 * 1024).unwrap();
        let tier0_entry = tier0_cache
            .publish(DbtCacheKey::new(0x1000, 0), &tier0)
            .unwrap()
            .entry();
        let mut tier1_workspace = RegionTranslationWorkspace::new(4096).unwrap();
        let tier1 = tier1_workspace
            .lower(&input, &region, allocation, 4096)
            .unwrap();
        assert_eq!(tier1.static_links().len(), 1);
        assert_eq!(tier1.static_links()[0].target_pc, 0x1008);
        assert_eq!(tier1.static_links()[0].kind, DbtLinkKind::BranchNotTaken);
        assert_eq!(tier1.cold_exit_relocations().len(), 1);
        let mut tier1_cache = DirectDbtCodeCache::new(16, 64 * 1024).unwrap();
        let tier1_entry = tier1_cache
            .publish(DbtCacheKey::new(0x1000, 0), &tier1)
            .unwrap()
            .entry();

        for budget in 0..=7 {
            assert_eq!(
                execute(tier1_entry, budget),
                execute(tier0_entry, budget),
                "budget={budget}"
            );
        }
    }

    #[test]
    fn loop_with_a_dead_entry_value_matches_tier0() {
        let words = [addi(5, 0, 1), bne(6, 7, -4)];
        let mut ir = DbtIrBlock::new(16).unwrap();
        for word in words {
            ir.lift_word(word).unwrap();
        }
        ir.analyze_future_values();
        let input = DbtBlockInput::new_ir(0x1000, &ir, DbtBlockMode::ChainableThroughput).unwrap();
        let mut region_workspace = LoopRegionWorkspace::new();
        let RegionBuildOutcome::Built(region) = region_workspace.build_optimized(0x1000, &ir)
        else {
            panic!("expected an optimized region")
        };
        assert!(!region.is_value_live(region.entry_value(5).unwrap()));
        let mut allocation_workspace = RegionAllocationWorkspace::new();
        let allocation = allocate_region(&region, &mut allocation_workspace).unwrap();
        let mut tier0_workspace = DbtTranslationWorkspace::new(4096, 16).unwrap();
        let tier0 = tier0_workspace.lower(&input, 4096).unwrap();
        let mut tier0_cache = DirectDbtCodeCache::new(16, 64 * 1024).unwrap();
        let tier0_entry = tier0_cache
            .publish(DbtCacheKey::new(0x1000, 0), &tier0)
            .unwrap()
            .entry();
        let mut tier1_workspace = RegionTranslationWorkspace::new(4096).unwrap();
        let tier1 = tier1_workspace
            .lower(&input, &region, allocation, 4096)
            .unwrap();
        let mut tier1_cache = DirectDbtCodeCache::new(16, 64 * 1024).unwrap();
        let tier1_entry = tier1_cache
            .publish(DbtCacheKey::new(0x1000, 0), &tier1)
            .unwrap()
            .entry();

        assert_eq!(execute(tier1_entry, 5), execute(tier0_entry, 5));
    }

    #[derive(Debug, PartialEq, Eq)]
    struct MemoryObservation {
        x5: u32,
        x10: u32,
        pc: u32,
        tag: DbtExitTag,
        exit: DbtExitRecord,
        reservation_valid: u32,
        ram: Vec<u8>,
    }

    fn execute_memory(
        entry: *const u8,
        mut ram: Vec<u8>,
        start: u32,
        end: u32,
        reservation: Option<u32>,
    ) -> MemoryObservation {
        let permissions = [0b011_u8];
        let mut cpu = Rv32imCpu::new(0x2000);
        cpu.set_register(10, start).unwrap();
        cpu.set_register(11, end).unwrap();
        let mut context = DbtContext {
            state: cpu.architectural_state_mut(),
            ram_base: ram.as_mut_ptr(),
            ram_len: ram.len() as u32,
            page_permissions: permissions.as_ptr(),
            page_count: 1,
            remaining_budget: 10,
            reservation_valid: u32::from(reservation.is_some()),
            reservation_address: reservation.unwrap_or(0),
            chain_transitions: 0,
            #[cfg(feature = "dbt-execution-profile")]
            profile_exit_kind: Default::default(),
            exit: DbtExitRecord::default(),
        };
        let entry: DbtEntry = unsafe { std::mem::transmute(entry) };
        let tag = DbtExitTag::try_from(unsafe { entry(&mut context) }).unwrap();
        let exit = context.exit;
        let reservation_valid = context.reservation_valid;
        drop(context);
        MemoryObservation {
            x5: cpu.register(5),
            x10: cpu.register(10),
            pc: cpu.pc(),
            tag,
            exit,
            reservation_valid,
            ram,
        }
    }

    #[test]
    fn ram_region_matches_tier0_for_ordered_two_iteration_loop() {
        let words = [
            lw(5, 10, 0),
            addi(5, 5, 1),
            sw(10, 5, 0),
            addi(10, 10, 4),
            bne(10, 11, -16),
        ];
        let mut ir = DbtIrBlock::new(16).unwrap();
        for word in words {
            ir.lift_word(word).unwrap();
        }
        ir.analyze_future_values();
        let input = DbtBlockInput::new_ir(0x2000, &ir, DbtBlockMode::ChainableThroughput).unwrap();
        let mut region_workspace = LoopRegionWorkspace::new();
        let RegionBuildOutcome::Built(region) = region_workspace.build_optimized(0x2000, &ir)
        else {
            panic!("expected an optimized RAM region")
        };
        let mut allocation_workspace = RegionAllocationWorkspace::new();
        let allocation = allocate_region(&region, &mut allocation_workspace).unwrap();

        let mut tier0_workspace = DbtTranslationWorkspace::new(4096, 16).unwrap();
        let tier0 = tier0_workspace.lower(&input, 4096).unwrap();
        let mut tier0_cache = DirectDbtCodeCache::new(16, 64 * 1024).unwrap();
        let tier0_entry = tier0_cache
            .publish(DbtCacheKey::new(0x2000, 0), &tier0)
            .unwrap()
            .entry();
        let mut tier1_workspace = RegionTranslationWorkspace::new(4096).unwrap();
        let tier1 = tier1_workspace
            .lower(&input, &region, allocation, 4096)
            .unwrap();
        let mut tier1_cache = DirectDbtCodeCache::new(16, 64 * 1024).unwrap();
        let tier1_entry = tier1_cache
            .publish(DbtCacheKey::new(0x2000, 0), &tier1)
            .unwrap()
            .entry();

        let mut ram = vec![0; 4096];
        ram[64..68].copy_from_slice(&7_u32.to_le_bytes());
        ram[68..72].copy_from_slice(&11_u32.to_le_bytes());
        assert_eq!(
            execute_memory(tier1_entry, ram.clone(), 64, 72, None),
            execute_memory(tier0_entry, ram, 64, 72, None)
        );
    }

    #[test]
    fn ram_region_preserves_second_iteration_pre_fault_state() {
        let words = [
            lw(5, 10, 0),
            addi(5, 5, 1),
            sw(10, 5, 0),
            addi(10, 10, 4),
            bne(10, 11, -16),
        ];
        let mut ir = DbtIrBlock::new(16).unwrap();
        for word in words {
            ir.lift_word(word).unwrap();
        }
        ir.analyze_future_values();
        let input = DbtBlockInput::new_ir(0x2000, &ir, DbtBlockMode::ChainableThroughput).unwrap();
        let mut region_workspace = LoopRegionWorkspace::new();
        let RegionBuildOutcome::Built(region) = region_workspace.build_optimized(0x2000, &ir)
        else {
            panic!("expected an optimized RAM region")
        };
        let mut allocation_workspace = RegionAllocationWorkspace::new();
        let allocation = allocate_region(&region, &mut allocation_workspace).unwrap();

        let mut tier0_workspace = DbtTranslationWorkspace::new(4096, 16).unwrap();
        let tier0 = tier0_workspace.lower(&input, 4096).unwrap();
        let mut tier0_cache = DirectDbtCodeCache::new(16, 64 * 1024).unwrap();
        let tier0_entry = tier0_cache
            .publish(DbtCacheKey::new(0x2000, 0), &tier0)
            .unwrap()
            .entry();
        let mut tier1_workspace = RegionTranslationWorkspace::new(4096).unwrap();
        let tier1 = tier1_workspace
            .lower(&input, &region, allocation, 4096)
            .unwrap();
        let mut tier1_cache = DirectDbtCodeCache::new(16, 64 * 1024).unwrap();
        let tier1_entry = tier1_cache
            .publish(DbtCacheKey::new(0x2000, 0), &tier1)
            .unwrap()
            .entry();

        let mut ram = vec![0; 4096];
        ram[4092..4096].copy_from_slice(&7_u32.to_le_bytes());
        assert_eq!(
            execute_memory(tier1_entry, ram.clone(), 4092, 4100, None),
            execute_memory(tier0_entry, ram, 4092, 4100, None)
        );
    }

    #[test]
    fn ram_region_store_invalidates_an_overlapping_reservation() {
        let words = [
            lw(5, 10, 0),
            addi(5, 5, 1),
            sw(10, 5, 0),
            addi(10, 10, 4),
            bne(10, 11, -16),
        ];
        let mut ir = DbtIrBlock::new(16).unwrap();
        for word in words {
            ir.lift_word(word).unwrap();
        }
        ir.analyze_future_values();
        let input = DbtBlockInput::new_ir(0x2000, &ir, DbtBlockMode::ChainableThroughput).unwrap();
        let mut region_workspace = LoopRegionWorkspace::new();
        let RegionBuildOutcome::Built(region) = region_workspace.build_optimized(0x2000, &ir)
        else {
            panic!("expected an optimized RAM region")
        };
        let mut allocation_workspace = RegionAllocationWorkspace::new();
        let allocation = allocate_region(&region, &mut allocation_workspace).unwrap();

        let mut tier0_workspace = DbtTranslationWorkspace::new(4096, 16).unwrap();
        let tier0 = tier0_workspace.lower(&input, 4096).unwrap();
        let mut tier0_cache = DirectDbtCodeCache::new(16, 64 * 1024).unwrap();
        let tier0_entry = tier0_cache
            .publish(DbtCacheKey::new(0x2000, 0), &tier0)
            .unwrap()
            .entry();
        let mut tier1_workspace = RegionTranslationWorkspace::new(4096).unwrap();
        let tier1 = tier1_workspace
            .lower(&input, &region, allocation, 4096)
            .unwrap();
        let mut tier1_cache = DirectDbtCodeCache::new(16, 64 * 1024).unwrap();
        let tier1_entry = tier1_cache
            .publish(DbtCacheKey::new(0x2000, 0), &tier1)
            .unwrap()
            .entry();

        let mut ram = vec![0; 4096];
        ram[64..68].copy_from_slice(&7_u32.to_le_bytes());
        assert_eq!(
            execute_memory(tier1_entry, ram.clone(), 64, 68, Some(64)),
            execute_memory(tier0_entry, ram, 64, 68, Some(64))
        );
    }
}
