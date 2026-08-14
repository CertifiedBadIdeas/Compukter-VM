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
use crate::rv32_dbt::block::DbtFutureValues;
use crate::rv32_dbt::ir::{DbtIrBlock, FutureValue};
use crate::rv32im::{
    CsrSource, DecodedInstruction, Rv32ArchitecturalState, Rv32ResolvedInstruction,
};
use std::cmp::Reverse;

pub(crate) trait FutureValueSource: Copy {
    fn value(self, guest: usize) -> FutureValue;
}

impl FutureValueSource for &[Rv32ResolvedInstruction] {
    fn value(self, guest: usize) -> FutureValue {
        future_value(self, guest)
    }
}

impl<const N: usize> FutureValueSource for &[Rv32ResolvedInstruction; N] {
    fn value(self, guest: usize) -> FutureValue {
        future_value(self.as_slice(), guest)
    }
}

impl FutureValueSource for &Vec<Rv32ResolvedInstruction> {
    fn value(self, guest: usize) -> FutureValue {
        future_value(self.as_slice(), guest)
    }
}

impl FutureValueSource for DbtFutureValues<'_, '_> {
    fn value(self, guest: usize) -> FutureValue {
        self.value(guest)
    }
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
    written: u32,
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
    protected_guests: u32,
}

impl RegisterCache {
    pub(crate) const fn new() -> Self {
        Self::direct()
    }

    pub(crate) const fn direct() -> Self {
        Self {
            host_pool: &DIRECT_HOST_POOL,
            entries: [None; DIRECT_HOST_POOL.len()],
            protected_guests: 0,
        }
    }

    pub(crate) const fn chainable() -> Self {
        Self {
            host_pool: &CHAINABLE_HOST_POOL,
            entries: [None; DIRECT_HOST_POOL.len()],
            protected_guests: 0,
        }
    }

    pub(crate) const fn is_chainable(&self) -> bool {
        self.host_pool.len() == CHAINABLE_HOST_POOL.len()
    }

    pub(crate) fn local_loop_plan(
        slots: &[Rv32ResolvedInstruction],
    ) -> Option<LocalLoopRegisterPlan> {
        Self::local_loop_plan_from_accesses(slots.iter().copied().map(instruction_access))
    }

    pub(crate) fn local_loop_plan_ir(ir: &DbtIrBlock) -> Option<LocalLoopRegisterPlan> {
        Self::local_loop_plan_from_accesses(ir.instructions().iter().copied().map(|instruction| {
            let effects = instruction.effects();
            let mut access = RegisterAccess::default();
            for register in effects.reads() {
                access.reads |= 1_u32 << register;
            }
            if let Some(register) = effects.write() {
                access.writes |= 1_u32 << register;
            }
            access.may_exit_before_write = effects.may_exit_before_write();
            access
        }))
    }

    fn local_loop_plan_from_accesses(
        accesses: impl Iterator<Item = RegisterAccess> + Clone,
    ) -> Option<LocalLoopRegisterPlan> {
        let mut defined = 1_u32;
        let mut carried = 0_u32;
        let mut referenced = 0_u32;
        let mut written = 0_u32;
        for access in accesses.clone() {
            carried |= access.reads & !defined;
            defined |= access.writes;
            referenced |= access.reads | access.writes;
            written |= access.writes;
        }
        carried &= !1;
        referenced &= !1;
        let resident = if referenced.count_ones() as usize <= CHAINABLE_HOST_POOL.len() {
            referenced
        } else {
            carried
        };
        let resident_count = resident.count_ones() as usize;
        let temporary_pressure = accesses
            .map(|access| ((access.reads | access.writes) & !resident & !1).count_ones() as usize)
            .max()
            .unwrap_or(0);
        if resident_count > CHAINABLE_HOST_POOL.len()
            || resident_count.saturating_add(temporary_pressure) > CHAINABLE_HOST_POOL.len()
        {
            return None;
        }
        let mut guests = [0; CHAINABLE_HOST_POOL.len()];
        let mut len = 0;
        for guest in 1..32 {
            if resident & (1_u32 << guest) != 0 {
                guests[len] = guest;
                len += 1;
            }
        }
        Some(LocalLoopRegisterPlan {
            guests,
            len,
            written: written & resident,
        })
    }

    pub(crate) fn preload_local_loop<S: FutureValueSource>(
        &mut self,
        plan: LocalLoopRegisterPlan,
        future: S,
        out: &mut X64Emitter,
    ) -> Result<(), EmitError> {
        debug_assert!(self.is_chainable());
        self.protected_guests = plan
            .guests()
            .iter()
            .fold(0_u32, |mask, guest| mask | (1_u32 << guest));
        for guest in plan.guests() {
            self.read(*guest, future, &[], out)?;
            if plan.written & (1_u32 << guest) != 0 {
                self.entries[self.index_of(*guest).unwrap()]
                    .as_mut()
                    .unwrap()
                    .dirty = true;
            }
        }
        Ok(())
    }

