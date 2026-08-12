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

pub(crate) const MAX_STATIC_LINKS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DbtLinkKind {
    Fallthrough,
    BranchTaken,
    BranchNotTaken,
    Jal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DbtStaticLink {
    pub(crate) target_pc: u32,
    pub(crate) displacement_offset: u32,
    pub(crate) reset_target_offset: u32,
    pub(crate) kind: DbtLinkKind,
}

impl DbtStaticLink {
    pub(crate) const EMPTY: Self = Self {
        target_pc: 0,
        displacement_offset: 0,
        reset_target_offset: 0,
        kind: DbtLinkKind::Fallthrough,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DbtBlockMode {
    DirectFast,
    ChainableThroughput,
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
    lowered_load_sites: u32,
    lowered_store_sites: u32,
    chain_entry_offset: u32,
    static_links: [DbtStaticLink; MAX_STATIC_LINKS],
    static_link_count: u8,
    code: &'a [u8],
}

impl<'a> TranslatedBlock<'a> {
    pub(crate) fn new(
        input: &DbtBlockInput<'_>,
        code: &'a [u8],
        lowered_load_sites: u32,
        lowered_store_sites: u32,
        chain_entry_offset: u32,
        static_links: &[DbtStaticLink],
    ) -> Result<Self, String> {
        if code.is_empty() {
            return Err("RV32 DBT compiled block cannot be empty".to_string());
        }
        if chain_entry_offset as usize >= code.len() {
            return Err(format!(
                "RV32 DBT chain entry offset {chain_entry_offset} is outside {} emitted bytes",
                code.len()
            ));
        }
        if static_links.len() > MAX_STATIC_LINKS {
            return Err(format!(
                "RV32 DBT block has {} static links but supports at most {MAX_STATIC_LINKS}",
                static_links.len()
            ));
        }
        if input.mode() != DbtBlockMode::ChainableThroughput && !static_links.is_empty() {
            return Err("only RV32 DBT chainable blocks can expose static links".to_string());
        }
        for link in static_links {
            let displacement_end = (link.displacement_offset as usize)
                .checked_add(4)
                .ok_or_else(|| "RV32 DBT link displacement range overflowed".to_string())?;
            if displacement_end > code.len() || link.reset_target_offset as usize >= code.len() {
                return Err("RV32 DBT static link lies outside emitted code".to_string());
            }
        }
        let mut stored_links = [DbtStaticLink::EMPTY; MAX_STATIC_LINKS];
        stored_links[..static_links.len()].copy_from_slice(static_links);
        Ok(Self {
            start_pc: input.start_pc(),
            instruction_count: input.slots().len() as u32,
            mode: input.mode(),
            lowered_load_sites,
            lowered_store_sites,
            chain_entry_offset,
            static_links: stored_links,
            static_link_count: static_links.len() as u8,
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

    pub(crate) fn lowered_load_sites(&self) -> u32 {
        self.lowered_load_sites
    }

    pub(crate) fn lowered_store_sites(&self) -> u32 {
        self.lowered_store_sites
    }

    pub(crate) fn chain_entry_offset(&self) -> u32 {
        self.chain_entry_offset
    }

    pub(crate) fn static_links(&self) -> &[DbtStaticLink] {
        &self.static_links[..self.static_link_count as usize]
    }

    pub(crate) fn code(&self) -> &[u8] {
        self.code
    }
}

#[cfg(test)]
mod tests {
    use super::{DbtBlockInput, DbtBlockMode, DbtLinkKind, DbtStaticLink, TranslatedBlock};
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
        let input = DbtBlockInput::new(0x1000, &slots, DbtBlockMode::DirectFast).unwrap();

        assert!(std::ptr::eq(input.slots().as_ptr(), slots.as_ptr()));
    }

    #[test]
    fn block_modes_distinguish_direct_and_chainable_fast_paths() {
        assert_ne!(DbtBlockMode::DirectFast, DbtBlockMode::ChainableThroughput);
    }

    #[test]
    fn block_input_rejects_empty_and_invalid_bounded_modes() {
        assert!(DbtBlockInput::new(0x1000, &[], DbtBlockMode::DirectFast).is_err());
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
        let block = TranslatedBlock::new(&input, &code, 3, 2, 0, &[]).unwrap();

        assert_eq!(block.start_pc(), 0x1000);
        assert_eq!(block.instruction_count(), 2);
        assert_eq!(block.mode(), DbtBlockMode::Bounded { max_attempts: 1 });
        assert_eq!(block.lowered_load_sites(), 3);
        assert_eq!(block.lowered_store_sites(), 2);
        assert_eq!(block.code(), &[0xc3]);
        assert_eq!(block.chain_entry_offset(), 0);
        assert!(block.static_links().is_empty());
    }

    #[test]
    fn translated_block_rejects_invalid_chain_metadata() {
        let slots = [slot(), slot()];
        let input = DbtBlockInput::new(0x1000, &slots, DbtBlockMode::DirectFast).unwrap();
        let code = [0xe9, 0, 0, 0, 0, 0xc3];
        let invalid = DbtStaticLink {
            target_pc: 0x1008,
            displacement_offset: 3,
            reset_target_offset: 6,
            kind: DbtLinkKind::Fallthrough,
        };

        assert!(TranslatedBlock::new(&input, &code, 0, 0, 6, &[]).is_err());
        assert!(TranslatedBlock::new(&input, &code, 0, 0, 0, &[invalid]).is_err());
    }
}
