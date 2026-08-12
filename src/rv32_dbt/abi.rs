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
    reason = "the direct x86_64 emitter consumes the DBT ABI in the next task"
)]

use crate::rv32im::Rv32ArchitecturalState;

pub(crate) const DBT_ABI_VERSION: u32 = 2;
pub(crate) type DbtEntry = unsafe extern "C" fn(*mut DbtContext) -> u32;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DbtExitTag {
    Completed = 0,
    BudgetExhausted = 1,
    SlowInstruction = 2,
    MemoryAccess = 3,
}

impl TryFrom<u32> for DbtExitTag {
    type Error = String;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Completed),
            1 => Ok(Self::BudgetExhausted),
            2 => Ok(Self::SlowInstruction),
            3 => Ok(Self::MemoryAccess),
            _ => Err(format!("unknown RV32 DBT exit tag {value}")),
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DbtExitRecord {
    pub(crate) next_pc: u32,
    pub(crate) attempted: u32,
    pub(crate) instruction_pc: u32,
    pub(crate) instruction_word: u32,
    pub(crate) address: u32,
    pub(crate) access_size: u32,
}

impl DbtExitRecord {
    pub(crate) const NEXT_PC_OFFSET: usize = std::mem::offset_of!(Self, next_pc);
    pub(crate) const ATTEMPTED_OFFSET: usize = std::mem::offset_of!(Self, attempted);
    pub(crate) const INSTRUCTION_PC_OFFSET: usize = std::mem::offset_of!(Self, instruction_pc);
    pub(crate) const INSTRUCTION_WORD_OFFSET: usize = std::mem::offset_of!(Self, instruction_word);
    pub(crate) const ADDRESS_OFFSET: usize = std::mem::offset_of!(Self, address);
    pub(crate) const ACCESS_SIZE_OFFSET: usize = std::mem::offset_of!(Self, access_size);
}

#[repr(C)]
pub(crate) struct DbtContext {
    pub(crate) state: *mut Rv32ArchitecturalState,
    pub(crate) ram_base: *mut u8,
    pub(crate) ram_len: u32,
    pub(crate) page_permissions: *const u8,
    pub(crate) page_count: u32,
    pub(crate) remaining_budget: u32,
    pub(crate) reservation_valid: u32,
    pub(crate) reservation_address: u32,
    pub(crate) chain_attempted: u32,
    pub(crate) chain_transitions: u32,
    pub(crate) exit: DbtExitRecord,
}

impl DbtContext {
    pub(crate) const ABI_VERSION: u32 = DBT_ABI_VERSION;
    pub(crate) const STATE_OFFSET: usize = std::mem::offset_of!(Self, state);
    pub(crate) const RAM_BASE_OFFSET: usize = std::mem::offset_of!(Self, ram_base);
    pub(crate) const RAM_LEN_OFFSET: usize = std::mem::offset_of!(Self, ram_len);
    pub(crate) const PAGE_PERMISSIONS_OFFSET: usize = std::mem::offset_of!(Self, page_permissions);
    pub(crate) const PAGE_COUNT_OFFSET: usize = std::mem::offset_of!(Self, page_count);
    pub(crate) const REMAINING_BUDGET_OFFSET: usize = std::mem::offset_of!(Self, remaining_budget);
    pub(crate) const RESERVATION_VALID_OFFSET: usize =
        std::mem::offset_of!(Self, reservation_valid);
    pub(crate) const RESERVATION_ADDRESS_OFFSET: usize =
        std::mem::offset_of!(Self, reservation_address);
    pub(crate) const CHAIN_ATTEMPTED_OFFSET: usize = std::mem::offset_of!(Self, chain_attempted);
    pub(crate) const CHAIN_TRANSITIONS_OFFSET: usize =
        std::mem::offset_of!(Self, chain_transitions);
    pub(crate) const EXIT_OFFSET: usize = std::mem::offset_of!(Self, exit);
}

#[cfg(test)]
mod tests {
    use super::{DbtContext, DbtExitRecord, DbtExitTag};

    #[test]
    fn context_offsets_match_repr_c_layout() {
        assert_eq!(DbtContext::ABI_VERSION, 2);
        assert_eq!(
            crate::rv32im::Rv32ArchitecturalState::ABI_VERSION,
            DbtContext::ABI_VERSION
        );
        assert_eq!(
            DbtContext::STATE_OFFSET,
            std::mem::offset_of!(DbtContext, state)
        );
        assert_eq!(
            DbtContext::RAM_BASE_OFFSET,
            std::mem::offset_of!(DbtContext, ram_base)
        );
        assert_eq!(
            DbtContext::EXIT_OFFSET,
            std::mem::offset_of!(DbtContext, exit)
        );
        assert_eq!(
            DbtContext::CHAIN_ATTEMPTED_OFFSET,
            std::mem::offset_of!(DbtContext, chain_attempted)
        );
        assert_eq!(
            DbtContext::CHAIN_TRANSITIONS_OFFSET,
            std::mem::offset_of!(DbtContext, chain_transitions)
        );
        assert_eq!(std::mem::size_of::<DbtExitRecord>() % 4, 0);
        assert_eq!(DbtExitRecord::NEXT_PC_OFFSET, 0);
        assert_eq!(DbtExitRecord::ATTEMPTED_OFFSET, 4);
        assert_eq!(DbtExitRecord::INSTRUCTION_PC_OFFSET, 8);
        assert_eq!(DbtExitRecord::INSTRUCTION_WORD_OFFSET, 12);
        assert_eq!(DbtExitRecord::ADDRESS_OFFSET, 16);
        assert_eq!(DbtExitRecord::ACCESS_SIZE_OFFSET, 20);
    }

    #[test]
    fn raw_exit_tags_reject_unknown_values() {
        assert_eq!(DbtExitTag::try_from(0), Ok(DbtExitTag::Completed));
        assert_eq!(DbtExitTag::try_from(3), Ok(DbtExitTag::MemoryAccess));
        assert!(DbtExitTag::try_from(u32::MAX).is_err());
    }
}
