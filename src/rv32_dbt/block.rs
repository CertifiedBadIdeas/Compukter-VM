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

use crate::rv32_dbt::ir::{may_exit_before_write, register_effects, DbtIrBlock, FutureValue};
#[cfg(feature = "dbt-execution-profile")]
use crate::rv32_dbt::profile::DbtProfileKey;
use crate::rv32im::DecodedFields;
use crate::rv32im::Rv32ResolvedInstruction;

pub(crate) const MAX_STATIC_LINKS: usize = 2;
pub(crate) const MAX_COLD_EXIT_RELOCATIONS: usize = MAX_STATIC_LINKS + 1;
#[cfg(feature = "dbt-execution-profile")]
pub(crate) const MAX_PROFILE_RELOCATIONS: usize = MAX_STATIC_LINKS + 1;

#[cfg(feature = "dbt-execution-profile")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DbtProfileRelocation {
    pub(crate) key: DbtProfileKey,
    pub(crate) count_address_offset: u32,
    pub(crate) overflow_address_offset: u32,
}

#[cfg(feature = "dbt-execution-profile")]
impl DbtProfileRelocation {
    pub(crate) const EMPTY: Self = Self {
        key: DbtProfileKey::Block { pc: 0 },
        count_address_offset: 0,
        overflow_address_offset: 0,
    };
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DbtColdExitRelocation {
    pub(crate) displacement_offset: u32,
}

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

#[derive(Debug, Clone, Copy)]
pub(crate) struct DbtBlockInput<'a> {
    start_pc: u32,
    source: DbtBlockSource<'a>,
    mode: DbtBlockMode,
}

#[derive(Debug, Clone, Copy)]
enum DbtBlockSource<'a> {
    Decoded(&'a [Rv32ResolvedInstruction]),
    MicroIr(&'a DbtIrBlock),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DbtLoweringInstruction {
    word: u32,
    fields: DecodedFields,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DbtFutureValues<'a> {
    input: DbtBlockInput<'a>,
    index: usize,
}

impl DbtFutureValues<'_> {
    pub(crate) fn value(self, guest: usize) -> FutureValue {
        match self.input.source {
            DbtBlockSource::MicroIr(ir) => ir.future_value(self.index, guest),
            DbtBlockSource::Decoded(_) => {
                let mut crossed_exit = false;
                for distance in 0..self.input.instruction_count().saturating_sub(self.index) {
                    let Some(instruction) = self.input.instruction(self.index + distance) else {
                        crossed_exit = true;
                        continue;
                    };
                    let (reads, write) = register_effects(instruction.fields());
                    if reads
                        .into_iter()
                        .flatten()
                        .any(|read| usize::from(read) == guest)
                    {
                        return FutureValue::Read(distance as u8);
                    }
                    crossed_exit |= may_exit_before_write(instruction.fields().operation);
                    if write.is_some_and(|write| usize::from(write) == guest) {
                        return if crossed_exit {
                            FutureValue::Read(distance as u8)
                        } else {
                            FutureValue::Dead(distance as u8)
                        };
                    }
                }
                FutureValue::Unused
            }
        }
    }
}

impl DbtLoweringInstruction {
    pub(crate) const fn word(self) -> u32 {
        self.word
    }

    pub(crate) const fn fields(self) -> DecodedFields {
        self.fields
    }
}

impl<'a> DbtBlockInput<'a> {
    pub(crate) fn new(
        start_pc: u32,
        slots: &'a [Rv32ResolvedInstruction],
        mode: DbtBlockMode,
    ) -> Result<Self, String> {
        Self::new_source(start_pc, DbtBlockSource::Decoded(slots), mode)
    }

    pub(crate) fn new_ir(
        start_pc: u32,
        ir: &'a DbtIrBlock,
        mode: DbtBlockMode,
    ) -> Result<Self, String> {
        Self::new_source(start_pc, DbtBlockSource::MicroIr(ir), mode)
    }

