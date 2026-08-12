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
    reason = "the machine dispatcher consumes the per-VM DBT owner in the next issue #498 task"
)]

use super::Rv32DbtStats;
use crate::rv32_dbt::abi::{DbtContext, DbtEntry, DbtExitTag};
use crate::rv32_dbt::block::{DbtBlockInput, DbtBlockMode};
use crate::rv32_dbt::code_cache::{DbtCacheHandle, DbtCacheKey, DirectDbtCodeCache};
use crate::rv32_dbt::executable::ExecutableScratch;
use crate::rv32_dbt::x86_64::lower::DbtTranslationWorkspace;
use crate::rv32_dbt::{DbtFault, DbtFaultKind};
use crate::rv32im::Rv32ResolvedInstruction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rv32DbtPolicy {
    Direct,
    Cached { sets: usize, cache_bytes: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreparedLocation {
    Scratch { serial: u64 },
    Cache(DbtCacheHandle),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreparedDbtBlock {
    location: PreparedLocation,
    instruction_count: u32,
    lowered_load_sites: u32,
    lowered_store_sites: u32,
}

impl PreparedDbtBlock {
    pub(crate) const fn instruction_count(self) -> u32 {
        self.instruction_count
    }

    pub(crate) const fn lowered_load_sites(self) -> u32 {
        self.lowered_load_sites
    }

    pub(crate) const fn lowered_store_sites(self) -> u32 {
        self.lowered_store_sites
    }
}

enum Rv32DbtStorage {
    Direct {
        scratch: ExecutableScratch,
        serial: u64,
    },
    Cached {
        cache: DirectDbtCodeCache,
        bounded_scratch: ExecutableScratch,
        scratch_serial: u64,
    },
}

pub(crate) struct Rv32DbtExecution {
    max_instructions: usize,
    decoded: Vec<Rv32ResolvedInstruction>,
    workspace: DbtTranslationWorkspace,
    storage: Rv32DbtStorage,
    translations: u64,
    publications: u64,
    native_dispatches: u64,
    typed_slow_exits: u64,
    lowered_load_sites: u64,
    lowered_store_sites: u64,
    emitted_bytes: u64,
    decoded_slots_built: u64,
    generation: u64,
}

impl Rv32DbtExecution {
    pub(crate) fn new(
        policy: Rv32DbtPolicy,
        max_instructions: usize,
        scratch_bytes: usize,
    ) -> Result<Self, DbtFault> {
        if max_instructions == 0 || max_instructions > u32::MAX as usize {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                "DBT max instructions must be between 1 and u32::MAX",
            ));
        }
        if scratch_bytes == 0 || !scratch_bytes.is_multiple_of(4096) {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                "DBT scratch bytes must be a positive multiple of 4096",
            ));
        }
        if let Rv32DbtPolicy::Cached { sets, cache_bytes } = policy {
            if sets == 0 || sets > u32::MAX as usize {
                return Err(Self::fault(
                    DbtFaultKind::Capacity,
                    "DBT cache sets must be between 1 and u32::MAX",
                ));
            }
            if cache_bytes == 0 || !cache_bytes.is_multiple_of(4096) {
                return Err(Self::fault(
                    DbtFaultKind::Capacity,
                    "DBT cache bytes must be a positive multiple of 4096",
                ));
            }
        }

        let storage = match policy {
            Rv32DbtPolicy::Direct => Rv32DbtStorage::Direct {
                scratch: ExecutableScratch::new(scratch_bytes)?,
                serial: 0,
            },
            Rv32DbtPolicy::Cached { sets, cache_bytes } => Rv32DbtStorage::Cached {
                cache: DirectDbtCodeCache::new(sets, cache_bytes)?,
                bounded_scratch: ExecutableScratch::new(scratch_bytes)?,
                scratch_serial: 0,
            },
        };
        Ok(Self {
            max_instructions,
            decoded: Vec::with_capacity(max_instructions),
            workspace: DbtTranslationWorkspace::new(scratch_bytes, max_instructions)?,
            storage,
            translations: 0,
            publications: 0,
            native_dispatches: 0,
            typed_slow_exits: 0,
            lowered_load_sites: 0,
            lowered_store_sites: 0,
            emitted_bytes: 0,
            decoded_slots_built: 0,
            generation: 0,
        })
    }

    pub(crate) const fn max_instructions(&self) -> usize {
        self.max_instructions
    }

    pub(crate) fn decoded_slots_mut(&mut self) -> &mut Vec<Rv32ResolvedInstruction> {
        self.decoded.clear();
        &mut self.decoded
    }

    pub(crate) fn decoded_slots(&self) -> &[Rv32ResolvedInstruction] {
        &self.decoded
    }

    pub(crate) fn lookup(&mut self, pc: u32, mode: DbtBlockMode) -> Option<PreparedDbtBlock> {
        if mode != DbtBlockMode::Fast {
            return None;
        }
        let Rv32DbtStorage::Cached { cache, .. } = &mut self.storage else {
            return None;
        };
        let handle = cache.lookup(DbtCacheKey::new(pc, self.generation))?;
        Some(PreparedDbtBlock {
            location: PreparedLocation::Cache(handle),
            instruction_count: cache.instruction_count(handle)?,
            lowered_load_sites: cache.lowered_load_sites(handle)?,
            lowered_store_sites: cache.lowered_store_sites(handle)?,
        })
    }

    pub(crate) fn translate(
        &mut self,
        pc: u32,
        mode: DbtBlockMode,
    ) -> Result<PreparedDbtBlock, DbtFault> {
        if self.decoded.is_empty() || self.decoded.len() > self.max_instructions {
            return Err(Self::fault(
                DbtFaultKind::Translation,
                "DBT decoded block must contain between 1 and max_instructions slots",
            ));
        }
        let input = DbtBlockInput::new(pc, &self.decoded, mode)
            .map_err(|message| Self::fault(DbtFaultKind::Translation, message))?;
        let block = self.workspace.lower(&input)?;
        let instruction_count = block.instruction_count();
        let lowered_load_sites = block.lowered_load_sites();
        let lowered_store_sites = block.lowered_store_sites();
        let emitted_bytes = block.code().len() as u64;

        let location = match (&mut self.storage, mode) {
            (Rv32DbtStorage::Direct { scratch, serial }, _) => {
                scratch.publish(block.code())?;
                *serial = serial.saturating_add(1);
                PreparedLocation::Scratch { serial: *serial }
            }
            (
                Rv32DbtStorage::Cached {
                    bounded_scratch,
                    scratch_serial,
                    ..
                },
                DbtBlockMode::Bounded { .. },
            ) => {
                bounded_scratch.publish(block.code())?;
                *scratch_serial = scratch_serial.saturating_add(1);
                PreparedLocation::Scratch {
                    serial: *scratch_serial,
                }
            }
            (Rv32DbtStorage::Cached { cache, .. }, DbtBlockMode::Fast) => PreparedLocation::Cache(
                cache.publish(DbtCacheKey::new(pc, self.generation), &block)?,
            ),
        };

        self.translations = self.translations.saturating_add(1);
        self.publications = self.publications.saturating_add(1);
        self.lowered_load_sites = self
            .lowered_load_sites
            .saturating_add(u64::from(lowered_load_sites));
        self.lowered_store_sites = self
            .lowered_store_sites
            .saturating_add(u64::from(lowered_store_sites));
        self.emitted_bytes = self.emitted_bytes.saturating_add(emitted_bytes);
        self.decoded_slots_built = self
            .decoded_slots_built
            .saturating_add(self.decoded.len() as u64);
        Ok(PreparedDbtBlock {
            location,
            instruction_count,
            lowered_load_sites,
            lowered_store_sites,
        })
    }

    pub(crate) unsafe fn execute(
        &mut self,
        prepared: PreparedDbtBlock,
        context: &mut DbtContext,
    ) -> Result<DbtExitTag, DbtFault> {
        let address = match (prepared.location, &self.storage) {
            (
                PreparedLocation::Scratch { serial },
                Rv32DbtStorage::Direct {
                    scratch,
                    serial: live,
                },
            ) if serial == *live => scratch.entry_address(),
            (
                PreparedLocation::Scratch { serial },
                Rv32DbtStorage::Cached {
                    bounded_scratch,
                    scratch_serial,
                    ..
                },
            ) if serial == *scratch_serial => bounded_scratch.entry_address(),
            (PreparedLocation::Cache(handle), Rv32DbtStorage::Cached { cache, .. }) => {
                cache.entry_address(handle)
            }
            _ => None,
        }
        .ok_or_else(|| {
            Self::fault(
                DbtFaultKind::InvalidExit,
                "prepared DBT block was invalidated before execution",
            )
        })?;
        let entry: DbtEntry = unsafe { std::mem::transmute(address) };
        let raw_tag = unsafe { entry(context) };
        let tag = DbtExitTag::try_from(raw_tag)
            .map_err(|message| Self::fault(DbtFaultKind::InvalidExit, message))?;
        self.native_dispatches = self.native_dispatches.saturating_add(1);
        if matches!(tag, DbtExitTag::SlowInstruction | DbtExitTag::MemoryAccess) {
            self.typed_slow_exits = self.typed_slow_exits.saturating_add(1);
        }
        Ok(tag)
    }

    pub(crate) fn invalidate_all(&mut self) {
        self.generation = self.generation.saturating_add(1);
        if let Rv32DbtStorage::Cached { cache, .. } = &mut self.storage {
            cache.invalidate_all();
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        let stats = self.stats();
        stats
            .reserved_bytes
            .saturating_add(stats.metadata_bytes)
            .saturating_add(
                self.decoded
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Rv32ResolvedInstruction>()),
            )
            .saturating_add(self.workspace.retained_bytes())
    }

    pub(crate) fn stats(&self) -> Rv32DbtStats {
        let (cache_stats, reserved_bytes, metadata_bytes) = match &self.storage {
            Rv32DbtStorage::Direct { scratch, .. } => {
                (Default::default(), scratch.reserved_bytes(), 0)
            }
            Rv32DbtStorage::Cached {
                cache,
                bounded_scratch,
                ..
            } => (
                cache.stats(),
                cache
                    .reserved_bytes()
                    .saturating_add(bounded_scratch.reserved_bytes()),
                cache.metadata_bytes(),
            ),
        };
        Rv32DbtStats {
            translations: self.translations,
            publications: self.publications,
            hits: cache_stats.hits,
            misses: cache_stats.misses,
            evictions: cache_stats
                .metadata_evictions
                .saturating_add(cache_stats.overlap_invalidations),
            metadata_evictions: cache_stats.metadata_evictions,
            overlap_invalidations: cache_stats.overlap_invalidations,
            native_dispatches: self.native_dispatches,
            typed_slow_exits: self.typed_slow_exits,
            lowered_load_sites: self.lowered_load_sites,
            lowered_store_sites: self.lowered_store_sites,
            decoded_slots_built: self.decoded_slots_built,
            emitted_bytes: self.emitted_bytes,
            reserved_bytes,
            metadata_bytes,
        }
    }

    fn fault(kind: DbtFaultKind, message: impl Into<String>) -> DbtFault {
        DbtFault::new(kind, 0, None, message)
    }
}

