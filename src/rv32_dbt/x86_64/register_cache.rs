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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FutureValue {
    Read(usize),
    Dead(usize),
    Unused,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RegisterAccess {
    reads: u32,
    writes: u32,
    may_exit_before_write: bool,
}

impl RegisterAccess {
    fn reads(self, guest: usize) -> bool {
        guest != 0 && self.reads & (1_u32 << guest) != 0
    }

    fn writes(self, guest: usize) -> bool {
        guest != 0 && self.writes & (1_u32 << guest) != 0
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalLoopRegisterPlan {
    guests: [usize; CHAINABLE_HOST_POOL.len()],
    len: usize,
}

impl LocalLoopRegisterPlan {
    pub(crate) fn guests(&self) -> &[usize] {
        &self.guests[..self.len]
    }
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

    pub(crate) const fn is_chainable(&self) -> bool {
        self.host_pool.len() == CHAINABLE_HOST_POOL.len()
    }

    pub(crate) fn local_loop_plan(
        slots: &[Rv32ResolvedInstruction],
    ) -> Option<LocalLoopRegisterPlan> {
        let referenced = slots.iter().copied().fold(0_u32, |mask, slot| {
            let access = instruction_access(slot);
            mask | access.reads | access.writes
        }) & !1;
        if referenced.count_ones() as usize > CHAINABLE_HOST_POOL.len() {
            return None;
        }
        let mut guests = [0; CHAINABLE_HOST_POOL.len()];
        let mut len = 0;
        for guest in 1..32 {
            if referenced & (1_u32 << guest) != 0 {
                guests[len] = guest;
                len += 1;
            }
        }
        Some(LocalLoopRegisterPlan { guests, len })
    }

    pub(crate) fn preload_local_loop(
        &mut self,
        plan: LocalLoopRegisterPlan,
        slots: &[Rv32ResolvedInstruction],
        out: &mut X64Emitter,
    ) -> Result<(), EmitError> {
        debug_assert!(self.is_chainable());
        for guest in plan.guests() {
            self.read(*guest, slots, &[], out)?;
        }
        Ok(())
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
        let index = self.entries[..self.host_pool.len()]
            .iter()
            .enumerate()
            .filter_map(|(index, resident)| {
                let resident = resident.unwrap();
                let future = future_value(remaining, resident.guest);
                (!pinned.contains(&resident.host)).then_some((
                    index,
                    matches!(future, FutureValue::Dead(_)),
                    match future {
                        FutureValue::Read(distance) | FutureValue::Dead(distance) => distance,
                        FutureValue::Unused => usize::MAX,
                    },
                    !resident.dirty,
                    Reverse(resident.guest),
                ))
            })
            .max_by_key(|(_, dead, distance, clean, guest)| (*dead, *distance, *clean, *guest))
            .map(|(index, _, _, _, _)| index)
            .ok_or(EmitError::InvalidOperand(
                "all RV32 register-cache hosts are pinned",
            ))?;
        if let Some(resident) = self.entries[index] {
            let dead = matches!(
                future_value(remaining, resident.guest),
                FutureValue::Dead(_)
            );
            if resident.dirty && !dead {
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

fn future_value(slots: &[Rv32ResolvedInstruction], guest: usize) -> FutureValue {
    let mut crossed_exit = false;
    for (distance, slot) in slots.iter().copied().enumerate() {
        let access = instruction_access(slot);
        if access.reads(guest) {
            return FutureValue::Read(distance);
        }
        crossed_exit |= access.may_exit_before_write;
        if access.writes(guest) {
            return if crossed_exit {
                FutureValue::Read(distance)
            } else {
                FutureValue::Dead(distance)
            };
        }
    }
    FutureValue::Unused
}

fn instruction_access(slot: Rv32ResolvedInstruction) -> RegisterAccess {
    let Rv32ResolvedInstruction::Valid { instruction, .. } = slot else {
        return RegisterAccess {
            may_exit_before_write: true,
            ..RegisterAccess::default()
        };
    };
    let mut access = RegisterAccess::default();
    let mut read = |guest: usize| access.reads |= 1_u32 << guest;
    let mut write = |guest: usize| access.writes |= 1_u32 << guest;
    match instruction {
        DecodedInstruction::Lui { rd, .. }
        | DecodedInstruction::Auipc { rd, .. }
        | DecodedInstruction::Jal { rd, .. } => write(rd),
        DecodedInstruction::Jalr { rd, rs1, .. }
        | DecodedInstruction::Load { rd, rs1, .. }
        | DecodedInstruction::LoadReserved { rd, rs1, .. } => {
            read(rs1);
            write(rd);
        }
        DecodedInstruction::Branch { rs1, rs2, .. }
        | DecodedInstruction::Store { rs1, rs2, .. } => {
            read(rs1);
            read(rs2);
        }
        DecodedInstruction::Immediate { rd, rs1, .. } => {
            read(rs1);
            write(rd);
        }
        DecodedInstruction::Register { rd, rs1, rs2, .. }
        | DecodedInstruction::StoreConditional { rd, rs1, rs2, .. }
        | DecodedInstruction::Atomic { rd, rs1, rs2, .. } => {
            read(rs1);
            read(rs2);
            write(rd);
        }
        DecodedInstruction::Csr { rd, source, .. } => {
            if let CsrSource::Register(rs) = source {
                read(rs);
            }
            write(rd);
        }
        DecodedInstruction::Fence
        | DecodedInstruction::FenceI
        | DecodedInstruction::Ecall
        | DecodedInstruction::Ebreak
        | DecodedInstruction::Mret => {}
    }
    access.may_exit_before_write = matches!(
        instruction,
        DecodedInstruction::Jal { .. }
            | DecodedInstruction::Jalr { .. }
            | DecodedInstruction::Load { .. }
            | DecodedInstruction::Store { .. }
            | DecodedInstruction::LoadReserved { .. }
            | DecodedInstruction::StoreConditional { .. }
            | DecodedInstruction::Atomic { .. }
            | DecodedInstruction::Csr { .. }
            | DecodedInstruction::FenceI
            | DecodedInstruction::Ecall
            | DecodedInstruction::Ebreak
            | DecodedInstruction::Mret
    );
    access
}

#[cfg(test)]
mod tests {
    use super::{future_value, FutureValue, RegisterCache};
    use crate::rv32_dbt::x86_64::emitter::{Gpr, X64Emitter};
    use crate::rv32im::{
        decode_product_word,
        encoding::{addi, bne, ecall, jal, jalr, lw},
        Rv32ResolvedInstruction,
    };

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
    fn local_loop_plan_keeps_every_referenced_guest_resident() {
        let slots = [
            slot(addi(5, 0, 1)),
            slot(lw(6, 5, 0)),
            slot(addi(6, 6, 1)),
            slot(bne(6, 7, -12)),
        ];

        let plan = RegisterCache::local_loop_plan(&slots).unwrap();

        assert_eq!(plan.guests(), &[5, 6, 7]);
    }

    #[test]
    fn local_loop_plan_rejects_more_guests_than_the_chainable_pool() {
        let slots = (1..=8)
            .map(|guest| slot(addi(guest, guest, 1)))
            .collect::<Vec<_>>();

        assert!(RegisterCache::local_loop_plan(&slots).is_none());
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
        assert_eq!(
            cache
                .read(
                    9,
                    &future,
                    &[
                        Gpr::Rbx,
                        Gpr::Rbp,
                        Gpr::Rsi,
                        Gpr::Rdi,
                        Gpr::R8,
                        Gpr::R9,
                        Gpr::R10,
                    ],
                    &mut out,
                )
                .unwrap(),
            Gpr::R11
        );
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

        assert_eq!(
            cache
                .read(
                    9,
                    &future,
                    &[
                        Gpr::Rbx,
                        Gpr::Rbp,
                        Gpr::Rsi,
                        Gpr::Rdi,
                        Gpr::R8,
                        Gpr::R9,
                        Gpr::R10,
                    ],
                    &mut out,
                )
                .unwrap(),
            Gpr::R11
        );
        assert_eq!(
            &out.bytes()[before..],
            [0x45, 0x89, 0x5e, 0x24, 0x45, 0x8b, 0x5e, 0x28]
        );
    }

    #[test]
    fn farthest_read_beats_cleanliness_for_a_live_victim() {
        let mut cache = RegisterCache::new();
        let mut out = X64Emitter::new(512, 16).unwrap();
        for guest in 1..=8 {
            cache.read(guest, &[], &[], &mut out).unwrap();
        }
        cache.write(8, &[], &[], &mut out).unwrap();
        let future = (1..=8)
            .map(|guest| slot(addi(guest, guest, 1)))
            .collect::<Vec<_>>();

        assert_eq!(cache.read(9, &future, &[], &mut out).unwrap(), Gpr::R11);
    }

    #[test]
    fn future_value_distinguishes_reads_from_killing_writes() {
        assert_eq!(
            future_value(&[slot(addi(3, 0, 1))], 3),
            FutureValue::Dead(0)
        );
        assert_eq!(
            future_value(&[slot(addi(3, 3, 1))], 3),
            FutureValue::Read(0)
        );
        assert_eq!(
            future_value(&[slot(addi(4, 3, 1))], 3),
            FutureValue::Read(0)
        );
        assert_eq!(future_value(&[slot(addi(4, 0, 1))], 3), FutureValue::Unused);
    }

    #[test]
    fn future_value_does_not_drop_state_across_a_possible_exit() {
        assert_eq!(future_value(&[slot(lw(4, 5, 0))], 4), FutureValue::Read(0));
        assert_eq!(future_value(&[slot(lw(4, 5, 0))], 5), FutureValue::Read(0));
        assert_eq!(
            future_value(&[slot(ecall()), slot(addi(3, 0, 1))], 3),
            FutureValue::Read(1)
        );
        assert_eq!(future_value(&[slot(jal(3, 2))], 3), FutureValue::Read(0));
        assert_eq!(
            future_value(&[slot(jalr(3, 4, 2))], 3),
            FutureValue::Read(0)
        );
    }

    #[test]
    fn proven_dead_dirty_guest_is_evicted_without_a_store() {
        let mut cache = RegisterCache::new();
        let mut out = X64Emitter::new(512, 16).unwrap();
        for guest in 1..=8 {
            cache.read(guest, &[], &[], &mut out).unwrap();
        }
        cache.write(8, &[], &[], &mut out).unwrap();
        let before = out.bytes().len();
        let future = [
            slot(addi(8, 0, 1)),
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
        assert_eq!(&out.bytes()[before..], [0x45, 0x8b, 0x5e, 0x28]);
    }

    #[test]
    fn current_instruction_protects_a_not_yet_materialized_source() {
        let mut cache = RegisterCache::new();
        let mut out = X64Emitter::new(512, 16).unwrap();
        for guest in 1..=8 {
            cache.read(guest, &[], &[], &mut out).unwrap();
        }
        cache.write(8, &[], &[], &mut out).unwrap();
        let current_and_future = [
            slot(crate::rv32im::encoding::sub(9, 10, 8)),
            slot(addi(8, 0, 1)),
        ];

        cache.read(10, &current_and_future, &[], &mut out).unwrap();

        assert!(cache.resident_guests().contains(&8));
    }
}