    pub(crate) fn read<S: FutureValueSource>(
        &mut self,
        guest: usize,
        future: S,
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
        let index = self.allocate(future, pinned, out)?;
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

    pub(crate) fn write<S: FutureValueSource>(
        &mut self,
        guest: usize,
        future: S,
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
        let index = self.allocate(future, pinned, out)?;
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

    pub(crate) fn reconcile_local_loop(&mut self, out: &mut X64Emitter) -> Result<(), EmitError> {
        for entry in &mut self.entries[..self.host_pool.len()] {
            let Some(resident) = *entry else {
                continue;
            };
            if self.protected_guests & (1_u32 << resident.guest) != 0 {
                continue;
            }
            if resident.dirty {
                out.mov_m32_r32(
                    Mem::base_disp(
                        Gpr::R14,
                        Rv32ArchitecturalState::register_offset(resident.guest) as i32,
                    ),
                    resident.host,
                )?;
            }
            *entry = None;
        }
        Ok(())
    }

    fn allocate<S: FutureValueSource>(
        &mut self,
        future_values: S,
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
                let future = future_values.value(resident.guest);
                (!pinned.contains(&resident.host)
                    && self.protected_guests & (1_u32 << resident.guest) == 0)
                    .then_some((
                        index,
                        matches!(future, FutureValue::Dead(_)),
                        match future {
                            FutureValue::Read(distance) | FutureValue::Dead(distance) => distance,
                            FutureValue::Unused => u8::MAX,
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
            let dead = matches!(future_values.value(resident.guest), FutureValue::Dead(_));
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
            return FutureValue::Read(distance as u8);
        }
        crossed_exit |= access.may_exit_before_write;
        if access.writes(guest) {
            return if crossed_exit {
                FutureValue::Read(distance as u8)
            } else {
                FutureValue::Dead(distance as u8)
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
        encoding::{add, addi, bne, ecall, jal, jalr, lw, slli, sw},
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
    fn local_loop_plan_retains_every_reference_when_the_complete_mapping_fits() {
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
    fn local_loop_plan_accepts_nine_references_with_five_carried_values() {
        let slots = [
            slot(lw(20, 5, 0)),
            slot(lw(24, 28, 0)),
            slot(addi(5, 5, 4)),
            slot(addi(28, 28, 4)),
            slot(slli(25, 20, 5)),
            slot(slli(26, 24, 4)),
            slot(add(20, 20, 14)),
            slot(add(24, 26, 24)),
            slot(add(20, 25, 20)),
            slot(add(20, 20, 24)),
            slot(sw(18, 20, 0)),
            slot(addi(18, 18, 4)),
            slot(bne(5, 15, -48)),
        ];

        let plan = RegisterCache::local_loop_plan(&slots).unwrap();

        assert_eq!(plan.guests(), &[5, 14, 15, 18, 28]);
    }

    #[test]
    fn local_loop_plan_rejects_more_per_instruction_temporaries_than_free_hosts() {
        let slots = [
            slot(addi(1, 1, 1)),
            slot(addi(2, 2, 1)),
            slot(addi(3, 3, 1)),
            slot(addi(4, 4, 1)),
            slot(addi(5, 5, 1)),
            slot(addi(20, 0, 1)),
            slot(addi(21, 0, 2)),
            slot(addi(22, 0, 3)),
            slot(add(20, 21, 22)),
            slot(bne(1, 2, -36)),
        ];

        assert!(RegisterCache::local_loop_plan(&slots).is_none());
    }

    #[test]
    fn loop_carried_hosts_survive_temporary_register_pressure() {
        let slots = [
            slot(lw(20, 5, 0)),
            slot(lw(24, 28, 0)),
            slot(add(25, 20, 14)),
            slot(add(26, 24, 15)),
            slot(sw(18, 26, 0)),
            slot(bne(5, 15, -20)),
        ];
        let plan = RegisterCache::local_loop_plan(&slots).unwrap();
        let mut cache = RegisterCache::chainable();
        let mut out = X64Emitter::new(512, slots.len()).unwrap();
        cache.preload_local_loop(plan, &slots, &mut out).unwrap();

        for guest in [20, 24, 25] {
            cache.read(guest, &[], &[], &mut out).unwrap();
        }

        for guest in plan.guests() {
            assert!(
                cache.resident(*guest).is_some(),
                "carried x{guest} was evicted"
            );
        }
    }

    #[test]
    fn local_loop_reconciliation_materializes_and_discards_only_temporaries() {
        let slots = [
            slot(lw(20, 5, 0)),
            slot(lw(24, 28, 0)),
            slot(addi(5, 5, 4)),
            slot(addi(28, 28, 4)),
            slot(slli(25, 20, 5)),
            slot(slli(26, 24, 4)),
            slot(add(20, 20, 14)),
            slot(add(24, 26, 24)),
            slot(add(20, 25, 20)),
            slot(add(20, 20, 24)),
            slot(sw(18, 20, 0)),
            slot(addi(18, 18, 4)),
            slot(bne(5, 15, -48)),
        ];
        let plan = RegisterCache::local_loop_plan(&slots).unwrap();
        assert!(!plan.guests().contains(&20));
        let mut cache = RegisterCache::chainable();
        let mut out = X64Emitter::new(512, slots.len()).unwrap();
        cache.preload_local_loop(plan, &slots, &mut out).unwrap();
        cache.write(20, &[], &[], &mut out).unwrap();
        let before = out.bytes().len();

        cache.reconcile_local_loop(&mut out).unwrap();

        assert!(out.bytes().len() > before);
        assert!(cache.resident(20).is_none());
        for guest in plan.guests() {
            assert!(cache.resident(*guest).is_some());
        }
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