#[cfg(test)]
mod tests {
    use super::{Rv32DbtExecution, Rv32DbtPolicy};
    use crate::rv32_dbt::abi::{DbtContext, DbtExitRecord, DbtExitTag};
    use crate::rv32_dbt::block::DbtBlockMode;
    use crate::rv32_dbt::DbtFaultKind;
    use crate::rv32im::{
        decode_product_word,
        encoding::{addi, lw, sw},
        Rv32ResolvedInstruction, Rv32imCpu,
    };

    fn fill_one(execution: &mut Rv32DbtExecution) {
        let word = addi(1, 1, 1);
        execution
            .decoded_slots_mut()
            .push(Rv32ResolvedInstruction::Valid {
                word,
                instruction: decode_product_word(word).unwrap(),
            });
    }

    fn fill_memory_pair(execution: &mut Rv32DbtExecution) {
        let slots = execution.decoded_slots_mut();
        for word in [lw(3, 1, 0), sw(1, 2, 0)] {
            slots.push(Rv32ResolvedInstruction::Valid {
                word,
                instruction: decode_product_word(word).unwrap(),
            });
        }
    }

    #[test]
    fn direct_and_cached_policies_share_one_lowerer() {
        let mut direct = Rv32DbtExecution::new(Rv32DbtPolicy::Direct, 8, 4096).unwrap();
        fill_one(&mut direct);
        let first = direct.translate(0x1000, DbtBlockMode::Fast).unwrap();
        fill_one(&mut direct);
        let second = direct.translate(0x1000, DbtBlockMode::Fast).unwrap();
        assert_ne!(first, second);
        assert_eq!(direct.stats().translations, 2);
        assert_eq!(direct.stats().publications, 2);

        let mut cached = Rv32DbtExecution::new(
            Rv32DbtPolicy::Cached {
                sets: 2,
                cache_bytes: 4096,
            },
            8,
            4096,
        )
        .unwrap();
        assert!(cached.lookup(0x1000, DbtBlockMode::Fast).is_none());
        fill_one(&mut cached);
        let published = cached.translate(0x1000, DbtBlockMode::Fast).unwrap();
        assert_eq!(cached.lookup(0x1000, DbtBlockMode::Fast), Some(published));
        assert_eq!(cached.stats().translations, 1);
        assert_eq!(cached.stats().publications, 1);
        assert_eq!(cached.stats().hits, 1);
        assert_eq!(cached.stats().misses, 1);
    }

