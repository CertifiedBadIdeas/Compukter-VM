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

#[cfg(feature = "dbt-code-audit")]
use super::block::MAX_COLD_EXIT_RELOCATIONS;
#[cfg(feature = "dbt-execution-profile")]
use super::block::MAX_PROFILE_RELOCATIONS;
use super::block::{DbtBlockMode, TranslatedBlock, MAX_STATIC_LINKS};
use super::executable::ExecutableMapping;
use super::x86_64::cold_exit::build_completed_exit_stub;
#[cfg(feature = "dbt-code-audit")]
use super::x86_64::register_cache::RegisterPressureAudit;
use super::{DbtFault, DbtFaultKind};
#[cfg(feature = "dbt-execution-profile")]
use crate::rv32_dbt::profile::ExactDbtProfile;
use crate::rv32_machine::{Rv32DbtCodeAlignment, DEFAULT_DBT_CODE_ALIGNMENT};
#[cfg(feature = "dbt-code-audit")]
use crate::rv32_machine::{
    Rv32DbtCodeBlock, Rv32DbtCodeEdge, Rv32DbtCodeSnapshot, Rv32DbtCodeSnapshotError,
    Rv32DbtSupportCodeKind, Rv32DbtSupportCodeRange,
};

const WAYS: usize = 2;
const GUEST_PAGE_BYTES: u32 = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
pub(crate) struct DbtCacheHit {
    entry: *const u8,
    instruction_count: u32,
}

impl DbtCacheHit {
    pub(crate) const fn entry(self) -> *const u8 {
        self.entry
    }

