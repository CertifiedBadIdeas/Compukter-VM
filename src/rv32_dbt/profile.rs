/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

use super::{DbtFault, DbtFaultKind};
use crate::rv32_machine::{
    Rv32DbtDynamicExitCounts, Rv32DbtExecutionProfile, Rv32DbtProfileBlock, Rv32DbtProfileEdge,
    Rv32DbtProfileEdgeKind,
};
use std::cell::UnsafeCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DbtProfileKey {
    Block {
        pc: u32,
    },
    Edge {
        source_pc: u32,
        target_pc: u32,
        kind: Rv32DbtProfileEdgeKind,
    },
}

#[derive(Debug)]
struct ProfileSlot {
    key: Option<DbtProfileKey>,
    count: UnsafeCell<u64>,
    overflows: UnsafeCell<u64>,
}

impl ProfileSlot {
    fn empty() -> Self {
        Self {
            key: None,
            count: UnsafeCell::new(0),
            overflows: UnsafeCell::new(0),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProfileCounterAddresses {
    pub(crate) count: *mut u64,
    pub(crate) overflows: *mut u64,
}

pub(crate) struct ExactDbtProfile {
    slots: Box<[ProfileSlot]>,
    used: usize,
    dynamic_exits: Rv32DbtDynamicExitCounts,
}

impl ExactDbtProfile {
    pub(crate) fn new(capacity: usize) -> Result<Self, DbtFault> {
        if capacity == 0 || !capacity.is_power_of_two() {
            return Err(Self::capacity_fault(
                "DBT execution-profile capacity must be a positive power of two",
            ));
        }
        let slots = std::iter::repeat_with(ProfileSlot::empty)
            .take(capacity)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Self {
            slots,
            used: 0,
            dynamic_exits: Rv32DbtDynamicExitCounts::default(),
        })
    }

    pub(crate) fn counter_for(
        &mut self,
        key: DbtProfileKey,
    ) -> Result<ProfileCounterAddresses, DbtFault> {
        let mask = self.slots.len() - 1;
        let start = profile_hash(key) as usize & mask;
        for distance in 0..self.slots.len() {
            let index = start.wrapping_add(distance) & mask;
            let slot = &mut self.slots[index];
            match slot.key {
                Some(stored) if stored == key => return Ok(counter_addresses(slot)),
                Some(_) => {}
                None => {
                    slot.key = Some(key);
                    self.used += 1;
                    return Ok(counter_addresses(slot));
                }
            }
        }
        Err(Self::capacity_fault(
            "DBT execution-profile table has no free record",
        ))
    }

    pub(crate) fn snapshot(&self) -> Rv32DbtExecutionProfile {
        let mut blocks = Vec::new();
        let mut static_edges = Vec::new();
        let mut counter_overflowed = false;
        for slot in &self.slots {
            let Some(key) = slot.key else { continue };
            // Native profile code cannot run concurrently with this immutable VM snapshot.
            let executions = unsafe { *slot.count.get() };
            counter_overflowed |= unsafe { *slot.overflows.get() } != 0;
            match key {
                DbtProfileKey::Block { pc } => {
                    blocks.push(Rv32DbtProfileBlock { pc, executions });
                }
                DbtProfileKey::Edge {
                    source_pc,
                    target_pc,
                    kind,
                } => static_edges.push(Rv32DbtProfileEdge {
                    source_pc,
                    target_pc,
                    kind,
                    executions,
                }),
            }
        }
        blocks.sort_unstable_by(|lhs, rhs| {
            rhs.executions
                .cmp(&lhs.executions)
                .then_with(|| lhs.pc.cmp(&rhs.pc))
        });
        static_edges.sort_unstable_by(|lhs, rhs| {
            rhs.executions
                .cmp(&lhs.executions)
                .then_with(|| lhs.source_pc.cmp(&rhs.source_pc))
                .then_with(|| lhs.target_pc.cmp(&rhs.target_pc))
                .then_with(|| lhs.kind.cmp(&rhs.kind))
        });
        Rv32DbtExecutionProfile {
            blocks,
            static_edges,
            dynamic_exits: self.dynamic_exits,
            capacity: self.slots.len(),
            used_records: self.used,
            retained_bytes: self.retained_bytes(),
            counter_overflowed,
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(
            self.slots
                .len()
                .saturating_mul(std::mem::size_of::<ProfileSlot>()),
        )
    }

    fn capacity_fault(message: &'static str) -> DbtFault {
        DbtFault::new(DbtFaultKind::Capacity, 0, None, message)
    }
}

fn counter_addresses(slot: &ProfileSlot) -> ProfileCounterAddresses {
    ProfileCounterAddresses {
        count: slot.count.get(),
        overflows: slot.overflows.get(),
    }
}

fn profile_hash(key: DbtProfileKey) -> u64 {
    let value = match key {
        DbtProfileKey::Block { pc } => u64::from(pc),
        DbtProfileKey::Edge {
            source_pc,
            target_pc,
            kind,
        } => {
            let kind = match kind {
                Rv32DbtProfileEdgeKind::Taken => 1_u64,
                Rv32DbtProfileEdgeKind::Fallthrough => 2,
                Rv32DbtProfileEdgeKind::Jump => 3,
            };
            u64::from(source_pc) | (u64::from(target_pc) << 32) ^ kind.rotate_left(17)
        }
    };
    let mut mixed = value ^ (value >> 30);
    mixed = mixed.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed ^= mixed >> 27;
    mixed = mixed.wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}

#[cfg(test)]
mod tests {
    use super::{DbtProfileKey, ExactDbtProfile};
    use crate::rv32_dbt::DbtFaultKind;

    #[test]
    fn profile_reuses_keys_and_rejects_a_full_table() {
        let mut profile = ExactDbtProfile::new(4).unwrap();
        let block = DbtProfileKey::Block { pc: 0x1000 };
        let first = profile.counter_for(block).unwrap();
        let second = profile.counter_for(block).unwrap();
        assert_eq!(first.count, second.count);

        profile
            .counter_for(DbtProfileKey::Block { pc: 0x1004 })
            .unwrap();
        profile
            .counter_for(DbtProfileKey::Block { pc: 0x1008 })
            .unwrap();
        profile
            .counter_for(DbtProfileKey::Block { pc: 0x100c })
            .unwrap();
        assert_eq!(
            profile
                .counter_for(DbtProfileKey::Block { pc: 0x1010 })
                .unwrap_err()
                .kind(),
            DbtFaultKind::Capacity
        );
    }

    #[test]
    fn snapshot_sorts_counts_and_reports_overflow() {
        let mut profile = ExactDbtProfile::new(4).unwrap();
        let hot = profile
            .counter_for(DbtProfileKey::Block { pc: 0x1000 })
            .unwrap();
        let cold = profile
            .counter_for(DbtProfileKey::Block { pc: 0x1004 })
            .unwrap();
        unsafe {
            hot.count.write(9);
            hot.overflows.write(1);
            cold.count.write(3);
        }

        let snapshot = profile.snapshot();
        assert_eq!(snapshot.blocks[0].pc, 0x1000);
        assert_eq!(snapshot.blocks[0].executions, 9);
        assert!(snapshot.counter_overflowed);
        assert_eq!(snapshot.used_records, 2);
        assert_eq!(snapshot.capacity, 4);
    }
}
