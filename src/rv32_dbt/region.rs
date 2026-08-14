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

use crate::rv32_dbt::ir::DbtIrBlock;
use crate::rv32im::{DecodedFields, DecodedOp, ImmOp, Load, Op, Store};

pub(crate) const MAX_REGION_INSTRUCTIONS: usize = 16;
pub(crate) const MAX_REGION_VALUES: usize = 64;
pub(crate) const MAX_REGION_MEMORY_EFFECTS: usize = MAX_REGION_INSTRUCTIONS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValueId(u8);

impl ValueId {
    const INVALID: Self = Self(u8::MAX);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegionBinaryOp {
    Add,
    Sub,
    ShiftLeft,
    SetLessThan,
    SetLessThanUnsigned,
    Xor,
    ShiftRight,
    ShiftRightArithmetic,
    Or,
    And,
    Multiply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegionValueKind {
    Empty,
    Parameter {
        guest: u8,
    },
    Constant(u32),
    Binary {
        op: RegionBinaryOp,
        lhs: ValueId,
        rhs: ValueId,
    },
    Load {
        effect: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegionMemoryEffectKind {
    Empty,
    Load {
        kind: Load,
        address: ValueId,
        output: ValueId,
    },
    Store {
        kind: Store,
        address: ValueId,
        value: ValueId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RegionMemoryEffect {
    kind: RegionMemoryEffectKind,
    pc: u32,
    word: u32,
    attempted_index: u8,
}

impl RegionMemoryEffect {
    const EMPTY: Self = Self {
        kind: RegionMemoryEffectKind::Empty,
        pc: 0,
        word: 0,
        attempted_index: 0,
    };

    pub(crate) const fn pc(self) -> u32 {
        self.pc
    }

    pub(crate) const fn word(self) -> u32 {
        self.word
    }

    pub(crate) const fn attempted_index(self) -> u8 {
        self.attempted_index
    }

    pub(crate) const fn kind(self) -> RegionMemoryEffectKind {
        self.kind
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegionFallbackReason {
    NotSelfLoop,
    TooManyInstructions,
    UnsupportedInstruction,
    InvalidInstruction,
    Capacity,
}

#[derive(Debug)]
pub(crate) struct LoopRegion<'a> {
    workspace: &'a LoopRegionWorkspace,
}

impl LoopRegion<'_> {
    pub(crate) fn instruction_count(&self) -> usize {
        self.workspace.instruction_count
    }

    pub(crate) fn entry_value(&self, guest: usize) -> Option<ValueId> {
        valid_value(self.workspace.entry_values[guest])
    }

    pub(crate) fn output_value(&self, guest: usize) -> Option<ValueId> {
        valid_value(self.workspace.guest_values[guest])
    }

    pub(crate) fn value_kind(&self, value: ValueId) -> RegionValueKind {
        self.workspace.values[usize::from(value.0)]
    }

    pub(crate) fn constant_value(&self, constant: u32) -> Option<ValueId> {
        self.workspace.values[..self.workspace.value_count]
            .iter()
            .position(|kind| *kind == RegionValueKind::Constant(constant))
            .map(|index| ValueId(index as u8))
    }

    pub(crate) const fn memory_effect_count(&self) -> usize {
        self.workspace.memory_effect_count
    }

    pub(crate) fn memory_effect(&self, index: usize) -> RegionMemoryEffect {
        self.workspace.memory_effects[index]
    }
}

#[derive(Debug)]
pub(crate) enum RegionBuildOutcome<'a> {
    Built(LoopRegion<'a>),
    Fallback(RegionFallbackReason),
}

#[derive(Debug)]
pub(crate) struct LoopRegionWorkspace {
    values: [RegionValueKind; MAX_REGION_VALUES],
    value_count: usize,
    entry_values: [ValueId; 32],
    guest_values: [ValueId; 32],
    memory_effects: [RegionMemoryEffect; MAX_REGION_MEMORY_EFFECTS],
    memory_effect_count: usize,
    instruction_count: usize,
}

impl LoopRegionWorkspace {
    pub(crate) const fn new() -> Self {
        Self {
            values: [RegionValueKind::Empty; MAX_REGION_VALUES],
            value_count: 0,
            entry_values: [ValueId::INVALID; 32],
            guest_values: [ValueId::INVALID; 32],
            memory_effects: [RegionMemoryEffect::EMPTY; MAX_REGION_MEMORY_EFFECTS],
            memory_effect_count: 0,
            instruction_count: 0,
        }
    }

    pub(crate) fn build<'a>(
        &'a mut self,
        start_pc: u32,
        ir: &DbtIrBlock,
    ) -> RegionBuildOutcome<'a> {
        if ir.invalid_word().is_some() {
            return RegionBuildOutcome::Fallback(RegionFallbackReason::InvalidInstruction);
        }
        let instructions = ir.instructions();
        if instructions.len() > MAX_REGION_INSTRUCTIONS {
            return RegionBuildOutcome::Fallback(RegionFallbackReason::TooManyInstructions);
        }
        if start_pc & 3 != 0 {
            return RegionBuildOutcome::Fallback(RegionFallbackReason::NotSelfLoop);
        }
        let Some((last_index, last)) = instructions.iter().copied().enumerate().last() else {
            return RegionBuildOutcome::Fallback(RegionFallbackReason::NotSelfLoop);
        };
        if instructions[..last_index]
            .iter()
            .any(|instruction| !is_supported_body_operation(instruction.fields().operation))
        {
            return RegionBuildOutcome::Fallback(RegionFallbackReason::UnsupportedInstruction);
        }
        let fields = last.fields();
        let DecodedOp::Branch(_) = fields.operation else {
            return RegionBuildOutcome::Fallback(RegionFallbackReason::NotSelfLoop);
        };
        let branch_pc = start_pc.wrapping_add((last_index as u32).wrapping_mul(4));
        if branch_pc.wrapping_add_signed(fields.immediate) != start_pc {
            return RegionBuildOutcome::Fallback(RegionFallbackReason::NotSelfLoop);
        }
        self.reset(instructions.len());
        for (index, instruction) in instructions[..last_index].iter().copied().enumerate() {
            if self
                .push_instruction(start_pc, index, instruction.word(), instruction.fields())
                .is_err()
            {
                return RegionBuildOutcome::Fallback(RegionFallbackReason::Capacity);
            }
        }
        if self.read_guest(fields.rs1).is_err() || self.read_guest(fields.rs2).is_err() {
            return RegionBuildOutcome::Fallback(RegionFallbackReason::Capacity);
        }
        RegionBuildOutcome::Built(LoopRegion { workspace: self })
    }

    fn reset(&mut self, instruction_count: usize) {
        self.values.fill(RegionValueKind::Empty);
        self.value_count = 0;
        self.entry_values.fill(ValueId::INVALID);
        self.guest_values.fill(ValueId::INVALID);
        self.memory_effects.fill(RegionMemoryEffect::EMPTY);
        self.memory_effect_count = 0;
        self.instruction_count = instruction_count;
        let zero = self.push_value(RegionValueKind::Constant(0)).unwrap();
        self.entry_values[0] = zero;
        self.guest_values[0] = zero;
    }

    fn push_instruction(
        &mut self,
        start_pc: u32,
        index: usize,
        word: u32,
        fields: DecodedFields,
    ) -> Result<(), ()> {
        let (op, immediate) = match fields.operation {
            DecodedOp::Immediate(op) => (immediate_binary_op(op), Some(fields.immediate as u32)),
            DecodedOp::Register(op) => (register_binary_op(op).ok_or(())?, None),
            DecodedOp::Load(kind) => return self.push_load(start_pc, index, word, fields, kind),
            DecodedOp::Store(kind) => return self.push_store(start_pc, index, word, fields, kind),
            _ => return Err(()),
        };
        let lhs = self.read_guest(fields.rs1)?;
        let rhs = if let Some(immediate) = immediate {
            self.push_value(RegionValueKind::Constant(immediate))?
        } else {
            self.read_guest(fields.rs2)?
        };
        let value = self.push_value(RegionValueKind::Binary { op, lhs, rhs })?;
        if fields.rd != 0 {
            self.guest_values[usize::from(fields.rd)] = value;
        }
        Ok(())
    }

    fn push_load(
        &mut self,
        start_pc: u32,
        index: usize,
        word: u32,
        fields: DecodedFields,
        kind: Load,
    ) -> Result<(), ()> {
        let address = self.push_address(fields.rs1, fields.immediate)?;
        let effect = u8::try_from(self.memory_effect_count).map_err(|_| ())?;
        let output = self.push_value(RegionValueKind::Load { effect })?;
        self.push_memory_effect(RegionMemoryEffect {
            kind: RegionMemoryEffectKind::Load {
                kind,
                address,
                output,
            },
            pc: start_pc.wrapping_add((index as u32).wrapping_mul(4)),
            word,
            attempted_index: index as u8,
        })?;
        if fields.rd != 0 {
            self.guest_values[usize::from(fields.rd)] = output;
        }
        Ok(())
    }

    fn push_store(
        &mut self,
        start_pc: u32,
        index: usize,
        word: u32,
        fields: DecodedFields,
        kind: Store,
    ) -> Result<(), ()> {
        let address = self.push_address(fields.rs1, fields.immediate)?;
        let value = self.read_guest(fields.rs2)?;
        self.push_memory_effect(RegionMemoryEffect {
            kind: RegionMemoryEffectKind::Store {
                kind,
                address,
                value,
            },
            pc: start_pc.wrapping_add((index as u32).wrapping_mul(4)),
            word,
            attempted_index: index as u8,
        })
    }

    fn push_address(&mut self, base_guest: u8, immediate: i32) -> Result<ValueId, ()> {
        let base = self.read_guest(base_guest)?;
        if immediate == 0 {
            return Ok(base);
        }
        let displacement = self.push_value(RegionValueKind::Constant(immediate as u32))?;
        self.push_value(RegionValueKind::Binary {
            op: RegionBinaryOp::Add,
            lhs: base,
            rhs: displacement,
        })
    }

    fn push_memory_effect(&mut self, effect: RegionMemoryEffect) -> Result<(), ()> {
        let slot = self
            .memory_effects
            .get_mut(self.memory_effect_count)
            .ok_or(())?;
        *slot = effect;
        self.memory_effect_count += 1;
        Ok(())
    }

    fn read_guest(&mut self, guest: u8) -> Result<ValueId, ()> {
        let index = usize::from(guest);
        if let Some(value) = valid_value(self.guest_values[index]) {
            return Ok(value);
        }
        let value = self.push_value(RegionValueKind::Parameter { guest })?;
        self.entry_values[index] = value;
        self.guest_values[index] = value;
        Ok(value)
    }

    fn push_value(&mut self, kind: RegionValueKind) -> Result<ValueId, ()> {
        let slot = self.values.get_mut(self.value_count).ok_or(())?;
        *slot = kind;
        let value = ValueId(self.value_count as u8);
        self.value_count += 1;
        Ok(value)
    }
}

const fn valid_value(value: ValueId) -> Option<ValueId> {
    if value.0 == u8::MAX {
        None
    } else {
        Some(value)
    }
}

const fn immediate_binary_op(op: ImmOp) -> RegionBinaryOp {
    match op {
        ImmOp::Add => RegionBinaryOp::Add,
        ImmOp::Slt => RegionBinaryOp::SetLessThan,
        ImmOp::Sltu => RegionBinaryOp::SetLessThanUnsigned,
        ImmOp::Xor => RegionBinaryOp::Xor,
        ImmOp::Or => RegionBinaryOp::Or,
        ImmOp::And => RegionBinaryOp::And,
        ImmOp::Sll => RegionBinaryOp::ShiftLeft,
        ImmOp::Srl => RegionBinaryOp::ShiftRight,
        ImmOp::Sra => RegionBinaryOp::ShiftRightArithmetic,
    }
}

const fn register_binary_op(op: Op) -> Option<RegionBinaryOp> {
    Some(match op {
        Op::Add => RegionBinaryOp::Add,
        Op::Sub => RegionBinaryOp::Sub,
        Op::Sll => RegionBinaryOp::ShiftLeft,
        Op::Slt => RegionBinaryOp::SetLessThan,
        Op::Sltu => RegionBinaryOp::SetLessThanUnsigned,
        Op::Xor => RegionBinaryOp::Xor,
        Op::Srl => RegionBinaryOp::ShiftRight,
        Op::Sra => RegionBinaryOp::ShiftRightArithmetic,
        Op::Or => RegionBinaryOp::Or,
        Op::And => RegionBinaryOp::And,
        Op::Mul => RegionBinaryOp::Multiply,
        _ => return None,
    })
}

const fn is_supported_body_operation(operation: DecodedOp) -> bool {
    match operation {
        DecodedOp::Load(_) | DecodedOp::Store(_) | DecodedOp::Immediate(_) => true,
        DecodedOp::Register(op) => matches!(
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
                | Op::Mul
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LoopRegionWorkspace, RegionBinaryOp, RegionBuildOutcome, RegionFallbackReason,
        RegionValueKind,
    };
    use crate::rv32_dbt::ir::DbtIrBlock;
    use crate::rv32im::encoding::{addi, bne, csrrw, div, lr_w, lw, sw};

    fn fallback_for_words(start_pc: u32, words: &[u32]) -> RegionFallbackReason {
        let mut ir = DbtIrBlock::new(words.len().max(1)).unwrap();
        for word in words {
            ir.lift_word(*word).unwrap();
        }
        let mut workspace = LoopRegionWorkspace::new();
        match workspace.build(start_pc, &ir) {
            RegionBuildOutcome::Fallback(reason) => reason,
            RegionBuildOutcome::Built(_) => panic!("expected Tier 1 fallback"),
        }
    }

    #[test]
    fn eligibility_accepts_a_final_branch_to_the_block_start() {
        let mut ir = DbtIrBlock::new(16).unwrap();
        ir.lift_word(addi(5, 5, 1)).unwrap();
        ir.lift_word(bne(5, 6, -4)).unwrap();

        let mut workspace = LoopRegionWorkspace::new();
        let outcome = workspace.build(0x608, &ir);

        let RegionBuildOutcome::Built(region) = outcome else {
            panic!("expected an eligible region")
        };
        assert_eq!(region.instruction_count(), 2);
    }

    #[test]
    fn eligibility_rejects_unsupported_body_operations() {
        for unsupported in [csrrw(1, 0xc00, 2), lr_w(1, 2, false, false), div(1, 2, 3)] {
            assert_eq!(
                fallback_for_words(0x800, &[unsupported, bne(5, 6, -4)]),
                RegionFallbackReason::UnsupportedInstruction
            );
        }
    }

    #[test]
    fn builder_tracks_parameters_and_loop_carried_outputs() {
        let mut ir = DbtIrBlock::new(16).unwrap();
        ir.lift_word(addi(5, 5, 1)).unwrap();
        ir.lift_word(bne(5, 6, -4)).unwrap();
        let mut workspace = LoopRegionWorkspace::new();

        let RegionBuildOutcome::Built(region) = workspace.build(0x608, &ir) else {
            panic!("expected an eligible region")
        };
        let entry = region.entry_value(5).expect("x5 entry parameter");
        let output = region.output_value(5).expect("x5 loop output");

        assert_eq!(
            region.value_kind(entry),
            RegionValueKind::Parameter { guest: 5 }
        );
        assert_eq!(
            region.value_kind(output),
            RegionValueKind::Binary {
                op: RegionBinaryOp::Add,
                lhs: entry,
                rhs: region.constant_value(1).expect("addi constant"),
            }
        );
        assert_ne!(entry, output);
    }

    #[test]
    fn builder_keeps_memory_effects_in_guest_order_with_precise_metadata() {
        let words = [lw(5, 10, 0), addi(5, 5, 1), sw(10, 5, 0), bne(10, 11, -12)];
        let mut ir = DbtIrBlock::new(16).unwrap();
        for word in words {
            ir.lift_word(word).unwrap();
        }
        let mut workspace = LoopRegionWorkspace::new();

        let RegionBuildOutcome::Built(region) = workspace.build(0x900, &ir) else {
            panic!("expected a memory loop region")
        };

        assert_eq!(region.memory_effect_count(), 2);
        assert_eq!(region.memory_effect(0).pc(), 0x900);
        assert_eq!(region.memory_effect(0).word(), words[0]);
        assert_eq!(region.memory_effect(0).attempted_index(), 0);
        assert_eq!(region.memory_effect(1).pc(), 0x908);
        assert_eq!(region.memory_effect(1).word(), words[2]);
        assert_eq!(region.memory_effect(1).attempted_index(), 2);
    }
}
