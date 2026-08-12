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
    reason = "the RV32 arithmetic lowerer consumes the register cache in the same issue #17 slice"
)]

use super::emitter::{EmitError, Gpr, Mem, X64Emitter};
use crate::rv32im::{
    CsrSource, DecodedInstruction, Rv32ArchitecturalState, Rv32ResolvedInstruction,
};
use std::cmp::Reverse;

const DIRECT_HOST_POOL: [Gpr; 8] = [
    Gpr::Rbx,
    Gpr::Rbp,
    Gpr::Rsi,
    Gpr::Rdi,
    Gpr::R8,
    Gpr::R9,
    Gpr::R10,
    Gpr::R11,
];

const CHAINABLE_HOST_POOL: [Gpr; 7] = [
    Gpr::Rbx,
    Gpr::Rbp,
    Gpr::Rsi,
    Gpr::R8,
    Gpr::R9,
    Gpr::R10,
    Gpr::R11,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Resident {
    guest: usize,
    host: Gpr,
    dirty: bool,
}

#[derive(Clone)]
pub(crate) struct RegisterCache {
    host_pool: &'static [Gpr],
    entries: [Option<Resident>; DIRECT_HOST_POOL.len()],
}

impl RegisterCache {
    pub(crate) const fn new() -> Self {
        Self::direct()
    }

    pub(crate) const fn direct() -> Self {
        Self {
            host_pool: &DIRECT_HOST_POOL,
            entries: [None; DIRECT_HOST_POOL.len()],
        }
    }

    pub(crate) const fn chainable() -> Self {
        Self {
            host_pool: &CHAINABLE_HOST_POOL,
            entries: [None; DIRECT_HOST_POOL.len()],
        }
    }

    pub(crate) fn read(
        &mut self,
        guest: usize,
        remaining: &[Rv32ResolvedInstruction],
        pinned: &[Gpr],
        out: &mut X64Emitter,
    ) -> Result<Gpr, EmitError> {
        if guest == 0 {
            out.xor_r32_r32(Gpr::Rax, Gpr::Rax)?;
            return Ok(Gpr::Rax);
        }
        if let Some(resident) = self.resident(guest) {
            return Ok(resident.host);
        }
        let index = self.allocate(remaining, pinned, out)?;
        let host = self.host_pool[index];
        out.mov_r32_m32(
            host,
            Mem::base_disp(
                Gpr::R14,
                Rv32ArchitecturalState::register_offset(guest) as i32,
            ),
        )?;
        self.entries[index] = Some(Resident {
            guest,
            host,
            dirty: false,
        });
        Ok(host)
    }

    pub(crate) fn write(
        &mut self,
        guest: usize,
        remaining: &[Rv32ResolvedInstruction],
        pinned: &[Gpr],
        out: &mut X64Emitter,
    ) -> Result<Option<Gpr>, EmitError> {
        if guest == 0 {
            return Ok(None);
        }
        if let Some(index) = self.index_of(guest) {
            let resident = self.entries[index].as_mut().unwrap();
            resident.dirty = true;
            return Ok(Some(resident.host));
        }
        let index = self.allocate(remaining, pinned, out)?;
        let host = self.host_pool[index];
        self.entries[index] = Some(Resident {
            guest,
            host,
            dirty: true,
        });
        Ok(Some(host))
    }

    pub(crate) fn flush(&mut self, out: &mut X64Emitter) -> Result<(), EmitError> {
        for resident in self.entries.iter_mut().flatten() {
            if resident.dirty {
                out.mov_m32_r32(
                    Mem::base_disp(
                        Gpr::R14,
                        Rv32ArchitecturalState::register_offset(resident.guest) as i32,
                    ),
                    resident.host,
                )?;
                resident.dirty = false;
            }
        }
        Ok(())
    }

    fn allocate(
        &mut self,
        remaining: &[Rv32ResolvedInstruction],
        pinned: &[Gpr],
        out: &mut X64Emitter,
    ) -> Result<usize, EmitError> {
        if let Some(index) = self.entries[..self.host_pool.len()]
            .iter()
            .position(Option::is_none)
        {
            return Ok(index);
        }
        let index = self
            .entries[..self.host_pool.len()]
            .iter()
            .enumerate()
            .filter_map(|(index, resident)| {
                let resident = resident.unwrap();
                (!pinned.contains(&resident.host)).then_some((
                    index,
                    next_use(remaining, resident.guest).unwrap_or(usize::MAX),
                    Reverse(resident.guest),
                ))
            })
            .max_by_key(|(_, next, guest)| (*next, *guest))
            .map(|(index, _, _)| index)
            .ok_or(EmitError::InvalidOperand(
                "all RV32 register-cache hosts are pinned",
            ))?;
        if let Some(resident) = self.entries[index] {
            if resident.dirty {
                out.mov_m32_r32(
                    Mem::base_disp(
                        Gpr::R14,
                        Rv32ArchitecturalState::register_offset(resident.guest) as i32,
                    ),
                    resident.host,
                )?;
            }
        }
        self.entries[index] = None;
        Ok(index)
    }

    fn resident(&self, guest: usize) -> Option<Resident> {
        self.entries
            .iter()
            .flatten()
            .find(|entry| entry.guest == guest)
            .copied()
    }

    fn index_of(&self, guest: usize) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.guest == guest))
    }

    #[cfg(test)]
    fn resident_guests(&self) -> Vec<usize> {
        let mut guests = self
            .entries
            .iter()
            .flatten()
            .map(|entry| entry.guest)
            .collect::<Vec<_>>();
        guests.sort_unstable();
        guests
    }

    #[cfg(test)]
    const fn host_pool(&self) -> &'static [Gpr] {
        self.host_pool
    }
}

