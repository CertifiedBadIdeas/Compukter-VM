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

use super::block::{DbtBlockMode, TranslatedBlock};
use super::executable::ExecutableMapping;
use super::{DbtFault, DbtFaultKind};

const WAYS: usize = 2;
const CODE_ALIGNMENT: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DbtCacheKey {
    pc: u32,
    generation: u64,
}

impl DbtCacheKey {
    pub(crate) const fn new(pc: u32, generation: u64) -> Self {
        Self { pc, generation }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DbtCacheHandle {
    set: u32,
    way: u8,
    serial: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DbtCodeCacheStats {
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) publications: u64,
    pub(crate) metadata_evictions: u64,
    pub(crate) overlap_invalidations: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct CacheEntry {
    valid: bool,
    key: Option<DbtCacheKey>,
    offset: usize,
    length: usize,
    instruction_count: u32,
    last_used: u64,
    serial: u64,
}

pub(crate) struct DirectDbtCodeCache {
    mapping: ExecutableMapping,
    sets: Vec<[CacheEntry; WAYS]>,
    write_cursor: usize,
    clock: u64,
    next_serial: u64,
    stats: DbtCodeCacheStats,
}

impl DirectDbtCodeCache {
    pub(crate) fn new(sets: usize, executable_bytes: usize) -> Result<Self, DbtFault> {
        if sets == 0 || sets > u32::MAX as usize {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                "DBT code cache requires between 1 and u32::MAX metadata sets",
            ));
        }
        Ok(Self {
            mapping: ExecutableMapping::new(executable_bytes)?,
            sets: vec![[CacheEntry::default(); WAYS]; sets],
            write_cursor: 0,
            clock: 0,
            next_serial: 0,
            stats: DbtCodeCacheStats::default(),
        })
    }

    pub(crate) fn lookup(&mut self, key: DbtCacheKey) -> Option<DbtCacheHandle> {
        let set = self.set_index(key);
        let way = self.sets[set]
            .iter()
            .position(|entry| entry.valid && entry.key == Some(key));
        let Some(way) = way else {
            self.stats.misses = self.stats.misses.saturating_add(1);
            return None;
        };
        self.clock = self.clock.saturating_add(1);
        let entry = &mut self.sets[set][way];
        entry.last_used = self.clock;
        self.stats.hits = self.stats.hits.saturating_add(1);
        Some(DbtCacheHandle {
            set: set as u32,
            way: way as u8,
            serial: entry.serial,
        })
    }

    pub(crate) fn publish(
        &mut self,
        key: DbtCacheKey,
        block: &TranslatedBlock<'_>,
    ) -> Result<DbtCacheHandle, DbtFault> {
        if block.mode() != DbtBlockMode::Fast {
            return Err(Self::fault(
                DbtFaultKind::Translation,
                "only Fast DBT blocks may enter the persistent code cache",
            ));
        }
        if block.start_pc() != key.pc {
            return Err(Self::fault(
                DbtFaultKind::Translation,
                "DBT code-cache key PC disagrees with compiled block PC",
            ));
        }
        if block.code().len() > self.mapping.capacity() {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                format!(
                    "native block requires {} bytes but code cache capacity is {} bytes",
                    block.code().len(),
                    self.mapping.capacity()
                ),
            ));
        }

        let aligned = align_up(self.write_cursor, CODE_ALIGNMENT);
        let offset = if aligned
            .checked_add(block.code().len())
            .is_some_and(|end| end <= self.mapping.capacity())
        {
            aligned
        } else {
            0
        };
        let end = offset + block.code().len();
        self.invalidate_overlapping(offset, end);
        self.mapping.publish_at(offset, block.code())?;

