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

use crate::memory::{MemoryBus, MemoryFault};
use crate::rv32im::{decode_fields, DecodedFields, DecodedOp};

const PAGE_BYTES: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValueId(u16);

impl ValueId {
    pub(crate) const ZERO: Self = Self(0);

    pub(crate) const fn new(raw: u16) -> Self {
        Self(raw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DbtIrOp {
    UpperImmediate,
    Jump,
    Branch,
    Load,
    Store,
    Immediate,
    Register,
    Fence,
    Atomic,
    Csr,
    Trap,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DbtIrInstruction {
    word: u32,
    fields: DecodedFields,
    inputs: [ValueId; 2],
    output: Option<ValueId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FutureValue {
    Unused,
    Read(u8),
    Dead(u8),
}

impl DbtIrInstruction {
    pub(crate) const fn word(self) -> u32 {
        self.word
    }

    pub(crate) const fn fields(self) -> DecodedFields {
        self.fields
    }

    pub(crate) const fn inputs(self) -> [ValueId; 2] {
        self.inputs
    }

    pub(crate) const fn output(self) -> Option<ValueId> {
        self.output
    }

    pub(crate) const fn operation(self) -> DbtIrOp {
        match self.fields.operation {
            DecodedOp::Lui | DecodedOp::Auipc => DbtIrOp::UpperImmediate,
            DecodedOp::Jal | DecodedOp::Jalr => DbtIrOp::Jump,
            DecodedOp::Branch(_) => DbtIrOp::Branch,
            DecodedOp::Load(_) | DecodedOp::LoadReserved(_) => DbtIrOp::Load,
            DecodedOp::Store(_) => DbtIrOp::Store,
            DecodedOp::Immediate(_) => DbtIrOp::Immediate,
            DecodedOp::Register(_) => DbtIrOp::Register,
            DecodedOp::Fence | DecodedOp::FenceI => DbtIrOp::Fence,
            DecodedOp::StoreConditional(_) | DecodedOp::Atomic(_, _) => DbtIrOp::Atomic,
            DecodedOp::Csr { .. } => DbtIrOp::Csr,
            DecodedOp::Ecall | DecodedOp::Ebreak | DecodedOp::Mret => DbtIrOp::Trap,
        }
    }
}

#[derive(Debug)]
pub(crate) struct DbtIrBlock {
    instructions: Vec<DbtIrInstruction>,
    future_values: Vec<[FutureValue; 32]>,
    values: [ValueId; 32],
    next_value: u16,
    capacity: usize,
    invalid_word: Option<u32>,
}

impl DbtIrBlock {
    pub(crate) fn new(capacity: usize) -> Result<Self, String> {
        if capacity == 0 || capacity > 64 {
            return Err(format!("RV32 DBT IR capacity {capacity} is outside 1..=64"));
        }
        let mut values = [ValueId::ZERO; 32];
        for (register, value) in values.iter_mut().enumerate().skip(1) {
            *value = ValueId::new(register as u16);
        }
        Ok(Self {
            instructions: Vec::with_capacity(capacity),
            future_values: Vec::with_capacity(capacity),
            values,
            next_value: 32,
            capacity,
            invalid_word: None,
        })
    }

    pub(crate) fn instructions(&self) -> &[DbtIrInstruction] {
        &self.instructions
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.instructions.capacity() * std::mem::size_of::<DbtIrInstruction>()
            + self.future_values.capacity() * std::mem::size_of::<[FutureValue; 32]>()
    }

    pub(crate) fn attempted_instruction_count(&self) -> usize {
        self.instructions.len() + usize::from(self.invalid_word.is_some())
    }

    pub(crate) const fn invalid_word(&self) -> Option<u32> {
        self.invalid_word
    }

    pub(crate) fn reset(&mut self) {
        self.instructions.clear();
        self.future_values.clear();
        self.values.fill(ValueId::ZERO);
        for (register, value) in self.values.iter_mut().enumerate().skip(1) {
            *value = ValueId::new(register as u16);
        }
        self.next_value = 32;
        self.invalid_word = None;
    }

    #[cfg(test)]
    pub(crate) fn instruction_capacity(&self) -> usize {
        self.instructions.capacity()
    }

    #[cfg(test)]
    pub(crate) fn analysis_capacity(&self) -> usize {
        self.future_values.capacity()
    }

    pub(crate) fn lift_word(&mut self, word: u32) -> Result<(), String> {
        if self.instructions.len() == self.capacity {
            return Err(format!(
                "RV32 DBT IR block exceeded its {}-instruction capacity",
                self.capacity
            ));
        }
        let fields = decode_fields(word)?;
        let (reads, write) = register_effects(fields);
        let inputs = reads.map(|register| {
            register.map_or(ValueId::ZERO, |register| self.values[usize::from(register)])
        });
        let output = if let Some(register) = write.filter(|register| *register != 0) {
            let value = ValueId::new(self.next_value);
            self.next_value = self
                .next_value
                .checked_add(1)
                .ok_or_else(|| "RV32 DBT IR value ID overflow".to_string())?;
            self.values[usize::from(register)] = value;
            Some(value)
        } else {
            None
        };
        self.instructions.push(DbtIrInstruction {
            word,
            fields,
            inputs,
            output,
        });
        Ok(())
    }

    pub(crate) fn analyze_future_values(&mut self) {
        self.future_values.clear();
        self.future_values
            .resize(self.instructions.len(), [FutureValue::Unused; 32]);
        let mut next_read = [None; 32];
        let mut next_write = [None; 32];
        let mut next_exit = None;
        for index in (0..self.instructions.len()).rev() {
            let fields = self.instructions[index].fields;
            let (reads, write) = register_effects(fields);
            if may_exit_before_write(fields.operation) {
                next_exit = Some(index);
            }
            for register in reads.into_iter().flatten() {
                next_read[usize::from(register)] = Some(index);
            }
            if let Some(register) = write {
                next_write[usize::from(register)] = Some(index);
            }
            for guest in 0..32 {
                let read = next_read[guest];
                let write = next_write[guest];
                self.future_values[index][guest] = match (read, write) {
                    (Some(read), Some(write)) if read <= write => {
                        FutureValue::Read((read - index) as u8)
                    }
                    (Some(read), None) => FutureValue::Read((read - index) as u8),
                    (_, Some(write)) if next_exit.is_some_and(|exit| exit <= write) => {
                        FutureValue::Read((write - index) as u8)
                    }
                    (_, Some(write)) => FutureValue::Dead((write - index) as u8),
                    (None, None) => FutureValue::Unused,
                };
            }
        }
    }

    pub(crate) fn future_value(&self, instruction: usize, guest: usize) -> FutureValue {
        self.future_values[instruction][guest]
    }
}

pub(crate) fn fill_ir_block(
    start_pc: u32,
    executable_end: u32,
    max_instructions: usize,
    bus: &dyn MemoryBus,
    block: &mut DbtIrBlock,
) -> Result<(), MemoryFault> {
    if max_instructions == 0 || max_instructions > block.capacity {
        return Err(MemoryFault::new(format!(
            "RV32 DBT IR block maximum {max_instructions} exceeds workspace capacity {}",
            block.capacity
        )));
    }
    let page_end = (start_pc & !(PAGE_BYTES - 1))
        .checked_add(PAGE_BYTES)
        .unwrap_or(u32::MAX);
    let block_end = executable_end.min(page_end);
    require_complete_word(start_pc, block_end)?;
    block.reset();

    let mut instruction_pc = start_pc;
    while block.attempted_instruction_count() < max_instructions {
        if instruction_pc
            .checked_add(4)
            .is_none_or(|instruction_end| instruction_end > block_end)
        {
            break;
        }
        let word = bus.load_i32(instruction_pc)? as u32;
        match block.lift_word(word) {
            Ok(()) => {
                if ends_ir_block(block.instructions.last().unwrap().fields.operation) {
                    break;
                }
            }
            Err(_) => {
                block.invalid_word = Some(word);
                break;
            }
        }
        instruction_pc = instruction_pc.wrapping_add(4);
    }
    block.analyze_future_values();
    Ok(())
}

fn require_complete_word(instruction_pc: u32, block_end: u32) -> Result<(), MemoryFault> {
    if instruction_pc
        .checked_add(4)
        .is_none_or(|instruction_end| instruction_end > block_end)
    {
        return Err(MemoryFault::at(
            instruction_pc,
            format!(
                "RV32 instruction at {instruction_pc:#010x} crosses DBT IR block boundary {block_end:#010x}"
            ),
        ));
    }
    Ok(())
}

fn ends_ir_block(operation: DecodedOp) -> bool {
    matches!(
        operation,
        DecodedOp::Jal
            | DecodedOp::Jalr
            | DecodedOp::Branch(_)
            | DecodedOp::Ecall
            | DecodedOp::Ebreak
            | DecodedOp::Mret
            | DecodedOp::FenceI
    )
}

pub(crate) fn register_effects(fields: DecodedFields) -> ([Option<u8>; 2], Option<u8>) {
    match fields.operation {
        DecodedOp::Lui | DecodedOp::Auipc | DecodedOp::Jal => ([None, None], Some(fields.rd)),
        DecodedOp::Jalr | DecodedOp::Load(_) | DecodedOp::LoadReserved(_) => {
            ([Some(fields.rs1), None], Some(fields.rd))
        }
        DecodedOp::Branch(_) | DecodedOp::Store(_) => ([Some(fields.rs1), Some(fields.rs2)], None),
        DecodedOp::Immediate(_) => ([Some(fields.rs1), None], Some(fields.rd)),
        DecodedOp::Register(_) | DecodedOp::StoreConditional(_) | DecodedOp::Atomic(_, _) => {
            ([Some(fields.rs1), Some(fields.rs2)], Some(fields.rd))
        }
        DecodedOp::Csr {
            immediate_source, ..
        } => (
            [(!immediate_source).then_some(fields.rs1), None],
            Some(fields.rd),
        ),
        DecodedOp::Fence
        | DecodedOp::FenceI
        | DecodedOp::Ecall
        | DecodedOp::Ebreak
        | DecodedOp::Mret => ([None, None], None),
    }
}

pub(crate) fn may_exit_before_write(operation: DecodedOp) -> bool {
    matches!(
        operation,
        DecodedOp::Jal
            | DecodedOp::Jalr
            | DecodedOp::Load(_)
            | DecodedOp::Store(_)
            | DecodedOp::LoadReserved(_)
            | DecodedOp::StoreConditional(_)
            | DecodedOp::Atomic(_, _)
            | DecodedOp::Csr { .. }
            | DecodedOp::FenceI
            | DecodedOp::Ecall
            | DecodedOp::Ebreak
            | DecodedOp::Mret
    )
}

#[cfg(test)]
mod tests {
    use super::{fill_ir_block, DbtIrBlock, DbtIrInstruction, DbtIrOp, FutureValue, ValueId};
    use crate::memory::MachineMemory;
    use crate::rv32im::encoding::{add, addi, ecall, jal};

    fn memory(words: &[u32]) -> MachineMemory {
        let mut memory = MachineMemory::zeroed(words.len().max(1) * 4).unwrap();
        for (index, word) in words.iter().copied().enumerate() {
            memory.store_i32(index as u32 * 4, word as i32).unwrap();
        }
        memory
    }

    #[test]
    fn lifting_assigns_block_local_versions_without_materializing_decoded_instructions() {
        let mut block = DbtIrBlock::new(2).unwrap();

        block.lift_word(addi(1, 0, 7)).unwrap();
        block.lift_word(add(2, 1, 1)).unwrap();

        assert_eq!(block.instructions().len(), 2);
        assert_eq!(block.instructions()[0].operation(), DbtIrOp::Immediate);
        assert_eq!(block.instructions()[0].inputs(), [ValueId::ZERO; 2]);
        assert_eq!(block.instructions()[0].output(), Some(ValueId::new(32)));
        assert_eq!(block.instructions()[1].operation(), DbtIrOp::Register);
        assert_eq!(
            block.instructions()[1].inputs(),
            [ValueId::new(32), ValueId::new(32)]
        );
        assert_eq!(block.instructions()[1].output(), Some(ValueId::new(33)));
    }

    #[test]
    fn ir_instruction_stays_small_enough_for_a_complete_block_to_fit_in_l1() {
        assert!(std::mem::size_of::<DbtIrInstruction>() <= 24);
    }

    #[test]
    fn reverse_analysis_distinguishes_reads_dead_writes_and_exits() {
        let mut block = DbtIrBlock::new(4).unwrap();
        block.lift_word(addi(3, 0, 1)).unwrap();
        block.lift_word(addi(4, 3, 1)).unwrap();
        block.lift_word(ecall()).unwrap();
        block.lift_word(addi(3, 0, 2)).unwrap();

        block.analyze_future_values();

        assert_eq!(block.future_value(0, 3), FutureValue::Dead(0));
        assert_eq!(block.future_value(1, 3), FutureValue::Read(0));
        assert_eq!(block.future_value(2, 3), FutureValue::Read(1));
        assert_eq!(block.future_value(3, 3), FutureValue::Dead(0));
        assert_eq!(block.future_value(0, 31), FutureValue::Unused);
    }

    #[test]
    fn fused_builder_stops_at_control_flow_and_reuses_its_storage() {
        let first = memory(&[addi(1, 1, 1), jal(0, 0), addi(2, 2, 1)]);
        let second = memory(&[addi(3, 3, 1)]);
        let mut block = DbtIrBlock::new(8).unwrap();
        let instruction_capacity = block.instruction_capacity();
        let analysis_capacity = block.analysis_capacity();

        fill_ir_block(0, 12, 8, &first, &mut block).unwrap();
        assert_eq!(block.instructions().len(), 2);
        assert_eq!(block.attempted_instruction_count(), 2);
        assert_eq!(block.invalid_word(), None);

        fill_ir_block(0, 4, 8, &second, &mut block).unwrap();
        assert_eq!(block.instructions().len(), 1);
        assert_eq!(block.instruction_capacity(), instruction_capacity);
        assert_eq!(block.analysis_capacity(), analysis_capacity);
    }

    #[test]
    fn fused_builder_retains_an_invalid_terminal_word_without_a_decoded_enum() {
        let memory = memory(&[addi(1, 1, 1), 0xffff_ffff, addi(2, 2, 1)]);
        let mut block = DbtIrBlock::new(8).unwrap();

        fill_ir_block(0, 12, 8, &memory, &mut block).unwrap();

        assert_eq!(block.instructions().len(), 1);
        assert_eq!(block.attempted_instruction_count(), 2);
        assert_eq!(block.invalid_word(), Some(0xffff_ffff));
    }
}