fn next_use(slots: &[Rv32ResolvedInstruction], guest: usize) -> Option<usize> {
    slots
        .iter()
        .position(|slot| instruction_mentions(*slot, guest))
}

fn instruction_mentions(slot: Rv32ResolvedInstruction, guest: usize) -> bool {
    let Rv32ResolvedInstruction::Valid { instruction, .. } = slot else {
        return false;
    };
    match instruction {
        DecodedInstruction::Lui { rd, .. }
        | DecodedInstruction::Auipc { rd, .. }
        | DecodedInstruction::Jal { rd, .. } => rd == guest,
        DecodedInstruction::Jalr { rd, rs1, .. }
        | DecodedInstruction::Load { rd, rs1, .. }
        | DecodedInstruction::LoadReserved { rd, rs1, .. } => rd == guest || rs1 == guest,
        DecodedInstruction::Branch { rs1, rs2, .. }
        | DecodedInstruction::Store { rs1, rs2, .. } => rs1 == guest || rs2 == guest,
        DecodedInstruction::Immediate { rd, rs1, .. } => rd == guest || rs1 == guest,
        DecodedInstruction::Register { rd, rs1, rs2, .. }
        | DecodedInstruction::StoreConditional { rd, rs1, rs2, .. }
        | DecodedInstruction::Atomic { rd, rs1, rs2, .. } => {
            rd == guest || rs1 == guest || rs2 == guest
        }
        DecodedInstruction::Csr { rd, source, .. } => {
            rd == guest || matches!(source, CsrSource::Register(rs) if rs == guest)
        }
        DecodedInstruction::Fence
        | DecodedInstruction::FenceI
        | DecodedInstruction::Ecall
        | DecodedInstruction::Ebreak
        | DecodedInstruction::Mret => false,
    }
}

#[cfg(test)]
mod tests {
    use super::RegisterCache;
    use crate::rv32_dbt::x86_64::emitter::{Gpr, X64Emitter};
    use crate::rv32im::{decode_product_word, encoding::addi, Rv32ResolvedInstruction};

    fn slot(word: u32) -> Rv32ResolvedInstruction {
        Rv32ResolvedInstruction::Valid {
            word,
            instruction: decode_product_word(word).unwrap(),
        }
    }

    #[test]
    fn chainable_cache_reserves_only_rdi() {
        assert_eq!(
            RegisterCache::direct().host_pool(),
            &[
                Gpr::Rbx,
                Gpr::Rbp,
                Gpr::Rsi,
                Gpr::Rdi,
                Gpr::R8,
                Gpr::R9,
                Gpr::R10,
                Gpr::R11,
            ]
        );
        assert_eq!(
            RegisterCache::chainable().host_pool(),
            &[
                Gpr::Rbx,
                Gpr::Rbp,
                Gpr::Rsi,
                Gpr::R8,
                Gpr::R9,
                Gpr::R10,
                Gpr::R11,
            ]
        );
    }

