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

use super::emitter::Gpr;
use crate::rv32_dbt::region::{
    LoopRegion, RegionMemoryEffectKind, RegionValueKind, ValueId, MAX_REGION_VALUES,
};

const REGION_HOSTS: [Gpr; 8] = [
    Gpr::Rbx,
    Gpr::Rbp,
    Gpr::Rsi,
    Gpr::R8,
    Gpr::R9,
    Gpr::R10,
    Gpr::R11,
    Gpr::Rcx,
];
const MAX_REGION_SPILL_SLOTS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostLocation {
    Empty,
    Constant(u32),
    Register(Gpr),
    Spill(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllocationFallback {
    SpillFrameExceeded,
}

#[derive(Debug, Clone, Copy)]
struct ActiveValue {
    value: ValueId,
    end: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct RegionAllocation {
    locations: [HostLocation; MAX_REGION_VALUES],
    spill_slots: u8,
    max_live: u8,
}

impl RegionAllocation {
    pub(crate) fn location(&self, value: ValueId) -> HostLocation {
        self.locations[value.index()]
    }

    pub(crate) fn register_values(&self) -> impl Iterator<Item = ValueId> + '_ {
        self.locations
            .iter()
            .enumerate()
            .filter_map(|(index, location)| {
                matches!(location, HostLocation::Register(_)).then(|| ValueId::from_index(index))
            })
    }

    pub(crate) const fn spill_slots(&self) -> u8 {
        self.spill_slots
    }

    pub(crate) const fn max_live(&self) -> u8 {
        self.max_live
    }

    pub(crate) const fn spill_frame_bytes(&self) -> usize {
        (self.spill_slots as usize * 8).next_multiple_of(16)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RegionAllocationWorkspace {
    allocation: RegionAllocation,
    live_ends: [usize; MAX_REGION_VALUES],
    active: [Option<ActiveValue>; REGION_HOSTS.len()],
}

impl RegionAllocationWorkspace {
    pub(crate) const fn new() -> Self {
        Self {
            allocation: RegionAllocation {
                locations: [HostLocation::Empty; MAX_REGION_VALUES],
                spill_slots: 0,
                max_live: 0,
            },
            live_ends: [0; MAX_REGION_VALUES],
            active: [None; REGION_HOSTS.len()],
        }
    }
}

pub(crate) fn allocate_region<'a>(
    region: &LoopRegion<'_>,
    workspace: &'a mut RegionAllocationWorkspace,
) -> Result<&'a RegionAllocation, AllocationFallback> {
    workspace.allocation.locations.fill(HostLocation::Empty);
    workspace.allocation.spill_slots = 0;
    workspace.allocation.max_live = 0;
    workspace.live_ends.fill(0);
    workspace.active.fill(None);
    compute_live_ends(region, &mut workspace.live_ends);

    for index in 0..region.value_count() {
        let Some((value, kind)) = region.value_at(index) else {
            continue;
        };
        if !region.is_value_live(value) {
            continue;
        }
        match kind {
            RegionValueKind::Constant(constant) => {
                workspace.allocation.locations[index] = HostLocation::Constant(constant);
                continue;
            }
            RegionValueKind::Empty => continue,
            _ => {}
        }
        for active in &mut workspace.active {
            if active.is_some_and(|active| active.end < index) {
                *active = None;
            }
        }
        let live_count = (0..=index)
            .filter(|candidate| {
                let Some((value, kind)) = region.value_at(*candidate) else {
                    return false;
                };
                region.is_value_live(value)
                    && !matches!(kind, RegionValueKind::Constant(_) | RegionValueKind::Empty)
                    && workspace.live_ends[*candidate] >= index
            })
            .count();
        workspace.allocation.max_live = workspace
            .allocation
            .max_live
            .max(live_count.min(u8::MAX as usize) as u8);
        if let Some((host_index, slot)) = workspace
            .active
            .iter_mut()
            .enumerate()
            .find(|(_, active)| active.is_none())
        {
            *slot = Some(ActiveValue {
                value,
                end: workspace.live_ends[index],
            });
            workspace.allocation.locations[index] =
                HostLocation::Register(REGION_HOSTS[host_index]);
        } else {
            let spill = usize::from(workspace.allocation.spill_slots);
            if spill == MAX_REGION_SPILL_SLOTS {
                return Err(AllocationFallback::SpillFrameExceeded);
            }
            workspace.allocation.locations[index] = HostLocation::Spill(spill as u8);
            workspace.allocation.spill_slots += 1;
        }
    }
    Ok(&workspace.allocation)
}

fn compute_live_ends(region: &LoopRegion<'_>, ends: &mut [usize; MAX_REGION_VALUES]) {
    for index in 0..region.value_count() {
        ends[index] = index;
        let Some((value, RegionValueKind::Binary { lhs, rhs, .. })) = region.value_at(index) else {
            continue;
        };
        if region.is_value_live(value) {
            ends[lhs.index()] = ends[lhs.index()].max(index);
            ends[rhs.index()] = ends[rhs.index()].max(index);
        }
    }
    let terminal = region.value_count();
    for guest in 1..32 {
        if let Some(output) = region.output_value(guest) {
            if region.entry_value(guest) != Some(output) {
                ends[output.index()] = terminal;
            }
        }
    }
    for index in 0..region.memory_effect_count() {
        match region.memory_effect(index).kind() {
            RegionMemoryEffectKind::Load {
                address, output, ..
            } => {
                ends[address.index()] = terminal;
                ends[output.index()] = terminal;
            }
            RegionMemoryEffectKind::Store { address, value, .. } => {
                ends[address.index()] = terminal;
                ends[value.index()] = terminal;
            }
            RegionMemoryEffectKind::Empty => {}
        }
    }
    if let Some(branch) = region.branch() {
        ends[branch.lhs.index()] = terminal;
        ends[branch.rhs.index()] = terminal;
    }
}

#[cfg(test)]
mod tests {
    use super::{allocate_region, HostLocation, RegionAllocationWorkspace};
    use crate::rv32_dbt::ir::DbtIrBlock;
    use crate::rv32_dbt::region::{LoopRegionWorkspace, RegionBuildOutcome};
    use crate::rv32_dbt::x86_64::emitter::Gpr;
    use crate::rv32im::encoding::{add, addi, bne};

    #[test]
    fn linear_scan_keeps_constants_out_of_abi_and_value_registers() {
        let words = [addi(5, 5, 1), add(6, 5, 7), bne(6, 8, -8)];
        let mut ir = DbtIrBlock::new(16).unwrap();
        for word in words {
            ir.lift_word(word).unwrap();
        }
        let mut region_workspace = LoopRegionWorkspace::new();
        let RegionBuildOutcome::Built(region) = region_workspace.build_optimized(0x1000, &ir)
        else {
            panic!("expected an optimized region")
        };
        let mut allocation_workspace = RegionAllocationWorkspace::new();

        let allocation = allocate_region(&region, &mut allocation_workspace).unwrap();

        assert!(matches!(
            allocation.location(region.constant_value(1).unwrap()),
            HostLocation::Constant(1)
        ));
        for value in allocation.register_values() {
            let HostLocation::Register(host) = allocation.location(value) else {
                unreachable!()
            };
            assert!(!matches!(
                host,
                Gpr::Rsp | Gpr::Rdi | Gpr::R12 | Gpr::R13 | Gpr::R14 | Gpr::R15
            ));
        }
    }

    #[test]
    fn linear_scan_uses_a_bounded_aligned_spill_frame_under_pressure() {
        let mut words = Vec::new();
        for register in 5..15 {
            words.push(addi(register, register, 1));
        }
        words.push(bne(5, 6, -40));
        let mut ir = DbtIrBlock::new(16).unwrap();
        for word in words {
            ir.lift_word(word).unwrap();
        }
        let mut region_workspace = LoopRegionWorkspace::new();
        let RegionBuildOutcome::Built(region) = region_workspace.build_optimized(0x1100, &ir)
        else {
            panic!("expected an optimized pressure region")
        };
        let mut allocation_workspace = RegionAllocationWorkspace::new();

        let allocation = allocate_region(&region, &mut allocation_workspace).unwrap();

        assert!(allocation.spill_slots() >= 2);
        assert_eq!(allocation.spill_frame_bytes() % 16, 0);
        assert!(allocation.max_live() > 8);
    }
}