    fn new_source(
        start_pc: u32,
        source: DbtBlockSource<'a>,
        mode: DbtBlockMode,
    ) -> Result<Self, String> {
        let instruction_count = match source {
            DbtBlockSource::Decoded(slots) => slots.len(),
            DbtBlockSource::MicroIr(ir) => ir.attempted_instruction_count(),
        };
        if instruction_count == 0 {
            return Err("RV32 DBT block cannot be empty".to_string());
        }
        if let DbtBlockMode::Bounded { max_attempts } = mode {
            if max_attempts == 0 || max_attempts as usize >= instruction_count {
                return Err(format!(
                    "RV32 DBT bounded attempt count {max_attempts} must be inside 1..{}",
                    instruction_count
                ));
            }
        }
        Ok(Self {
            start_pc,
            source,
            mode,
        })
    }

    pub(crate) fn start_pc(&self) -> u32 {
        self.start_pc
    }

    pub(crate) fn slots(&self) -> &[Rv32ResolvedInstruction] {
        match self.source {
            DbtBlockSource::Decoded(slots) => slots,
            DbtBlockSource::MicroIr(_) => {
                panic!("micro-IR DBT input does not expose decoded slots")
            }
        }
    }

    pub(crate) fn instruction_count(&self) -> usize {
        match self.source {
            DbtBlockSource::Decoded(slots) => slots.len(),
            DbtBlockSource::MicroIr(ir) => ir.attempted_instruction_count(),
        }
    }

    pub(crate) fn instruction(&self, index: usize) -> Option<DbtLoweringInstruction> {
        match self.source {
            DbtBlockSource::Decoded(slots) => match *slots.get(index)? {
                Rv32ResolvedInstruction::Valid { word, instruction } => {
                    Some(DbtLoweringInstruction {
                        word,
                        fields: DecodedFields::from_instruction(instruction),
                    })
                }
                Rv32ResolvedInstruction::Invalid { .. } => None,
            },
            DbtBlockSource::MicroIr(ir) => {
                ir.instructions()
                    .get(index)
                    .copied()
                    .map(|instruction| DbtLoweringInstruction {
                        word: instruction.word(),
                        fields: instruction.fields(),
                    })
            }
        }
    }

    pub(crate) fn word(&self, index: usize) -> Option<u32> {
        match self.source {
            DbtBlockSource::Decoded(slots) => match *slots.get(index)? {
                Rv32ResolvedInstruction::Valid { word, .. }
                | Rv32ResolvedInstruction::Invalid { word } => Some(word),
            },
            DbtBlockSource::MicroIr(ir) => ir
                .instructions()
                .get(index)
                .map(|instruction| instruction.word())
                .or_else(|| {
                    (index == ir.instructions().len())
                        .then(|| ir.invalid_word())
                        .flatten()
                }),
        }
    }

    pub(crate) const fn uses_micro_ir(&self) -> bool {
        matches!(self.source, DbtBlockSource::MicroIr(_))
    }