    pub(crate) const fn instruction_count(self) -> u32 {
        self.instruction_count
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DbtCodeCacheStats {
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) publications: u64,
    pub(crate) metadata_evictions: u64,
    pub(crate) overlap_invalidations: u64,
    pub(crate) links_established: u64,
    pub(crate) links_reset: u64,
    pub(crate) alignment_padding_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct CacheLink {
    target_pc: u32,
    displacement_offset: u32,
    reset_target_offset: u32,
    linked: bool,
}

#[derive(Clone, Copy, Debug)]
struct CacheEntry {
    valid: bool,
    key: DbtCacheKey,
    offset: usize,
    length: usize,
    instruction_count: u32,
    chain_entry_offset: u32,
    links: [CacheLink; MAX_STATIC_LINKS],
    link_count: u8,
    #[cfg(feature = "dbt-code-audit")]
    cold_exit_displacements: [u32; MAX_COLD_EXIT_RELOCATIONS],
    #[cfg(feature = "dbt-code-audit")]
    cold_exit_count: u8,
}

impl Default for CacheEntry {
    fn default() -> Self {
        Self {
            valid: false,
            key: DbtCacheKey::default(),
            offset: 0,
            length: 0,
            instruction_count: 0,
            chain_entry_offset: 0,
            links: [CacheLink::default(); MAX_STATIC_LINKS],
            link_count: 0,
            #[cfg(feature = "dbt-code-audit")]
            cold_exit_displacements: [0; MAX_COLD_EXIT_RELOCATIONS],
            #[cfg(feature = "dbt-code-audit")]
            cold_exit_count: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct CacheSet {
    ways: [CacheEntry; WAYS],
    mru_way: u8,
}

pub(crate) struct DirectDbtCodeCache {
    mapping: ExecutableMapping,
    sets: Vec<CacheSet>,
    set_mask: usize,
    write_cursor: usize,
    completed_exit_stub_offset: usize,
    completed_exit_stub_len: usize,
    block_region_start: usize,
    code_alignment: Rv32DbtCodeAlignment,
    stats: DbtCodeCacheStats,
    #[cfg(feature = "dbt-code-audit")]
    register_pressure: Vec<(DbtCacheKey, RegisterPressureAudit)>,
}

impl DirectDbtCodeCache {
    pub(crate) fn new(sets: usize, executable_bytes: usize) -> Result<Self, DbtFault> {
        Self::new_with_alignment(sets, executable_bytes, DEFAULT_DBT_CODE_ALIGNMENT)
    }

    pub(crate) fn new_with_alignment(
        sets: usize,
        executable_bytes: usize,
        code_alignment: Rv32DbtCodeAlignment,
    ) -> Result<Self, DbtFault> {
        if sets == 0 || sets > u32::MAX as usize || !sets.is_power_of_two() {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                "DBT code cache requires a power-of-two metadata set count between 1 and u32::MAX",
            ));
        }
        let alignment_bytes = code_alignment.bytes();
        if !alignment_bytes.is_power_of_two() || !(16..=256).contains(&alignment_bytes) {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                "DBT code alignment must be a power of two between 16 and 256 bytes",
            ));
        }
        let mut mapping = ExecutableMapping::new(executable_bytes)?;
        let stub = build_completed_exit_stub()
            .map_err(|error| Self::fault(DbtFaultKind::Translation, error.to_string()))?;
        mapping.publish_at(0, &stub)?;
        let block_region_start = stub.len();
        if block_region_start >= executable_bytes {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                "DBT code cache cannot contain support code and a guest block",
            ));
        }
        Ok(Self {
            mapping,
            sets: vec![CacheSet::default(); sets],
            set_mask: sets - 1,
            write_cursor: block_region_start,
            completed_exit_stub_offset: 0,
            completed_exit_stub_len: stub.len(),
            block_region_start,
            code_alignment,
            stats: DbtCodeCacheStats::default(),
            #[cfg(feature = "dbt-code-audit")]
            register_pressure: Vec::new(),
        })
    }

    pub(crate) fn completed_exit_stub_range(&self) -> std::ops::Range<usize> {
        self.completed_exit_stub_offset
            ..self.completed_exit_stub_offset + self.completed_exit_stub_len
    }

    pub(crate) const fn block_region_start(&self) -> usize {
        self.block_region_start
    }

    pub(crate) fn lookup(&mut self, key: DbtCacheKey) -> Option<DbtCacheHit> {
        let set = self.set_index(key);
        let way = self.sets[set]
            .ways
            .iter()
            .position(|entry| entry.valid && entry.key == key);
        let Some(way) = way else {
            self.stats.misses = self.stats.misses.saturating_add(1);
            return None;
        };
        self.sets[set].mru_way = way as u8;
        let entry = &self.sets[set].ways[way];
        let offset = entry.offset;
        let instruction_count = entry.instruction_count;
        let entry = self.mapping.entry_address(offset)?;
        self.stats.hits = self.stats.hits.saturating_add(1);
        Some(DbtCacheHit {
            entry,
            instruction_count,
        })
    }

    #[cfg(not(feature = "dbt-execution-profile"))]
    pub(crate) fn publish(
        &mut self,
        key: DbtCacheKey,
        block: &TranslatedBlock<'_>,
    ) -> Result<DbtCacheHit, DbtFault> {
        self.publish_inner(key, block)
    }

    #[cfg(feature = "dbt-execution-profile")]
    pub(crate) fn publish(
        &mut self,
        key: DbtCacheKey,
        block: &TranslatedBlock<'_>,
    ) -> Result<DbtCacheHit, DbtFault> {
        self.publish_inner(key, block, None)
    }

    #[cfg(feature = "dbt-execution-profile")]
    pub(crate) fn publish_profiled(
        &mut self,
        key: DbtCacheKey,
        block: &TranslatedBlock<'_>,
        profile: &mut ExactDbtProfile,
    ) -> Result<DbtCacheHit, DbtFault> {
        self.publish_inner(key, block, Some(profile))
    }

    fn publish_inner(
        &mut self,
        key: DbtCacheKey,
        block: &TranslatedBlock<'_>,
        #[cfg(feature = "dbt-execution-profile")] mut profile: Option<&mut ExactDbtProfile>,
    ) -> Result<DbtCacheHit, DbtFault> {
        if block.mode() != DbtBlockMode::ChainableThroughput {
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
        let block_capacity = self.mapping.capacity() - self.block_region_start;
        if block.code().len() > block_capacity {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                format!(
                    "native block requires {} bytes but code cache block region is {} bytes",
                    block.code().len(),
                    block_capacity
                ),
            ));
        }

        #[cfg(feature = "dbt-execution-profile")]
        let resolved_profile_addresses = {
            let relocations = block.profile_relocations();
            let mut resolved = [None; MAX_PROFILE_RELOCATIONS];
            if !relocations.is_empty() {
                let profile = profile.as_deref_mut().ok_or_else(|| {
                    Self::fault(
                        DbtFaultKind::AbiInvariant,
                        "profiled DBT block publication has no execution-profile storage",
                    )
                })?;
                for (slot, relocation) in resolved.iter_mut().zip(relocations) {
                    *slot = Some(profile.counter_for(relocation.key)?);
                }
            }
            resolved
        };

        let chain_entry_offset = block.chain_entry_offset();
        let (placement_cursor, offset) = publication_offset(
            self.write_cursor,
            chain_entry_offset,
            self.code_alignment,
        )
        .filter(|offset| {
            offset
                .checked_add(block.code().len())
                .is_some_and(|end| end <= self.mapping.capacity())
        })
        .map(|offset| (self.write_cursor, offset))
        .or_else(|| {
            publication_offset(
                self.block_region_start,
                chain_entry_offset,
                self.code_alignment,
            )
            .filter(|offset| {
                offset
                    .checked_add(block.code().len())
                    .is_some_and(|end| end <= self.mapping.capacity())
            })
            .map(|offset| (self.block_region_start, offset))
        })
        .ok_or_else(|| {
            Self::fault(
                DbtFaultKind::Capacity,
                format!(
                    "native block requires {} bytes plus alignment padding but code cache block region is {} bytes",
                    block.code().len(),
                    block_capacity
                ),
            )
        })?;
        let alignment_padding = offset - placement_cursor;
        let end = offset + block.code().len();
        self.invalidate_overlapping(offset, end)?;

        let set = self.set_index(key);
        let way = self.select_way(set);
        if self.sets[set].ways[way].valid {
            self.invalidate_entry(set, way)?;
            self.stats.metadata_evictions = self.stats.metadata_evictions.saturating_add(1);
        }
        self.mapping.publish_at(offset, block.code())?;
        #[cfg(feature = "dbt-execution-profile")]
        for (relocation, addresses) in block
            .profile_relocations()
            .iter()
            .zip(resolved_profile_addresses.iter().flatten())
        {
            self.mapping.patch_u64(
                offset,
                block.code().len(),
                offset + relocation.count_address_offset as usize,
                addresses.count as usize as u64,
            )?;
            self.mapping.patch_u64(
                offset,
                block.code().len(),
                offset + relocation.overflow_address_offset as usize,
                addresses.overflows as usize as u64,
            )?;
        }
        for relocation in block.cold_exit_relocations() {
            self.mapping.patch_rel32(
                offset,
                block.code().len(),
                offset + relocation.displacement_offset as usize,
                self.completed_exit_stub_offset,
            )?;
        }
        let mut links = [CacheLink::default(); MAX_STATIC_LINKS];
        for (stored, descriptor) in links.iter_mut().zip(block.static_links()) {
            stored.target_pc = descriptor.target_pc;
            stored.displacement_offset = descriptor.displacement_offset;
            stored.reset_target_offset = descriptor.reset_target_offset;
        }
        #[cfg(feature = "dbt-code-audit")]
        let mut cold_exit_displacements = [0; MAX_COLD_EXIT_RELOCATIONS];
        #[cfg(feature = "dbt-code-audit")]
        for (stored, relocation) in cold_exit_displacements
            .iter_mut()
            .zip(block.cold_exit_relocations())
        {
            *stored = relocation.displacement_offset;
        }
        self.sets[set].ways[way] = CacheEntry {
            valid: true,
            key,
            offset,
            length: block.code().len(),
            instruction_count: block.instruction_count(),
            chain_entry_offset: block.chain_entry_offset(),
            links,
            link_count: block.static_links().len() as u8,
            #[cfg(feature = "dbt-code-audit")]
            cold_exit_displacements,
            #[cfg(feature = "dbt-code-audit")]
            cold_exit_count: block.cold_exit_relocations().len() as u8,
        };
        #[cfg(feature = "dbt-code-audit")]
        {
            self.register_pressure
                .retain(|(stored_key, _)| *stored_key != key);
            self.register_pressure
                .push((key, block.register_pressure()));
        }
        self.sets[set].mru_way = way as u8;
        self.write_cursor = end;
        self.link_outgoing(set, way)?;
        self.link_incoming(set, way)?;
        self.stats.publications = self.stats.publications.saturating_add(1);
        self.stats.alignment_padding_bytes = self
            .stats
            .alignment_padding_bytes
            .saturating_add(alignment_padding as u64);
        let entry = self.mapping.entry_address(offset).ok_or_else(|| {
            Self::fault(
                DbtFaultKind::AbiInvariant,
                "published DBT block has no executable entry address",
            )
        })?;
        Ok(DbtCacheHit {
            entry,
            instruction_count: block.instruction_count(),
        })
    }

    pub(crate) fn invalidate_all(&mut self) {
        for set in 0..self.sets.len() {
            for way in 0..WAYS {
                let link_count = self.sets[set].ways[way].link_count as usize;
                for link in 0..link_count {
                    if self.sets[set].ways[way].links[link].linked {
                        self.reset_link(set, way, link)
                            .expect("validated DBT link metadata must remain patchable");
                    }
                }
            }
        }
        for set in &mut self.sets {
            for entry in &mut set.ways {
                *entry = CacheEntry::default();
            }
        }
        #[cfg(feature = "dbt-code-audit")]
        self.register_pressure.clear();
    }

    pub(crate) const fn stats(&self) -> DbtCodeCacheStats {
        self.stats
    }

    pub(crate) const fn reserved_bytes(&self) -> usize {
        self.mapping.reserved_bytes()
    }

    pub(crate) fn metadata_bytes(&self) -> usize {
        self.sets.len() * std::mem::size_of::<CacheSet>()
    }

    pub(crate) fn live_entry_bytes(&self) -> usize {
        self.sets
            .iter()
            .flat_map(|set| set.ways.iter())
            .filter(|entry| entry.valid)
            .map(|entry| entry.length)
            .sum()
    }

    pub(crate) fn code_prefix_bytes(&self) -> usize {
        self.sets
            .iter()
            .flat_map(|set| set.ways.iter())
            .filter(|entry| entry.valid)
            .filter_map(|entry| entry.offset.checked_add(entry.length))
            .max()
            .unwrap_or(self.completed_exit_stub_offset + self.completed_exit_stub_len)
    }

    #[cfg(feature = "dbt-code-audit")]
    pub(crate) fn snapshot(
        &self,
        generation: u64,
    ) -> Result<Rv32DbtCodeSnapshot, Rv32DbtCodeSnapshotError> {
        let mut entries = self
            .sets
            .iter()
            .flat_map(|set| set.ways.iter())
            .filter(|entry| entry.valid)
            .copied()
            .collect::<Vec<_>>();
        entries.sort_unstable_by_key(|entry| (entry.offset, entry.key.pc));

        let support_end = self
            .completed_exit_stub_offset
            .checked_add(self.completed_exit_stub_len)
            .ok_or_else(|| Rv32DbtCodeSnapshotError::new("DBT support-code range overflows"))?;
        if self.completed_exit_stub_len == 0 || support_end > self.mapping.capacity() {
            return Err(Rv32DbtCodeSnapshotError::new(
                "invalid completed-exit support-code range",
            ));
        }
        let support_code = vec![Rv32DbtSupportCodeRange {
            kind: Rv32DbtSupportCodeKind::CompletedExitStub,
            offset: u32::try_from(self.completed_exit_stub_offset)
                .map_err(|_| Rv32DbtCodeSnapshotError::new("DBT support offset exceeds u32"))?,
            length: u32::try_from(self.completed_exit_stub_len)
                .map_err(|_| Rv32DbtCodeSnapshotError::new("DBT support length exceeds u32"))?,
        }];
        let mut previous_end = support_end;
        let mut cold_exit_displacements = Vec::new();
        let mut blocks = Vec::with_capacity(entries.len());
        for entry in entries {
            if entry.key.generation != generation {
                return Err(Rv32DbtCodeSnapshotError::new(format!(
                    "live DBT block generation {} differs from current {generation}",
                    entry.key.generation
                )));
            }
            let end = entry.offset.checked_add(entry.length).ok_or_else(|| {
                Rv32DbtCodeSnapshotError::new("live DBT block range overflows host address space")
            })?;
            if entry.length == 0 || end > self.mapping.capacity() || entry.offset < previous_end {
                return Err(Rv32DbtCodeSnapshotError::new(format!(
                    "invalid live DBT block range {}..{end}",
                    entry.offset
                )));
            }
            let chain_entry = entry
                .offset
                .checked_add(entry.chain_entry_offset as usize)
                .filter(|offset| *offset < end)
                .ok_or_else(|| Rv32DbtCodeSnapshotError::new("invalid DBT chain entry offset"))?;
            let mut edges = Vec::with_capacity(entry.link_count as usize);
            for link in &entry.links[..entry.link_count as usize] {
                let displacement = entry
                    .offset
                    .checked_add(link.displacement_offset as usize)
                    .filter(|offset| {
                        offset
                            .checked_add(4)
                            .is_some_and(|end| end <= entry.offset + entry.length)
                    })
                    .ok_or_else(|| {
                        Rv32DbtCodeSnapshotError::new("invalid DBT link displacement")
                    })?;
                let reset = entry
                    .offset
                    .checked_add(link.reset_target_offset as usize)
                    .filter(|offset| *offset < end)
                    .ok_or_else(|| Rv32DbtCodeSnapshotError::new("invalid DBT reset target"))?;
                edges.push(Rv32DbtCodeEdge {
                    target_pc: link.target_pc,
                    displacement_offset: u32::try_from(displacement).map_err(|_| {
                        Rv32DbtCodeSnapshotError::new("DBT displacement exceeds u32")
                    })?,
                    reset_target_offset: u32::try_from(reset).map_err(|_| {
                        Rv32DbtCodeSnapshotError::new("DBT reset target exceeds u32")
                    })?,
                    linked: link.linked,
                });
            }
            for displacement in &entry.cold_exit_displacements[..entry.cold_exit_count as usize] {
                let displacement = entry
                    .offset
                    .checked_add(*displacement as usize)
                    .filter(|offset| {
                        offset
                            .checked_add(4)
                            .is_some_and(|end| end <= entry.offset + entry.length)
                    })
                    .ok_or_else(|| {
                        Rv32DbtCodeSnapshotError::new("invalid DBT cold-exit displacement")
                    })?;
                cold_exit_displacements.push(displacement);
            }
            blocks.push(Rv32DbtCodeBlock {
                guest_pc: entry.key.pc,
                generation: entry.key.generation,
                offset: u32::try_from(entry.offset)
                    .map_err(|_| Rv32DbtCodeSnapshotError::new("DBT offset exceeds u32"))?,
                length: u32::try_from(entry.length)
                    .map_err(|_| Rv32DbtCodeSnapshotError::new("DBT length exceeds u32"))?,
                chain_entry_offset: u32::try_from(chain_entry)
                    .map_err(|_| Rv32DbtCodeSnapshotError::new("DBT chain entry exceeds u32"))?,
                guest_instruction_count: entry.instruction_count,
                register_pressure: self
                    .register_pressure
                    .iter()
                    .find_map(|(key, pressure)| (*key == entry.key).then_some((*pressure).into()))
                    .ok_or_else(|| {
                        Rv32DbtCodeSnapshotError::new(
                            "live DBT block is missing register-pressure metadata",
                        )
                    })?,
                edges,
            });
            previous_end = end;
        }
        let used_bytes = self
            .mapping
            .snapshot_prefix(previous_end)
            .map_err(|error| Rv32DbtCodeSnapshotError::new(error.to_string()))?;
        for block in &blocks {
            for edge in block.edges.iter().filter(|edge| edge.linked) {
                let target = blocks
                    .iter()
                    .find(|target| {
                        target.guest_pc == edge.target_pc && target.generation == block.generation
                    })
                    .ok_or_else(|| {
                        Rv32DbtCodeSnapshotError::new(format!(
                            "linked DBT edge from {:#010x} has no live target {:#010x}",
                            block.guest_pc, edge.target_pc
                        ))
                    })?;
                let displacement_offset = edge.displacement_offset as usize;
                let displacement_end = displacement_offset + 4;
                let displacement = i32::from_le_bytes(
                    used_bytes[displacement_offset..displacement_end]
                        .try_into()
                        .expect("validated DBT displacement range"),
                );
                let resolved = i64::try_from(displacement_end)
                    .ok()
                    .and_then(|origin| origin.checked_add(i64::from(displacement)));
                if resolved != Some(i64::from(target.chain_entry_offset)) {
                    return Err(Rv32DbtCodeSnapshotError::new(format!(
                        "linked DBT edge from {:#010x} does not resolve to live target {:#010x}",
                        block.guest_pc, edge.target_pc
                    )));
                }
            }
        }
        for displacement_offset in cold_exit_displacements {
            let displacement_end = displacement_offset + 4;
            let displacement = i32::from_le_bytes(
                used_bytes[displacement_offset..displacement_end]
                    .try_into()
                    .expect("validated DBT cold-exit displacement range"),
            );
            let resolved = i64::try_from(displacement_end)
                .ok()
                .and_then(|origin| origin.checked_add(i64::from(displacement)));
            if resolved != Some(self.completed_exit_stub_offset as i64) {
                return Err(Rv32DbtCodeSnapshotError::new(
                    "DBT cold-exit relocation does not resolve to the completed-exit stub",
                ));
            }
        }
        Ok(Rv32DbtCodeSnapshot {
            generation,
            used_bytes,
            support_code,
            blocks,
        })
    }

    fn set_index(&self, key: DbtCacheKey) -> usize {
        let pc = u64::from(key.pc >> 2);
        let mixed = pc ^ key.generation ^ key.generation.rotate_left(23);
        (mixed as usize) & self.set_mask
    }

    fn select_way(&self, set: usize) -> usize {
        self.sets[set]
            .ways
            .iter()
            .position(|entry| !entry.valid)
            .unwrap_or_else(|| usize::from(self.sets[set].mru_way ^ 1))
    }

    fn invalidate_overlapping(&mut self, start: usize, end: usize) -> Result<(), DbtFault> {
        loop {
            let victim = (0..self.sets.len()).find_map(|set| {
                (0..WAYS).find_map(|way| {
                    let entry = self.sets[set].ways[way];
                    (entry.valid
                        && ranges_overlap(start, end, entry.offset, entry.offset + entry.length))
                    .then_some((set, way))
                })
            });
            let Some((set, way)) = victim else {
                return Ok(());
            };
            self.invalidate_entry(set, way)?;
            self.stats.overlap_invalidations = self.stats.overlap_invalidations.saturating_add(1);
        }
    }

    fn invalidate_entry(&mut self, target_set: usize, target_way: usize) -> Result<(), DbtFault> {
        let target = self.sets[target_set].ways[target_way];
        if !target.valid {
            return Ok(());
        }
        let target_key = target.key;
        #[cfg(feature = "dbt-code-audit")]
        self.register_pressure
            .retain(|(stored_key, _)| *stored_key != target_key);
        for set in 0..self.sets.len() {
            for way in 0..WAYS {
                let link_count = self.sets[set].ways[way].link_count as usize;
                for link in 0..link_count {
                    let source = self.sets[set].ways[way];
                    let record = source.links[link];
                    if record.linked
                        && record.target_pc == target_key.pc
                        && source.valid
                        && source.key.generation == target_key.generation
                    {
                        self.reset_link(set, way, link)?;
                    }
                }
            }
        }
        self.sets[target_set].ways[target_way] = CacheEntry::default();
        Ok(())
    }

    fn link_outgoing(&mut self, source_set: usize, source_way: usize) -> Result<(), DbtFault> {
        let source = self.sets[source_set].ways[source_way];
        let source_key = source.key;
        for link in 0..source.link_count as usize {
            let record = source.links[link];
            if !same_guest_page(source_key.pc, record.target_pc) {
                continue;
            }
            let target_key = DbtCacheKey::new(record.target_pc, source_key.generation);
            if let Some((target_set, target_way)) = self.find_entry(target_key) {
                self.establish_link(source_set, source_way, link, target_set, target_way)?;
            }
        }
        Ok(())
    }

    fn link_incoming(&mut self, target_set: usize, target_way: usize) -> Result<(), DbtFault> {
        let target_key = self.sets[target_set].ways[target_way].key;
        for source_set in 0..self.sets.len() {
            for source_way in 0..WAYS {
                let source = self.sets[source_set].ways[source_way];
                if !source.valid {
                    continue;
                }
                let source_key = source.key;
                for link in 0..source.link_count as usize {
                    let record = source.links[link];
                    if !record.linked
                        && record.target_pc == target_key.pc
                        && source_key.generation == target_key.generation
                        && same_guest_page(source_key.pc, target_key.pc)
                    {
                        self.establish_link(source_set, source_way, link, target_set, target_way)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn establish_link(
        &mut self,
        source_set: usize,
        source_way: usize,
        link: usize,
        target_set: usize,
        target_way: usize,
    ) -> Result<(), DbtFault> {
        let source = self.sets[source_set].ways[source_way];
        if source.links[link].linked {
            return Ok(());
        }
        let target = self.sets[target_set].ways[target_way];
        let record = source.links[link];
        self.mapping.patch_rel32(
            source.offset,
            source.length,
            source.offset + record.displacement_offset as usize,
            target.offset + target.chain_entry_offset as usize,
        )?;
        self.sets[source_set].ways[source_way].links[link].linked = true;
        self.stats.links_established = self.stats.links_established.saturating_add(1);
        Ok(())
    }

    fn reset_link(
        &mut self,
        source_set: usize,
        source_way: usize,
        link: usize,
    ) -> Result<(), DbtFault> {
        let source = self.sets[source_set].ways[source_way];
        if !source.links[link].linked {
            return Ok(());
        }
        let record = source.links[link];
        self.mapping.patch_rel32(
            source.offset,
            source.length,
            source.offset + record.displacement_offset as usize,
            source.offset + record.reset_target_offset as usize,
        )?;
        self.sets[source_set].ways[source_way].links[link].linked = false;
        self.stats.links_reset = self.stats.links_reset.saturating_add(1);
        Ok(())
    }

    fn find_entry(&self, key: DbtCacheKey) -> Option<(usize, usize)> {
        let set = self.set_index(key);
        self.sets[set]
            .ways
            .iter()
            .position(|entry| entry.valid && entry.key == key)
            .map(|way| (set, way))
    }

    fn fault(kind: DbtFaultKind, message: impl Into<String>) -> DbtFault {
        DbtFault::new(kind, 0, None, message)
    }
}

fn align_up(value: usize, alignment: usize) -> usize {
    align_up_checked(value, alignment).unwrap_or(usize::MAX)
}

fn align_up_checked(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
}

fn publication_offset(
    cursor: usize,
    chain_entry_offset: u32,
    alignment: Rv32DbtCodeAlignment,
) -> Option<usize> {
    let anchor_offset = match alignment {
        Rv32DbtCodeAlignment::BlockBase(_) => 0,
        Rv32DbtCodeAlignment::ChainEntry(_) => chain_entry_offset as usize,
    };
    cursor
        .checked_add(anchor_offset)
        .and_then(|anchor| align_up_checked(anchor, alignment.bytes()))
        .and_then(|aligned| aligned.checked_sub(anchor_offset))
}

fn ranges_overlap(lhs_start: usize, lhs_end: usize, rhs_start: usize, rhs_end: usize) -> bool {
    lhs_start < rhs_end && rhs_start < lhs_end
}

fn same_guest_page(lhs: u32, rhs: u32) -> bool {
    lhs / GUEST_PAGE_BYTES == rhs / GUEST_PAGE_BYTES
}

#[cfg(test)]
mod tests {
    use super::{DbtCacheKey, DirectDbtCodeCache};
    use crate::rv32_dbt::block::{
        DbtBlockInput, DbtBlockMode, DbtLinkKind, DbtStaticLink, TranslatedBlock,
    };
    #[cfg(all(feature = "dbt-execution-profile", feature = "dbt-code-audit"))]
    use crate::rv32_dbt::profile::{DbtProfileKey, ExactDbtProfile};
    #[cfg(feature = "dbt-code-audit")]
    use crate::rv32_dbt::x86_64::lower::DbtTranslationWorkspace;
    #[cfg(feature = "dbt-code-audit")]
    use crate::rv32_dbt::x86_64::register_cache::RegisterPressureAudit;
    use crate::rv32_dbt::DbtFaultKind;
    use crate::rv32_machine::{Rv32DbtCodeAlignment, DEFAULT_DBT_CODE_ALIGNMENT};
    use crate::rv32im::{decode_product_word, encoding::addi, Rv32ResolvedInstruction};
    #[cfg(feature = "dbt-code-audit")]
    use std::collections::BTreeSet;

    const PAGE_BYTES: usize = 4096;

    #[cfg(all(feature = "dbt-execution-profile", feature = "dbt-code-audit"))]
    #[test]
    fn profiled_publication_patches_stable_counter_addresses() {
        use crate::rv32_dbt::block::DbtProfileRelocation;

        let key = DbtCacheKey::new(0x1000, 0);
        let profile_key = DbtProfileKey::Block { pc: 0x1000 };
        let word = addi(1, 0, 1);
        let slots = [Rv32ResolvedInstruction::Valid {
            word,
            instruction: decode_product_word(word).unwrap(),
        }];
        let input = DbtBlockInput::new(key.pc, &slots, DbtBlockMode::ChainableThroughput).unwrap();
        let code = [0x90; 24];
        let block = TranslatedBlock::new_profiled(
            &input,
            &code,
            0,
            0,
            0,
            &[],
            &[],
            &[DbtProfileRelocation {
                key: profile_key,
                count_address_offset: 2,
                overflow_address_offset: 12,
            }],
        )
        .unwrap();
        let mut profile = ExactDbtProfile::new(4).unwrap();
        let mut cache = DirectDbtCodeCache::new(4, PAGE_BYTES).unwrap();

        cache.publish_profiled(key, &block, &mut profile).unwrap();

        let (set, way) = cache.find_entry(key).unwrap();
        let offset = cache.sets[set].ways[way].offset;
        let bytes = cache.mapping.snapshot_prefix(offset + code.len()).unwrap();
        let addresses = profile.counter_for(profile_key).unwrap();
        assert_eq!(
            &bytes[offset + 2..offset + 10],
            &(addresses.count as usize as u64).to_le_bytes()
        );
        assert_eq!(
            &bytes[offset + 12..offset + 20],
            &(addresses.overflows as usize as u64).to_le_bytes()
        );
    }

    #[cfg(feature = "dbt-execution-profile")]
    #[test]
    fn profile_capacity_failure_never_publishes_the_block() {
        use crate::rv32_dbt::block::{DbtProfileRelocation, DbtStaticLink};
        use crate::rv32_dbt::profile::{DbtProfileKey, ExactDbtProfile};

        let key = DbtCacheKey::new(0x2000, 0);
        let word = addi(1, 0, 1);
        let slots = [Rv32ResolvedInstruction::Valid {
            word,
            instruction: decode_product_word(word).unwrap(),
        }];
        let input = DbtBlockInput::new(key.pc, &slots, DbtBlockMode::ChainableThroughput).unwrap();
        let block = TranslatedBlock::new_profiled(
            &input,
            &[0x90; 48],
            0,
            0,
            0,
            &[] as &[DbtStaticLink],
            &[],
            &[
                DbtProfileRelocation {
                    key: DbtProfileKey::Block { pc: key.pc },
                    count_address_offset: 2,
                    overflow_address_offset: 12,
                },
                DbtProfileRelocation {
                    key: DbtProfileKey::Block { pc: key.pc + 4 },
                    count_address_offset: 22,
                    overflow_address_offset: 32,
                },
            ],
        )
        .unwrap();
        let mut profile = ExactDbtProfile::new(1).unwrap();
        let mut cache = DirectDbtCodeCache::new(4, PAGE_BYTES).unwrap();

        assert!(cache.publish_profiled(key, &block, &mut profile).is_err());
        assert!(cache.lookup(key).is_none());
        assert_eq!(cache.stats().publications, 0);
    }

    fn block(pc: u32, code: &[u8]) -> TranslatedBlock<'_> {
        block_with_links(pc, code, 0, &[])
    }

    fn block_with_links<'a>(
        pc: u32,
        code: &'a [u8],
        chain_entry_offset: u32,
        links: &[DbtStaticLink],
    ) -> TranslatedBlock<'a> {
        let word = addi(1, 1, 1);
        let slots = [Rv32ResolvedInstruction::Valid {
            word,
            instruction: decode_product_word(word).unwrap(),
        }];
        let input = DbtBlockInput::new(pc, &slots, DbtBlockMode::ChainableThroughput).unwrap();
        TranslatedBlock::new(&input, code, 0, 0, chain_entry_offset, links, &[]).unwrap()
    }

    fn returning(value: u32) -> [u8; 6] {
        let mut code = [0xb8, 0, 0, 0, 0, 0xc3];
        code[1..5].copy_from_slice(&value.to_le_bytes());
        code
    }

    fn source_link(target_pc: u32) -> ([u8; 11], DbtStaticLink) {
        let mut code = [0xe9, 0, 0, 0, 0, 0xb8, 0, 0, 0, 0, 0xc3];
        code[6..10].copy_from_slice(&7_u32.to_le_bytes());
        let link = DbtStaticLink {
            target_pc,
            displacement_offset: 1,
            reset_target_offset: 5,
            kind: DbtLinkKind::Fallthrough,
        };
        (code, link)
    }

    unsafe fn execute(entry: *const u8) -> u32 {
        let entry: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(entry) };
        unsafe { entry() }
    }

    #[cfg(feature = "dbt-code-audit")]
    #[test]
    fn snapshot_contains_only_live_entries_and_resolved_link_bytes() {
        let mut cache = DirectDbtCodeCache::new(4, PAGE_BYTES).unwrap();
        let source_key = DbtCacheKey::new(0x1000, 0);
        let target_key = DbtCacheKey::new(0x1004, 0);
        let eviction_keys = [0x1008, 0x1018, 0x1028].map(|pc| DbtCacheKey::new(pc, 0));
        let (source_code, link) = source_link(target_key.pc);

        cache
            .publish(
                source_key,
                &block_with_links(0x1000, &source_code, 5, &[link]),
            )
            .unwrap();
        cache
            .publish(target_key, &block(0x1004, &returning(9)))
            .unwrap();
        for (index, key) in eviction_keys.into_iter().enumerate() {
            cache
                .publish(key, &block(key.pc, &returning(11 + index as u32)))
                .unwrap();
        }

        let snapshot = cache.snapshot(0).unwrap();
        let pcs = snapshot
            .blocks
            .iter()
            .map(|block| block.guest_pc)
            .collect::<BTreeSet<_>>();
        assert!(pcs.contains(&source_key.pc));
        assert!(pcs.contains(&target_key.pc));
        assert_eq!(
            pcs.intersection(&BTreeSet::from(eviction_keys.map(|key| key.pc)))
                .count(),
            2
        );

        if let (Some(source), Some(target)) = (
            snapshot
                .blocks
                .iter()
                .find(|block| block.guest_pc == source_key.pc),
            snapshot
                .blocks
                .iter()
                .find(|block| block.guest_pc == target_key.pc),
        ) {
            let edge = source.edges.iter().find(|edge| edge.linked).unwrap();
            let displacement_offset = edge.displacement_offset as usize;
            let displacement = i32::from_le_bytes(
                snapshot.used_bytes[displacement_offset..displacement_offset + 4]
                    .try_into()
                    .unwrap(),
            );
            let resolved = (displacement_offset + 4) as i64 + i64::from(displacement);
            assert_eq!(resolved, i64::from(target.chain_entry_offset));
        }
    }

    #[cfg(feature = "dbt-code-audit")]
    #[test]
    fn snapshot_keeps_register_pressure() {
        let mut cache = DirectDbtCodeCache::new(4, PAGE_BYTES).unwrap();
        let key = DbtCacheKey::new(0x1000, 0);
        let pressure = RegisterPressureAudit {
            body_arch_loads: 3,
            dirty_live_eviction_stores: 2,
            allocation_pressure: 4,
            scratch_clobber_sites: [5, 6, 7],
            ..RegisterPressureAudit::default()
        };

        cache
            .publish(
                key,
                &block(key.pc, &[0xc3]).with_register_pressure(pressure),
            )
            .unwrap();

        let snapshot = cache.snapshot(0).unwrap();
        let actual = snapshot.blocks[0].register_pressure;
        assert_eq!(actual.body_arch_loads, 3);
        assert_eq!(actual.dirty_live_eviction_stores, 2);
        assert_eq!(actual.allocation_pressure, 4);
        assert_eq!(actual.scratch_clobber_sites, [5, 6, 7]);
    }

    #[cfg(feature = "dbt-code-audit")]
    #[test]
    fn snapshot_rejects_link_metadata_that_disagrees_with_executable_bytes() {
        let mut cache = DirectDbtCodeCache::new(4, PAGE_BYTES).unwrap();
        let source_key = DbtCacheKey::new(0x1000, 0);
        let target_key = DbtCacheKey::new(0x1004, 0);
        let (source_code, link) = source_link(target_key.pc);
        cache
            .publish(
                source_key,
                &block_with_links(source_key.pc, &source_code, 5, &[link]),
            )
            .unwrap();
        cache
            .publish(target_key, &block(target_key.pc, &returning(9)))
            .unwrap();

        let (source_set, source_way) = cache.find_entry(source_key).unwrap();
        let source = cache.sets[source_set].ways[source_way];
        let record = source.links[0];
        assert!(record.linked);
        cache
            .mapping
            .patch_rel32(
                source.offset,
                source.length,
                source.offset + record.displacement_offset as usize,
                source.offset + record.reset_target_offset as usize,
            )
            .unwrap();

        assert!(cache.snapshot(0).is_err());
    }

    #[cfg(feature = "dbt-code-audit")]
    #[test]
    fn snapshot_rejects_cold_exit_that_no_longer_targets_shared_stub() {
        let mut cache = DirectDbtCodeCache::new(4, PAGE_BYTES).unwrap();
        let key = DbtCacheKey::new(0x1000, 0);
        let word = addi(1, 1, 1);
        let slots = [Rv32ResolvedInstruction::Valid {
            word,
            instruction: decode_product_word(word).unwrap(),
        }];
        let input = DbtBlockInput::new(key.pc, &slots, DbtBlockMode::ChainableThroughput).unwrap();
        let mut workspace = DbtTranslationWorkspace::new(PAGE_BYTES, slots.len()).unwrap();
        let translated = workspace.lower(&input, PAGE_BYTES as u32).unwrap();
        assert!(!translated.cold_exit_relocations().is_empty());
        cache.publish(key, &translated).unwrap();

        let (set, way) = cache.find_entry(key).unwrap();
        let entry = cache.sets[set].ways[way];
        let displacement = entry.offset + entry.cold_exit_displacements[0] as usize;
        cache
            .mapping
            .patch_rel32(entry.offset, entry.length, displacement, entry.offset)
            .unwrap();

        assert!(cache.snapshot(0).is_err());
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
        TranslatedBlock::new(&input, &[0xc3], 0, 0, 0, &[], &[]).unwrap()
    }

    #[test]
    fn resident_key_hits_but_another_generation_misses() {
        let mut cache = DirectDbtCodeCache::new(2, PAGE_BYTES).unwrap();
        let key = DbtCacheKey::new(0x1000, 7);

        assert!(cache.lookup(key).is_none());
        let published = cache.publish(key, &block(0x1000, &[0xc3])).unwrap();
        let hit = cache.lookup(key).unwrap();

        assert!(!published.entry().is_null());
        assert_eq!(published.instruction_count(), 1);
        assert_eq!(hit.entry(), published.entry());
        assert_eq!(hit.instruction_count(), published.instruction_count());
        assert!(cache.lookup(DbtCacheKey::new(0x1000, 8)).is_none());
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 2);
        assert_eq!(cache.stats().publications, 1);
        assert_eq!(cache.reserved_bytes(), PAGE_BYTES * 2);
    }

    #[test]
    fn code_cache_wrap_preserves_completed_exit_support_region() {
        let mut cache = DirectDbtCodeCache::new(8, PAGE_BYTES).unwrap();
        let support = cache.completed_exit_stub_range();
        assert_eq!(support.start, 0);
        assert!(support.end <= cache.block_region_start());

        let code = vec![0x90; 1_500];
        for index in 0..6_u32 {
            cache
                .publish(
                    DbtCacheKey::new(0x1000 + index * 4, 0),
                    &block(0x1000 + index * 4, &code),
                )
                .unwrap();
        }

        assert!(cache
            .sets
            .iter()
            .flat_map(|set| set.ways.iter())
            .filter(|entry| entry.valid)
            .all(|entry| entry.offset >= cache.block_region_start()));
    }

    #[test]
    fn published_guest_blocks_honor_the_default_alignment() {
        let mut cache = DirectDbtCodeCache::new(4, PAGE_BYTES).unwrap();
        let key = DbtCacheKey::new(0x1000, 0);

        cache.publish(key, &block(key.pc, &returning(7))).unwrap();
        let (set, way) = cache.find_entry(key).unwrap();

        assert_eq!(
            cache.sets[set].ways[way].offset % DEFAULT_DBT_CODE_ALIGNMENT.bytes(),
            0
        );
    }

    #[test]
    fn block_base_policy_aligns_every_callable_entry() {
        let mut cache = DirectDbtCodeCache::new_with_alignment(
            4,
            PAGE_BYTES,
            Rv32DbtCodeAlignment::BlockBase(128),
        )
        .unwrap();
        let keys = [DbtCacheKey::new(0x1000, 0), DbtCacheKey::new(0x1004, 0)];

        for key in keys {
            cache.publish(key, &block(key.pc, &returning(7))).unwrap();
            let (set, way) = cache.find_entry(key).unwrap();
            assert_eq!(cache.sets[set].ways[way].offset % 128, 0);
        }
        assert!(cache.stats().alignment_padding_bytes > 0);
    }

    #[test]
    fn chain_entry_policy_aligns_direct_link_targets() {
        let mut cache = DirectDbtCodeCache::new_with_alignment(
            4,
            PAGE_BYTES,
            Rv32DbtCodeAlignment::ChainEntry(32),
        )
        .unwrap();
        let key = DbtCacheKey::new(0x1000, 0);

        cache
            .publish(key, &block_with_links(key.pc, &returning(7), 5, &[]))
            .unwrap();
        let (set, way) = cache.find_entry(key).unwrap();
        let entry = cache.sets[set].ways[way];

        assert_eq!((entry.offset + entry.chain_entry_offset as usize) % 32, 0);
        assert!(cache.stats().alignment_padding_bytes > 0);
    }

    #[test]
    fn wraparound_preserves_the_configured_alignment_anchor() {
        for alignment in [
            Rv32DbtCodeAlignment::BlockBase(128),
            Rv32DbtCodeAlignment::ChainEntry(32),
        ] {
            let mut cache =
                DirectDbtCodeCache::new_with_alignment(8, PAGE_BYTES, alignment).unwrap();
            let code = vec![0x90; 1_500];

            for index in 0..6_u32 {
                let pc = 0x2000 + index * 4;
                cache
                    .publish(
                        DbtCacheKey::new(pc, 0),
                        &block_with_links(pc, &code, 5, &[]),
                    )
                    .unwrap();
            }

            for entry in cache
                .sets
                .iter()
                .flat_map(|set| set.ways.iter())
                .filter(|entry| entry.valid)
            {
                assert!(entry.offset >= cache.block_region_start());
                let anchor = match alignment {
                    Rv32DbtCodeAlignment::BlockBase(_) => entry.offset,
                    Rv32DbtCodeAlignment::ChainEntry(_) => {
                        entry.offset + entry.chain_entry_offset as usize
                    }
                };
                assert_eq!(anchor % alignment.bytes(), 0);
            }
        }
    }

    #[cfg(feature = "dbt-code-audit")]
    #[test]
    fn snapshot_validates_links_to_an_aligned_chain_entry() {
        let mut cache = DirectDbtCodeCache::new_with_alignment(
            8,
            PAGE_BYTES,
            Rv32DbtCodeAlignment::ChainEntry(32),
        )
        .unwrap();
        let source_key = DbtCacheKey::new(0x3000, 0);
        let target_key = DbtCacheKey::new(0x3004, 0);
        let (source_code, link) = source_link(target_key.pc);

        cache
            .publish(
                source_key,
                &block_with_links(source_key.pc, &source_code, 5, &[link]),
            )
            .unwrap();
        cache
            .publish(
                target_key,
                &block_with_links(target_key.pc, &returning(9), 5, &[]),
            )
            .unwrap();

        let snapshot = cache.snapshot(0).unwrap();
        let target = snapshot
            .blocks
            .iter()
            .find(|block| block.guest_pc == target_key.pc)
            .unwrap();
        assert_eq!(target.chain_entry_offset as usize % 32, 0);
        assert!(snapshot
            .blocks
            .iter()
            .find(|block| block.guest_pc == source_key.pc)
            .unwrap()
            .edges
            .iter()
            .any(|edge| edge.linked));
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
        let fourth = DbtCacheKey::new(24, 0);

        let first_hit = cache.publish(first, &block(0, &[0xc3])).unwrap();
        cache.publish(second, &block(8, &[0xc3])).unwrap();
        assert_eq!(cache.lookup(first), Some(first_hit));
        cache.publish(third, &block(16, &[0xc3])).unwrap();

        assert!(cache.lookup(first).is_some());
        assert!(cache.lookup(second).is_none());
        assert!(cache.lookup(third).is_some());
        cache.publish(fourth, &block(24, &[0xc3])).unwrap();

        assert!(cache.lookup(first).is_none());
        assert!(cache.lookup(third).is_some());
        assert!(cache.lookup(fourth).is_some());
        assert_eq!(cache.stats().metadata_evictions, 2);
    }

    #[test]
    fn rejects_non_power_of_two_set_count() {
        assert_eq!(
            DirectDbtCodeCache::new(3, PAGE_BYTES).err().unwrap().kind(),
            DbtFaultKind::Capacity
        );
    }

    #[test]
    fn invalidation_revokes_lookup_without_releasing_fixed_storage() {
        let mut cache = DirectDbtCodeCache::new(2, PAGE_BYTES).unwrap();
        let key = DbtCacheKey::new(0, 0);
        let published = cache.publish(key, &block(0, &[0xc3])).unwrap();
        let metadata_bytes = cache.metadata_bytes();

        assert!(!published.entry().is_null());
        assert_eq!(published.instruction_count(), 1);
        assert_eq!(cache.live_entry_bytes(), 1);
        cache.invalidate_all();

        assert!(cache.lookup(key).is_none());
        assert_eq!(cache.live_entry_bytes(), 0);
        assert_eq!(cache.metadata_bytes(), metadata_bytes);
        assert_eq!(cache.reserved_bytes(), PAGE_BYTES * 2);
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

    #[test]
    fn lazy_links_work_for_either_publication_order() {
        for target_first in [false, true] {
            let mut cache = DirectDbtCodeCache::new(8, PAGE_BYTES).unwrap();
            let source_key = DbtCacheKey::new(0x1000, 3);
            let target_key = DbtCacheKey::new(0x1004, 3);
            let (source_code, link) = source_link(0x1004);
            let target_code = returning(9);

            if target_first {
                cache
                    .publish(target_key, &block(0x1004, &target_code))
                    .unwrap();
            }
            let source = cache
                .publish(
                    source_key,
                    &block_with_links(0x1000, &source_code, 0, &[link]),
                )
                .unwrap();
            if target_first {
                assert_eq!(unsafe { execute(source.entry()) }, 9);
            } else {
                assert_eq!(unsafe { execute(source.entry()) }, 7);
                cache
                    .publish(target_key, &block(0x1004, &target_code))
                    .unwrap();
                assert_eq!(unsafe { execute(source.entry()) }, 9);
            }
            assert_eq!(cache.stats().links_established, 1);
        }
    }

    #[test]
    fn conditional_slots_link_independently() {
        let mut cache = DirectDbtCodeCache::new(8, PAGE_BYTES).unwrap();
        let mut source_code = [0_u8; 22];
        let (first, _) = source_link(0x2004);
        let (second, _) = source_link(0x2008);
        source_code[..11].copy_from_slice(&first);
        source_code[11..].copy_from_slice(&second);
        let links = [
            DbtStaticLink {
                target_pc: 0x2004,
                displacement_offset: 1,
                reset_target_offset: 5,
                kind: DbtLinkKind::BranchNotTaken,
            },
            DbtStaticLink {
                target_pc: 0x2008,
                displacement_offset: 12,
                reset_target_offset: 16,
                kind: DbtLinkKind::BranchTaken,
            },
        ];
        let source = cache
            .publish(
                DbtCacheKey::new(0x2000, 0),
                &block_with_links(0x2000, &source_code, 0, &links),
            )
            .unwrap();

        cache
            .publish(DbtCacheKey::new(0x2004, 0), &block(0x2004, &returning(9)))
            .unwrap();
        assert_eq!(unsafe { execute(source.entry()) }, 9);
        assert_eq!(unsafe { execute(source.entry().add(11)) }, 7);

        cache
            .publish(DbtCacheKey::new(0x2008, 0), &block(0x2008, &returning(11)))
            .unwrap();
        assert_eq!(unsafe { execute(source.entry().add(11)) }, 11);
        assert_eq!(cache.stats().links_established, 2);
    }

    #[test]
    fn destination_eviction_and_full_invalidation_reset_incoming_links() {
        let mut cache = DirectDbtCodeCache::new(2, PAGE_BYTES).unwrap();
        let (source_code, link) = source_link(0x3004);
        let source = cache
            .publish(
                DbtCacheKey::new(0x3000, 0),
                &block_with_links(0x3000, &source_code, 0, &[link]),
            )
            .unwrap();
        cache
            .publish(DbtCacheKey::new(0x3004, 0), &block(0x3004, &returning(9)))
            .unwrap();
        assert_eq!(unsafe { execute(source.entry()) }, 9);

        cache
            .publish(DbtCacheKey::new(0x300c, 0), &block(0x300c, &returning(10)))
            .unwrap();
        cache
            .publish(DbtCacheKey::new(0x3014, 0), &block(0x3014, &returning(11)))
            .unwrap();
        assert_eq!(unsafe { execute(source.entry()) }, 7);
        assert_eq!(cache.stats().links_reset, 1);

        cache
            .publish(DbtCacheKey::new(0x3004, 0), &block(0x3004, &returning(12)))
            .unwrap();
        assert_eq!(unsafe { execute(source.entry()) }, 12);
        cache.invalidate_all();
        assert_eq!(unsafe { execute(source.entry()) }, 7);
        assert_eq!(cache.stats().links_reset, 2);
    }

    #[test]
    fn circular_overwrite_unlinks_a_destination_before_replacing_its_bytes() {
        let mut cache = DirectDbtCodeCache::new(16, PAGE_BYTES).unwrap();
        cache
            .publish(DbtCacheKey::new(0x4004, 0), &block(0x4004, &returning(9)))
            .unwrap();
        let (source_code, link) = source_link(0x4004);
        let source = cache
            .publish(
                DbtCacheKey::new(0x4000, 0),
                &block_with_links(0x4000, &source_code, 0, &[link]),
            )
            .unwrap();
        assert_eq!(unsafe { execute(source.entry()) }, 9);

        let filler_len =
            PAGE_BYTES - super::align_up(cache.write_cursor, cache.code_alignment.bytes());
        cache
            .publish(
                DbtCacheKey::new(0x4048, 0),
                &block(0x4048, &vec![0x90; filler_len]),
            )
            .unwrap();
        cache
            .publish(DbtCacheKey::new(0x408c, 0), &block(0x408c, &returning(13)))
            .unwrap();

        assert_eq!(unsafe { execute(source.entry()) }, 7);
        assert_eq!(cache.stats().links_reset, 1);
    }

    #[test]
    fn cross_page_static_edge_never_links() {
        let mut cache = DirectDbtCodeCache::new(4, PAGE_BYTES).unwrap();
        let (source_code, link) = source_link(0x5000);
        let source = cache
            .publish(
                DbtCacheKey::new(0x4ffc, 0),
                &block_with_links(0x4ffc, &source_code, 0, &[link]),
            )
            .unwrap();
        cache
            .publish(DbtCacheKey::new(0x5000, 0), &block(0x5000, &returning(9)))
            .unwrap();

        assert_eq!(unsafe { execute(source.entry()) }, 7);
        assert_eq!(cache.stats().links_established, 0);
    }

    #[test]
    fn fixed_link_metadata_stays_compact_per_cache_entry() {
        let link = std::mem::size_of::<super::CacheLink>();
        let entry = std::mem::size_of::<super::CacheEntry>();
        assert!(link <= 16, "CacheLink is {link} bytes");
        #[cfg(not(feature = "dbt-code-audit"))]
        assert!(entry <= 80, "CacheEntry is {entry} bytes");
        #[cfg(feature = "dbt-code-audit")]
        assert!(entry <= 96, "audit CacheEntry is {entry} bytes");
    }
}
