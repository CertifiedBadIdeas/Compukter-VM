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
use super::region_alloc::{HostLocation, RegionAllocation};
use crate::rv32_dbt::abi::{DbtContext, DbtExitRecord, DbtExitTag};
use crate::rv32_dbt::block::{DbtBlockInput, TranslatedBlock};
use crate::rv32_dbt::region::{LoopRegion, RegionBinaryOp, RegionValueKind, ValueId};
use crate::rv32_dbt::{DbtFault, DbtFaultKind};
use crate::rv32im::{Branch, Rv32ArchitecturalState};

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
        if region.memory_effect_count() != 0 {
            return Err(DbtFault::new(
                DbtFaultKind::Translation,
                input.start_pc(),
                None,
                "Tier 1 arithmetic lowering does not yet accept RAM effects",
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
        emit_values(region, allocation, out)
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
        emit_exit(
            region,
            allocation,
            out,
            frame_bytes,
            DbtExitTag::Completed,
            input
                .start_pc()
                .wrapping_add(region.instruction_count() as u32 * 4),
            true,
            false,
        )
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
        TranslatedBlock::new(input, code, 0, 0, chain_entry_offset, &[], &[])
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

fn emit_values(
    region: &LoopRegion<'_>,
    allocation: &RegionAllocation,
    out: &mut X64Emitter,
) -> Result<(), EmitError> {
    for index in 0..region.value_count() {
        let Some((value, kind)) = region.value_at(index) else {
            continue;
        };
        if !region.is_value_live(value) {
            continue;
        }
        let RegionValueKind::Binary { op, lhs, rhs } = kind else {
            continue;
        };
        load_value(allocation, lhs, Gpr::Rax, out)?;
        emit_binary(op, allocation, rhs, out)?;
        store_value(allocation, value, Gpr::Rax, out)?;
    }
    Ok(())
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
        if entry == output {
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
        if entry == output {
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
        for guest in 1..32 {
            let Some(output) = region.output_value(guest) else {
                continue;
            };
            if region.entry_value(guest) == Some(output) {
                continue;
            }
            let value = if reconciled {
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
                (Some(entry), Some(output)) if entry != output
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
    use crate::rv32_dbt::block::{DbtBlockInput, DbtBlockMode};
    use crate::rv32_dbt::code_cache::{DbtCacheKey, DirectDbtCodeCache};
    use crate::rv32_dbt::ir::DbtIrBlock;
    use crate::rv32_dbt::region::{LoopRegionWorkspace, RegionBuildOutcome};
    use crate::rv32_dbt::x86_64::lower::DbtTranslationWorkspace;
    use crate::rv32_dbt::x86_64::region_alloc::{allocate_region, RegionAllocationWorkspace};
    use crate::rv32im::{
        encoding::{addi, bne},
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
}