        let set = self.set_index(key);
        let way = self.select_way(set);
        if self.sets[set][way].valid {
            self.stats.metadata_evictions = self.stats.metadata_evictions.saturating_add(1);
        }
        self.clock = self.clock.saturating_add(1);
        self.next_serial = self.next_serial.saturating_add(1);
        let serial = self.next_serial;
        self.sets[set][way] = CacheEntry {
            valid: true,
            key: Some(key),
            offset,
            length: block.code().len(),
            instruction_count: block.instruction_count(),
            last_used: self.clock,
            serial,
        };
        self.write_cursor = end;
        self.stats.publications = self.stats.publications.saturating_add(1);
        Ok(DbtCacheHandle {
            set: set as u32,
            way: way as u8,
            serial,
        })
    }

    pub(crate) fn entry_address(&self, handle: DbtCacheHandle) -> Option<*const u8> {
        let entry = self
            .sets
            .get(handle.set as usize)?
            .get(handle.way as usize)?;
        if !entry.valid || entry.serial != handle.serial {
            return None;
        }
        self.mapping.entry_address(entry.offset)
    }

    pub(crate) fn instruction_count(&self, handle: DbtCacheHandle) -> Option<u32> {
        let entry = self
            .sets
            .get(handle.set as usize)?
            .get(handle.way as usize)?;
        (entry.valid && entry.serial == handle.serial).then_some(entry.instruction_count)
    }

    pub(crate) fn invalidate_all(&mut self) {
        for set in &mut self.sets {
            for entry in set {
                entry.valid = false;
            }
        }
    }

    pub(crate) const fn stats(&self) -> DbtCodeCacheStats {
        self.stats
    }

    pub(crate) const fn reserved_bytes(&self) -> usize {
        self.mapping.reserved_bytes()
    }

    pub(crate) fn metadata_bytes(&self) -> usize {
        self.sets.len() * std::mem::size_of::<[CacheEntry; WAYS]>()
    }

    pub(crate) fn live_entry_bytes(&self) -> usize {
        self.sets
            .iter()
            .flatten()
            .filter(|entry| entry.valid)
            .map(|entry| entry.length)
            .sum()
    }

    fn set_index(&self, key: DbtCacheKey) -> usize {
        let pc = u64::from(key.pc >> 2);
        let mixed = pc ^ key.generation ^ key.generation.rotate_left(23);
        (mixed as usize) % self.sets.len()
    }

    fn select_way(&self, set: usize) -> usize {
        self.sets[set]
            .iter()
            .position(|entry| !entry.valid)
            .unwrap_or_else(|| {
                self.sets[set]
                    .iter()
                    .enumerate()
                    .min_by_key(|(way, entry)| (entry.last_used, *way))
                    .map(|(way, _)| way)
                    .unwrap()
            })
    }

    fn invalidate_overlapping(&mut self, start: usize, end: usize) {
        for entry in self.sets.iter_mut().flatten() {
            if entry.valid && ranges_overlap(start, end, entry.offset, entry.offset + entry.length)
            {
                entry.valid = false;
                self.stats.overlap_invalidations =
                    self.stats.overlap_invalidations.saturating_add(1);
            }
        }
    }

    fn fault(kind: DbtFaultKind, message: impl Into<String>) -> DbtFault {
        DbtFault::new(kind, 0, None, message)
    }
}

fn align_up(value: usize, alignment: usize) -> usize {
    value
        .checked_add(alignment - 1)
        .map_or(usize::MAX, |value| value & !(alignment - 1))
}

fn ranges_overlap(lhs_start: usize, lhs_end: usize, rhs_start: usize, rhs_end: usize) -> bool {
    lhs_start < rhs_end && rhs_start < lhs_end
}

#[cfg(test)]
mod tests {
    use super::{DbtCacheKey, DirectDbtCodeCache};
    use crate::rv32_dbt::block::{DbtBlockInput, DbtBlockMode, TranslatedBlock};
    use crate::rv32_dbt::DbtFaultKind;
    use crate::rv32im::{decode_product_word, encoding::addi, Rv32ResolvedInstruction};

    const PAGE_BYTES: usize = 4096;