    #[test]
    fn x0_is_materialized_but_never_resident_or_dirty() {
        let mut cache = RegisterCache::new();
        let mut out = X64Emitter::new(128, 16).unwrap();

        assert_eq!(cache.read(0, &[], &[], &mut out).unwrap(), Gpr::Rax);
        assert_eq!(cache.write(0, &[], &[], &mut out).unwrap(), None);
        cache.flush(&mut out).unwrap();

        assert_eq!(out.finish().unwrap(), [0x31, 0xc0]);
        assert!(cache.resident_guests().is_empty());
    }

    #[test]
    fn first_read_loads_once_and_dirty_flush_writes_once() {
        let mut cache = RegisterCache::new();
        let mut out = X64Emitter::new(128, 16).unwrap();

        let host = cache.read(3, &[], &[], &mut out).unwrap();
        assert_eq!(cache.read(3, &[], &[], &mut out).unwrap(), host);
        assert_eq!(cache.write(3, &[], &[], &mut out).unwrap(), Some(host));
        cache.flush(&mut out).unwrap();

        assert_eq!(
            out.finish().unwrap(),
            [0x41, 0x8b, 0x5e, 0x10, 0x41, 0x89, 0x5e, 0x10]
        );
    }

    #[test]
    fn overwrite_allocates_without_loading_old_value() {
        let mut cache = RegisterCache::new();
        let mut out = X64Emitter::new(128, 16).unwrap();

        assert_eq!(cache.write(7, &[], &[], &mut out).unwrap(), Some(Gpr::Rbx));

        assert!(out.bytes().is_empty());
    }

    #[test]
    fn ninth_guest_evicts_no_future_use_then_farthest_next_use() {
        let mut cache = RegisterCache::new();
        let mut out = X64Emitter::new(512, 16).unwrap();
        for guest in 1..=8 {
            cache.read(guest, &[], &[], &mut out).unwrap();
        }
        assert_eq!(cache.resident_guests(), vec![1, 2, 3, 4, 5, 6, 7, 8]);

        let future = [
            slot(addi(1, 1, 1)),
            slot(addi(2, 2, 1)),
            slot(addi(3, 3, 1)),
            slot(addi(4, 4, 1)),
            slot(addi(5, 5, 1)),
            slot(addi(6, 6, 1)),
            slot(addi(7, 7, 1)),
        ];
        assert_eq!(cache.read(9, &future, &[], &mut out).unwrap(), Gpr::R11);
        assert_eq!(cache.resident_guests(), vec![1, 2, 3, 4, 5, 6, 7, 9]);

        let future = [
            slot(addi(1, 1, 1)),
            slot(addi(2, 2, 1)),
            slot(addi(3, 3, 1)),
            slot(addi(4, 4, 1)),
            slot(addi(5, 5, 1)),
            slot(addi(6, 6, 1)),
            slot(addi(7, 7, 1)),
            slot(addi(9, 9, 1)),
        ];
        assert_eq!(cache.read(10, &future, &[], &mut out).unwrap(), Gpr::R11);
        assert_eq!(cache.resident_guests(), vec![1, 2, 3, 4, 5, 6, 7, 10]);
    }

    #[test]
    fn dirty_eviction_writes_canonical_state_before_reusing_host() {
        let mut cache = RegisterCache::new();
        let mut out = X64Emitter::new(512, 16).unwrap();
        for guest in 1..=8 {
            cache.read(guest, &[], &[], &mut out).unwrap();
        }
        cache.write(8, &[], &[], &mut out).unwrap();
        let before = out.bytes().len();
        let future = (1..=7)
            .map(|guest| slot(addi(guest, guest, 1)))
            .collect::<Vec<_>>();

        assert_eq!(cache.read(9, &future, &[], &mut out).unwrap(), Gpr::R11);
        assert_eq!(
            &out.bytes()[before..],
            [0x45, 0x89, 0x5e, 0x24, 0x45, 0x8b, 0x5e, 0x28]
        );
    }
}