    pub(crate) const fn micro_ir(&self) -> Option<&'a DbtIrBlock> {
        match self.source {
            DbtBlockSource::MicroIr(ir) => Some(ir),
            DbtBlockSource::Decoded(_) => None,
        }
    }

    pub(crate) const fn future_values(&self, index: usize) -> DbtFutureValues<'a> {
        DbtFutureValues {
            input: *self,
            index,
        }
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
    local_self_backedge_sites: u32,
    chain_entry_offset: u32,
    static_links: [DbtStaticLink; MAX_STATIC_LINKS],
    static_link_count: u8,
    cold_exit_relocations: [DbtColdExitRelocation; MAX_COLD_EXIT_RELOCATIONS],
    cold_exit_relocation_count: u8,
    #[cfg(feature = "dbt-execution-profile")]
    profile_relocations: [DbtProfileRelocation; MAX_PROFILE_RELOCATIONS],
    #[cfg(feature = "dbt-execution-profile")]
    profile_relocation_count: u8,
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
        cold_exit_relocations: &[DbtColdExitRelocation],
    ) -> Result<Self, String> {
        Self::new_inner(
            input,
            code,
            lowered_load_sites,
            lowered_store_sites,
            chain_entry_offset,
            static_links,
            cold_exit_relocations,
            #[cfg(feature = "dbt-execution-profile")]
            &[],
        )
    }

    #[cfg(feature = "dbt-execution-profile")]
    pub(crate) fn new_profiled(
        input: &DbtBlockInput<'_>,
        code: &'a [u8],
        lowered_load_sites: u32,
        lowered_store_sites: u32,
        chain_entry_offset: u32,
        static_links: &[DbtStaticLink],
        cold_exit_relocations: &[DbtColdExitRelocation],
        profile_relocations: &[DbtProfileRelocation],
    ) -> Result<Self, String> {
        Self::new_inner(
            input,
            code,
            lowered_load_sites,
            lowered_store_sites,
            chain_entry_offset,
            static_links,
            cold_exit_relocations,
            profile_relocations,
        )
    }

    fn new_inner(
        input: &DbtBlockInput<'_>,
        code: &'a [u8],
        lowered_load_sites: u32,
        lowered_store_sites: u32,
        chain_entry_offset: u32,
        static_links: &[DbtStaticLink],
        cold_exit_relocations: &[DbtColdExitRelocation],
        #[cfg(feature = "dbt-execution-profile")] profile_relocations: &[DbtProfileRelocation],
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
        if cold_exit_relocations.len() > MAX_COLD_EXIT_RELOCATIONS {
            return Err(format!(
                "RV32 DBT block has {} cold exits but supports at most {MAX_COLD_EXIT_RELOCATIONS}",
                cold_exit_relocations.len()
            ));
        }
        if input.mode() != DbtBlockMode::ChainableThroughput && !cold_exit_relocations.is_empty() {
            return Err("only RV32 DBT chainable blocks can expose cold exits".to_string());
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
        for relocation in cold_exit_relocations {
            let displacement_end = (relocation.displacement_offset as usize)
                .checked_add(4)
                .ok_or_else(|| "RV32 DBT cold exit displacement range overflowed".to_string())?;
            if displacement_end > code.len() {
                return Err("RV32 DBT cold exit relocation lies outside emitted code".to_string());
            }
        }
        let mut stored_cold_exits = [DbtColdExitRelocation::default(); MAX_COLD_EXIT_RELOCATIONS];
        stored_cold_exits[..cold_exit_relocations.len()].copy_from_slice(cold_exit_relocations);
        #[cfg(feature = "dbt-execution-profile")]
        let stored_profile_relocations = {
            if profile_relocations.len() > MAX_PROFILE_RELOCATIONS {
                return Err(format!(
                    "RV32 DBT block has {} profile relocations but supports at most {MAX_PROFILE_RELOCATIONS}",
                    profile_relocations.len()
                ));
            }
            for relocation in profile_relocations {
                for offset in [
                    relocation.count_address_offset,
                    relocation.overflow_address_offset,
                ] {
                    let end = (offset as usize).checked_add(8).ok_or_else(|| {
                        "RV32 DBT profile relocation range overflowed".to_string()
                    })?;
                    if end > code.len() {
                        return Err(
                            "RV32 DBT profile relocation lies outside emitted code".to_string()
                        );
                    }
                }
            }
            let mut stored = [DbtProfileRelocation::EMPTY; MAX_PROFILE_RELOCATIONS];
            stored[..profile_relocations.len()].copy_from_slice(profile_relocations);
            stored
        };
        Ok(Self {
            start_pc: input.start_pc(),
            instruction_count: input.instruction_count() as u32,
            mode: input.mode(),
            lowered_load_sites,
            lowered_store_sites,
            local_self_backedge_sites: 0,
            chain_entry_offset,
            static_links: stored_links,
            static_link_count: static_links.len() as u8,
            cold_exit_relocations: stored_cold_exits,
            cold_exit_relocation_count: cold_exit_relocations.len() as u8,
            #[cfg(feature = "dbt-execution-profile")]
            profile_relocations: stored_profile_relocations,
            #[cfg(feature = "dbt-execution-profile")]
            profile_relocation_count: profile_relocations.len() as u8,
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

    pub(crate) fn with_local_self_backedge(mut self) -> Self {
        self.local_self_backedge_sites = 1;
        self
    }

    pub(crate) fn local_self_backedge_sites(&self) -> u32 {
        self.local_self_backedge_sites
    }

    pub(crate) fn chain_entry_offset(&self) -> u32 {
        self.chain_entry_offset
    }

    pub(crate) fn static_links(&self) -> &[DbtStaticLink] {
        &self.static_links[..self.static_link_count as usize]
    }

    pub(crate) fn cold_exit_relocations(&self) -> &[DbtColdExitRelocation] {
        &self.cold_exit_relocations[..self.cold_exit_relocation_count as usize]
    }

    #[cfg(feature = "dbt-execution-profile")]
    pub(crate) fn profile_relocations(&self) -> &[DbtProfileRelocation] {
        &self.profile_relocations[..self.profile_relocation_count as usize]
    }

    pub(crate) fn code(&self) -> &[u8] {
        self.code
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "dbt-execution-profile")]
    use super::DbtProfileRelocation;
    use super::{
        DbtBlockInput, DbtBlockMode, DbtColdExitRelocation, DbtLinkKind, DbtStaticLink,
        TranslatedBlock,
    };
    use crate::rv32_dbt::ir::DbtIrBlock;
    #[cfg(feature = "dbt-execution-profile")]
    use crate::rv32_dbt::profile::DbtProfileKey;
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
    fn block_input_accepts_analyzed_micro_ir_without_decoded_slots() {
        let mut ir = DbtIrBlock::new(2).unwrap();
        ir.lift_word(addi(1, 1, 1)).unwrap();
        ir.lift_word(addi(2, 1, 1)).unwrap();
        ir.analyze_future_values();

        let input = DbtBlockInput::new_ir(0x1000, &ir, DbtBlockMode::DirectFast).unwrap();

        assert_eq!(input.instruction_count(), 2);
        assert_eq!(input.instruction(0).unwrap().word(), addi(1, 1, 1));
        assert!(input.uses_micro_ir());
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
        let block = TranslatedBlock::new(&input, &code, 3, 2, 0, &[], &[]).unwrap();

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

        assert!(TranslatedBlock::new(&input, &code, 0, 0, 6, &[], &[]).is_err());
        assert!(TranslatedBlock::new(&input, &code, 0, 0, 0, &[invalid], &[]).is_err());
    }

    #[test]
    fn cold_exit_relocations_are_chainable_and_bounded_by_code() {
        let slots = [slot()];
        let chainable =
            DbtBlockInput::new(0x1000, &slots, DbtBlockMode::ChainableThroughput).unwrap();
        let direct = DbtBlockInput::new(0x1000, &slots, DbtBlockMode::DirectFast).unwrap();
        let code = [0xe9, 0, 0, 0, 0, 0xc3];
        let cold = [DbtColdExitRelocation {
            displacement_offset: 1,
        }];

        let block = TranslatedBlock::new(&chainable, &code, 0, 0, 0, &[], &cold).unwrap();
        assert_eq!(block.cold_exit_relocations(), &cold);
        assert!(TranslatedBlock::new(&direct, &code, 0, 0, 0, &[], &cold).is_err());
        assert!(TranslatedBlock::new(
            &chainable,
            &code,
            0,
            0,
            0,
            &[],
            &[DbtColdExitRelocation {
                displacement_offset: 3,
            }],
        )
        .is_err());
    }

    #[cfg(feature = "dbt-execution-profile")]
    #[test]
    fn profiled_block_keeps_bounded_counter_relocations() {
        let slots = [Rv32ResolvedInstruction::Valid {
            word: addi(1, 0, 1),
            instruction: decode_product_word(addi(1, 0, 1)).unwrap(),
        }];
        let input = DbtBlockInput::new(0x1000, &slots, DbtBlockMode::ChainableThroughput).unwrap();
        let relocation = DbtProfileRelocation {
            key: DbtProfileKey::Block { pc: 0x1000 },
            count_address_offset: 2,
            overflow_address_offset: 12,
        };
        let block =
            TranslatedBlock::new_profiled(&input, &[0x90; 24], 0, 0, 0, &[], &[], &[relocation])
                .unwrap();
        assert_eq!(block.profile_relocations(), &[relocation]);
    }
}
