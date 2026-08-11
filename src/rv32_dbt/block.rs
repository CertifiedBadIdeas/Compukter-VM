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
    reason = "the direct x86_64 translator consumes block metadata in the next task"
)]

use crate::rv32im::Rv32ResolvedInstruction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DbtBlockMode {
    Fast,
    Bounded { max_attempts: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DbtBlockInput<'a> {
    start_pc: u32,
    slots: &'a [Rv32ResolvedInstruction],
    mode: DbtBlockMode,
}

impl<'a> DbtBlockInput<'a> {
    pub(crate) fn new(
        start_pc: u32,
        slots: &'a [Rv32ResolvedInstruction],
        mode: DbtBlockMode,
    ) -> Result<Self, String> {
        if slots.is_empty() {
            return Err("RV32 DBT block cannot be empty".to_string());
        }
        if let DbtBlockMode::Bounded { max_attempts } = mode {
            if max_attempts == 0 || max_attempts as usize >= slots.len() {
                return Err(format!(
                    "RV32 DBT bounded attempt count {max_attempts} must be inside 1..{}",
                    slots.len()
                ));
            }
        }
        Ok(Self {
            start_pc,
            slots,
            mode,
        })
    }

    pub(crate) fn start_pc(&self) -> u32 {
        self.start_pc
    }

    pub(crate) fn slots(&self) -> &[Rv32ResolvedInstruction] {
        self.slots
    }

    pub(crate) fn mode(&self) -> DbtBlockMode {
        self.mode
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TranslatedBlock<'a> {
    start_pc: u32,
    instruction_count: u32,
    mode: DbtBlockMode,
    code: &'a [u8],
}

impl<'a> TranslatedBlock<'a> {
    pub(crate) fn new(input: &DbtBlockInput<'_>, code: &'a [u8]) -> Result<Self, String> {
        if code.is_empty() {
            return Err("RV32 DBT compiled block cannot be empty".to_string());
        }
        Ok(Self {
            start_pc: input.start_pc(),
            instruction_count: input.slots().len() as u32,
            mode: input.mode(),
            code,
        })
    }

    pub(crate) fn start_pc(&self) -> u32 {
        self.start_pc
    }

    pub(crate) fn instruction_count(&self) -> u32 {
        self.instruction_count
    }

    pub(crate) fn mode(&self) -> DbtBlockMode {
        self.mode
    }

    pub(crate) fn code(&self) -> &[u8] {
        self.code
    }
}

#[cfg(test)]
mod tests {
    use super::{DbtBlockInput, DbtBlockMode, TranslatedBlock};
    use crate::rv32im::{decode_product_word, encoding::addi, Rv32ResolvedInstruction};

    fn slot() -> Rv32ResolvedInstruction {
        let word = addi(1, 1, 1);
        Rv32ResolvedInstruction::Valid {
            word,
            instruction: decode_product_word(word).unwrap(),
        }
    }

    #[test]
    fn block_input_borrows_the_callers_slots() {
        let slots = [slot(), slot()];
        let input = DbtBlockInput::new(0x1000, &slots, DbtBlockMode::Fast).unwrap();

        assert!(std::ptr::eq(input.slots().as_ptr(), slots.as_ptr()));
    }

    #[test]
    fn block_input_rejects_empty_and_invalid_bounded_modes() {
        assert!(DbtBlockInput::new(0x1000, &[], DbtBlockMode::Fast).is_err());
        let one_slot = [slot()];
        assert!(
            DbtBlockInput::new(0x1000, &one_slot, DbtBlockMode::Bounded { max_attempts: 0 },)
                .is_err()
        );
        assert!(
            DbtBlockInput::new(0x1000, &one_slot, DbtBlockMode::Bounded { max_attempts: 1 },)
                .is_err()
        );
    }

    #[test]
    fn translated_block_keeps_cache_independent_metadata() {
        let slots = [slot(), slot()];
        let input =
            DbtBlockInput::new(0x1000, &slots, DbtBlockMode::Bounded { max_attempts: 1 }).unwrap();
        let code = [0xc3];
        let block = TranslatedBlock::new(&input, &code).unwrap();

        assert_eq!(block.start_pc(), 0x1000);
        assert_eq!(block.instruction_count(), 2);
        assert_eq!(block.mode(), DbtBlockMode::Bounded { max_attempts: 1 });
        assert_eq!(block.code(), &[0xc3]);
    }
}
