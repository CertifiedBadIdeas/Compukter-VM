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

use super::{decode_product_word, DecodedInstruction, Rv32ResolvedInstruction};
use crate::memory::{MemoryBus, MemoryFault};

const PAGE_BYTES: u32 = 4096;
pub(crate) const MAX_BLOCK_INSTRUCTIONS: usize = 64;

pub(crate) fn validate_block_max_instructions(max_instructions: usize) -> Result<(), String> {
    if !(1..=MAX_BLOCK_INSTRUCTIONS).contains(&max_instructions) {
        return Err(format!(
            "RV32 decoded block maximum instruction count {max_instructions} is outside 1..={MAX_BLOCK_INSTRUCTIONS}"
        ));
    }
    Ok(())
}

pub(crate) fn fill_decoded_block(
    start_pc: u32,
    executable_end: u32,
    max_instructions: usize,
    bus: &mut dyn MemoryBus,
    slots: &mut Vec<Rv32ResolvedInstruction>,
) -> Result<(), MemoryFault> {
    validate_block_max_instructions(max_instructions).map_err(MemoryFault::new)?;
    let page_end = (start_pc & !(PAGE_BYTES - 1))
        .checked_add(PAGE_BYTES)
        .unwrap_or(u32::MAX);
    let block_end = executable_end.min(page_end);
    require_complete_word(start_pc, block_end)?;

    let first = resolve_slot(start_pc, bus)?;
    slots.clear();
    slots.push(first);
    let mut instruction_pc = start_pc.wrapping_add(4);
    while slots.len() < max_instructions && !ends_basic_block(*slots.last().unwrap()) {
        if instruction_pc
            .checked_add(4)
            .is_none_or(|instruction_end| instruction_end > block_end)
        {
            break;
        }
        match resolve_slot(instruction_pc, bus) {
            Ok(slot) => slots.push(slot),
            Err(_) => break,
        }
        instruction_pc = instruction_pc.wrapping_add(4);
    }

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
                "RV32 instruction at {instruction_pc:#010x} crosses decoded block boundary {block_end:#010x}"
            ),
        ));
    }
    Ok(())
}

fn resolve_slot(
    instruction_pc: u32,
    bus: &mut dyn MemoryBus,
) -> Result<Rv32ResolvedInstruction, MemoryFault> {
    let word = bus.load_i32(instruction_pc)? as u32;
    Ok(match decode_product_word(word) {
        Ok(instruction) => Rv32ResolvedInstruction::Valid { word, instruction },
        Err(_) => Rv32ResolvedInstruction::Invalid { word },
    })
}

pub(crate) const fn ends_basic_block(slot: Rv32ResolvedInstruction) -> bool {
    matches!(
        slot,
        Rv32ResolvedInstruction::Invalid { .. }
            | Rv32ResolvedInstruction::Valid {
                instruction: DecodedInstruction::Jal { .. }
                    | DecodedInstruction::Jalr { .. }
                    | DecodedInstruction::Branch { .. }
                    | DecodedInstruction::Ecall
                    | DecodedInstruction::Ebreak
                    | DecodedInstruction::Mret
                    | DecodedInstruction::FenceI,
                ..
            }
    )
}

#[cfg(test)]
mod tests {
    use super::{ends_basic_block, fill_decoded_block};
    use crate::memory::MachineMemory;
    use crate::rv32im::encoding::{addi, jal};
    use crate::rv32im::Rv32ResolvedInstruction;

    fn memory(words: &[u32]) -> MachineMemory {
        memory_at(0, words)
    }

    fn memory_at(start: u32, words: &[u32]) -> MachineMemory {
        let size = start as usize + words.len().max(1) * 4;
        let mut memory = MachineMemory::zeroed(size).unwrap();
        for (index, word) in words.iter().copied().enumerate() {
            memory
                .store_i32(start + index as u32 * 4, word as i32)
                .unwrap();
        }
        memory
    }

    #[test]
    fn direct_builder_stops_after_control_flow() {
        let mut memory = memory(&[addi(1, 1, 1), jal(0, 0), addi(2, 2, 1)]);
        let mut slots = Vec::with_capacity(64);

        fill_decoded_block(0, 12, 64, &mut memory, &mut slots).unwrap();

        assert_eq!(slots.len(), 2);
        assert!(ends_basic_block(slots[1]));
    }

    #[test]
    fn direct_builder_stops_at_the_executable_page_end() {
        let mut memory = memory_at(0x0ff8, &[addi(1, 1, 1), addi(2, 2, 1), addi(3, 3, 1)]);
        let mut slots = Vec::with_capacity(64);

        fill_decoded_block(0x0ff8, 0x1004, 64, &mut memory, &mut slots).unwrap();

        assert_eq!(slots.len(), 2);
    }

    #[test]
    fn direct_builder_keeps_an_invalid_terminal_slot() {
        let mut memory = memory(&[0xffff_ffff]);
        let mut slots = Vec::with_capacity(8);

        fill_decoded_block(0, 4, 8, &mut memory, &mut slots).unwrap();

        assert!(matches!(
            slots.as_slice(),
            [Rv32ResolvedInstruction::Invalid { word: 0xffff_ffff }]
        ));
    }
}