    #[test]
    fn transient_scratch_and_persistent_cache_have_independent_capacities() {
        let direct = Rv32DbtExecution::new(Rv32DbtPolicy::Direct, 8, 8 * 1024).unwrap();
        assert_eq!(direct.stats().reserved_bytes, 16 * 1024);

        let cached = Rv32DbtExecution::new(
            Rv32DbtPolicy::Cached {
                sets: 2,
                cache_bytes: 16 * 1024,
            },
            8,
            8 * 1024,
        )
        .unwrap();
        assert_eq!(cached.stats().reserved_bytes, 48 * 1024);
    }

    #[test]
    fn bounded_blocks_never_enter_the_persistent_cache() {
        let mut cached = Rv32DbtExecution::new(
            Rv32DbtPolicy::Cached {
                sets: 2,
                cache_bytes: 4096,
            },
            8,
            4096,
        )
        .unwrap();
        fill_one(&mut cached);
        let second = cached.decoded_slots()[0];
        cached.decoded.push(second);
        cached
            .translate(0x1000, DbtBlockMode::Bounded { max_attempts: 1 })
            .unwrap();

        assert!(cached.lookup(0x1000, DbtBlockMode::Fast).is_none());
        assert_eq!(cached.stats().translations, 1);
        assert_eq!(cached.stats().publications, 1);
        assert_eq!(cached.stats().misses, 1);
    }