    fn block(pc: u32, code: &[u8]) -> TranslatedBlock<'_> {
        let word = addi(1, 1, 1);
        let slots = [Rv32ResolvedInstruction::Valid {
            word,
            instruction: decode_product_word(word).unwrap(),
        }];
        let input = DbtBlockInput::new(pc, &slots, DbtBlockMode::Fast).unwrap();
        TranslatedBlock::new(&input, code, 0, 0).unwrap()
    }

    fn bounded_block(pc: u32) -> TranslatedBlock<'static> {
        let word = addi(1, 1, 1);
        let slot = Rv32ResolvedInstruction::Valid {
            word,
            instruction: decode_product_word(word).unwrap(),
        };
        let slots = [slot, slot];
        let input =
            DbtBlockInput::new(pc, &slots, DbtBlockMode::Bounded { max_attempts: 1 }).unwrap();
        TranslatedBlock::new(&input, &[0xc3], 0, 0).unwrap()
    }

    #[test]
    fn resident_key_hits_but_another_generation_misses() {
        let mut cache = DirectDbtCodeCache::new(2, PAGE_BYTES).unwrap();
        let key = DbtCacheKey::new(0x1000, 7);

        assert!(cache.lookup(key).is_none());
        let handle = cache.publish(key, &block(0x1000, &[0xc3])).unwrap();

        assert_eq!(cache.lookup(key), Some(handle));
        assert!(cache.lookup(DbtCacheKey::new(0x1000, 8)).is_none());
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 2);
        assert_eq!(cache.stats().publications, 1);
        assert_eq!(cache.reserved_bytes(), PAGE_BYTES);
    }

    #[test]
    fn circular_overwrite_invalidates_only_overlapping_entries() {
        let mut cache = DirectDbtCodeCache::new(4, PAGE_BYTES).unwrap();
        let first = DbtCacheKey::new(0, 0);
        let second = DbtCacheKey::new(4, 0);
        let wrapping = DbtCacheKey::new(8, 0);

        cache.publish(first, &block(0, &vec![0x90; 1024])).unwrap();
        cache.publish(second, &block(4, &vec![0x90; 3000])).unwrap();
        cache
            .publish(wrapping, &block(8, &vec![0x90; 512]))
            .unwrap();

        assert!(cache.lookup(first).is_none());
        assert!(cache.lookup(second).is_some());
        assert!(cache.lookup(wrapping).is_some());
        assert_eq!(cache.stats().overlap_invalidations, 1);
    }

    #[test]
    fn two_way_conflict_evicts_the_least_recently_used_entry() {
        let mut cache = DirectDbtCodeCache::new(2, PAGE_BYTES).unwrap();
        let first = DbtCacheKey::new(0, 0);
        let second = DbtCacheKey::new(8, 0);
        let third = DbtCacheKey::new(16, 0);

        let first_handle = cache.publish(first, &block(0, &[0xc3])).unwrap();
        cache.publish(second, &block(8, &[0xc3])).unwrap();
        assert_eq!(cache.lookup(first), Some(first_handle));
        cache.publish(third, &block(16, &[0xc3])).unwrap();

        assert!(cache.lookup(first).is_some());
        assert!(cache.lookup(second).is_none());
        assert!(cache.lookup(third).is_some());
        assert_eq!(cache.stats().metadata_evictions, 1);
    }

    #[test]
    fn invalidation_revokes_handles_without_releasing_fixed_storage() {
        let mut cache = DirectDbtCodeCache::new(2, PAGE_BYTES).unwrap();
        let key = DbtCacheKey::new(0, 0);
        let handle = cache.publish(key, &block(0, &[0xc3])).unwrap();
        let metadata_bytes = cache.metadata_bytes();

        assert!(cache.entry_address(handle).is_some());
        assert_eq!(cache.instruction_count(handle), Some(1));
        assert_eq!(cache.live_entry_bytes(), 1);
        cache.invalidate_all();

        assert!(cache.entry_address(handle).is_none());
        assert_eq!(cache.instruction_count(handle), None);
        assert_eq!(cache.live_entry_bytes(), 0);
        assert_eq!(cache.metadata_bytes(), metadata_bytes);
        assert_eq!(cache.reserved_bytes(), PAGE_BYTES);
    }

    #[test]
    fn rejects_non_fast_mismatched_and_oversized_blocks() {
        let mut cache = DirectDbtCodeCache::new(1, PAGE_BYTES).unwrap();

        assert_eq!(
            cache
                .publish(DbtCacheKey::new(0, 0), &bounded_block(0))
                .unwrap_err()
                .kind(),
            DbtFaultKind::Translation
        );
        assert_eq!(
            cache
                .publish(DbtCacheKey::new(4, 0), &block(0, &[0xc3]))
                .unwrap_err()
                .kind(),
            DbtFaultKind::Translation
        );
        assert_eq!(
            cache
                .publish(
                    DbtCacheKey::new(0, 0),
                    &block(0, &vec![0x90; PAGE_BYTES + 1]),
                )
                .unwrap_err()
                .kind(),
            DbtFaultKind::Capacity
        );
    }

    #[test]
    fn independently_owned_caches_never_share_entries() {
        let key = DbtCacheKey::new(0x1000, 0);
        let mut first = DirectDbtCodeCache::new(1, PAGE_BYTES).unwrap();
        let mut second = DirectDbtCodeCache::new(1, PAGE_BYTES).unwrap();

        first.publish(key, &block(0x1000, &[0xc3])).unwrap();

        assert!(first.lookup(key).is_some());
        assert!(second.lookup(key).is_none());
    }
}