    #[test]
    fn cached_hits_retain_static_lowering_metadata() {
        let mut cached = Rv32DbtExecution::new(
            Rv32DbtPolicy::Cached {
                sets: 2,
                cache_bytes: 4096,
            },
            8,
            4096,
        )
        .unwrap();
        fill_memory_pair(&mut cached);
        let published = cached.translate(0x1000, DbtBlockMode::Fast).unwrap();
        let hit = cached.lookup(0x1000, DbtBlockMode::Fast).unwrap();

        assert_eq!(published.instruction_count(), 2);
        assert_eq!(published.lowered_load_sites(), 1);
        assert_eq!(published.lowered_store_sites(), 1);
        assert_eq!(hit, published);
        assert_eq!(cached.stats().lowered_load_sites, 1);
        assert_eq!(cached.stats().lowered_store_sites, 1);
    }

    #[test]
    fn republishing_direct_scratch_revokes_the_previous_handle() {
        let mut direct = Rv32DbtExecution::new(Rv32DbtPolicy::Direct, 8, 4096).unwrap();
        fill_one(&mut direct);
        let stale = direct.translate(0x1000, DbtBlockMode::Fast).unwrap();
        fill_one(&mut direct);
        direct.translate(0x1000, DbtBlockMode::Fast).unwrap();

        let mut cpu = Rv32imCpu::new(0x1000);
        let mut context = DbtContext {
            state: cpu.architectural_state_mut(),
            ram_base: std::ptr::null_mut(),
            ram_len: 0,
            page_permissions: std::ptr::null(),
            page_count: 0,
            remaining_budget: 1,
            reservation_valid: 0,
            reservation_address: 0,
            exit: DbtExitRecord::default(),
        };
        let error = unsafe { direct.execute(stale, &mut context) }.unwrap_err();

        assert_eq!(error.kind(), DbtFaultKind::InvalidExit);
        assert_eq!(direct.stats().native_dispatches, 0);
    }

    #[test]
    fn owner_executes_a_live_handle_without_exposing_its_entry_pointer() {
        let mut direct = Rv32DbtExecution::new(Rv32DbtPolicy::Direct, 8, 4096).unwrap();
        fill_one(&mut direct);
        let prepared = direct.translate(0x1000, DbtBlockMode::Fast).unwrap();
        let mut cpu = Rv32imCpu::new(0x1000);
        let mut ram = [0_u8; 4096];
        let permissions = [0b111_u8];
        let mut context = DbtContext {
            state: cpu.architectural_state_mut(),
            ram_base: ram.as_mut_ptr(),
            ram_len: ram.len() as u32,
            page_permissions: permissions.as_ptr(),
            page_count: permissions.len() as u32,
            remaining_budget: 1,
            reservation_valid: 0,
            reservation_address: 0,
            exit: DbtExitRecord::default(),
        };

        let tag = unsafe { direct.execute(prepared, &mut context) }.unwrap();

        assert_eq!(tag, DbtExitTag::Completed);
        assert_eq!(cpu.register(1), 1);
        assert_eq!(context.exit.attempted, 1);
        assert_eq!(direct.stats().native_dispatches, 1);
    }

    #[test]
    fn invalid_geometry_is_rejected_before_owner_construction() {
        for result in [
            Rv32DbtExecution::new(Rv32DbtPolicy::Direct, 0, 4096),
            Rv32DbtExecution::new(Rv32DbtPolicy::Direct, 8, 1),
            Rv32DbtExecution::new(
                Rv32DbtPolicy::Cached {
                    sets: 0,
                    cache_bytes: 4096,
                },
                8,
                4096,
            ),
        ] {
            assert_eq!(result.err().unwrap().kind(), DbtFaultKind::Capacity);
        }
    }
}
