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

use super::region_alloc::HostLocation;

const MAX_COPY_PAIRS: usize = 31;
const MAX_COPY_ACTIONS: usize = MAX_COPY_PAIRS * 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CopyPair {
    pub(crate) source: HostLocation,
    pub(crate) destination: HostLocation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopySource {
    Location(HostLocation),
    Scratch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReconciliationAction {
    Save(HostLocation),
    Move {
        source: CopySource,
        destination: HostLocation,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyPlanError {
    Capacity,
    InvalidSource,
    InvalidDestination,
    DuplicateDestination,
    ScratchDependency,
}

#[derive(Debug, Clone, Copy)]
struct PendingCopy {
    source: CopySource,
    destination: HostLocation,
}

#[derive(Debug, Clone)]
pub(crate) struct ReconciliationPlan {
    actions: [Option<ReconciliationAction>; MAX_COPY_ACTIONS],
    action_count: usize,
}

impl ReconciliationPlan {
    pub(crate) fn build(copies: &[CopyPair]) -> Result<Self, CopyPlanError> {
        let mut pending: [Option<PendingCopy>; MAX_COPY_PAIRS] = [None; MAX_COPY_PAIRS];
        let mut pending_count = 0;
        for copy in copies {
            validate_pair(*copy)?;
            if copy.source == copy.destination {
                continue;
            }
            if pending[..pending_count]
                .iter()
                .flatten()
                .any(|pending| pending.destination == copy.destination)
            {
                return Err(CopyPlanError::DuplicateDestination);
            }
            let slot = pending
                .get_mut(pending_count)
                .ok_or(CopyPlanError::Capacity)?;
            *slot = Some(PendingCopy {
                source: CopySource::Location(copy.source),
                destination: copy.destination,
            });
            pending_count += 1;
        }

        let mut plan = Self {
            actions: [None; MAX_COPY_ACTIONS],
            action_count: 0,
        };
        while pending_count != 0 {
            if let Some(index) = ready_copy_index(&pending, pending_count) {
                let copy = remove_pending(&mut pending, &mut pending_count, index);
                plan.push(ReconciliationAction::Move {
                    source: copy.source,
                    destination: copy.destination,
                })?;
                continue;
            }

            if pending[..pending_count]
                .iter()
                .flatten()
                .any(|copy| copy.source == CopySource::Scratch)
            {
                return Err(CopyPlanError::ScratchDependency);
            }
            let destination = pending[0].ok_or(CopyPlanError::Capacity)?.destination;
            plan.push(ReconciliationAction::Save(destination))?;
            for copy in pending[..pending_count].iter_mut().flatten() {
                if copy.source == CopySource::Location(destination) {
                    copy.source = CopySource::Scratch;
                }
            }
        }
        Ok(plan)
    }

    pub(crate) fn actions(&self) -> impl Iterator<Item = &ReconciliationAction> {
        self.actions[..self.action_count].iter().flatten()
    }

    fn push(&mut self, action: ReconciliationAction) -> Result<(), CopyPlanError> {
        let slot = self
            .actions
            .get_mut(self.action_count)
            .ok_or(CopyPlanError::Capacity)?;
        *slot = Some(action);
        self.action_count += 1;
        Ok(())
    }
}

fn validate_pair(copy: CopyPair) -> Result<(), CopyPlanError> {
    if copy.source == HostLocation::Empty {
        return Err(CopyPlanError::InvalidSource);
    }
    if !matches!(
        copy.destination,
        HostLocation::Register(_) | HostLocation::Spill(_)
    ) {
        return Err(CopyPlanError::InvalidDestination);
    }
    Ok(())
}

fn ready_copy_index(
    pending: &[Option<PendingCopy>; MAX_COPY_PAIRS],
    pending_count: usize,
) -> Option<usize> {
    (0..pending_count).find(|candidate| {
        let destination = pending[*candidate]
            .expect("dense pending copies")
            .destination;
        !pending[..pending_count]
            .iter()
            .flatten()
            .any(|copy| copy.source == CopySource::Location(destination))
    })
}

fn remove_pending(
    pending: &mut [Option<PendingCopy>; MAX_COPY_PAIRS],
    pending_count: &mut usize,
    index: usize,
) -> PendingCopy {
    let copy = pending[index].take().expect("dense pending copies");
    *pending_count -= 1;
    pending[index] = pending[*pending_count].take();
    copy
}

#[cfg(test)]
mod tests {
    use super::{CopyPair, CopySource, ReconciliationAction, ReconciliationPlan};
    use crate::rv32_dbt::x86_64::emitter::Gpr;
    use crate::rv32_dbt::x86_64::region_alloc::HostLocation;

    #[test]
    fn plans_parallel_copies_without_losing_values() {
        let cases: &[&[CopyPair]] = &[
            &[
                pair(reg(Gpr::Rbx), reg(Gpr::Rbx)),
                pair(reg(Gpr::Rbp), reg(Gpr::Rsi)),
            ],
            &[
                pair(reg(Gpr::Rbx), reg(Gpr::Rbp)),
                pair(reg(Gpr::Rsi), reg(Gpr::Rbx)),
            ],
            &[
                pair(reg(Gpr::Rbx), reg(Gpr::Rbp)),
                pair(reg(Gpr::Rbp), reg(Gpr::Rbx)),
            ],
            &[
                pair(reg(Gpr::Rbx), reg(Gpr::Rbp)),
                pair(reg(Gpr::Rbp), spill(0)),
                pair(spill(0), reg(Gpr::Rbx)),
            ],
            &[
                pair(spill(0), reg(Gpr::Rbx)),
                pair(reg(Gpr::Rbx), spill(1)),
                pair(spill(2), HostLocation::Constant(0x1234_5678)),
            ],
        ];

        for copies in cases {
            let plan = ReconciliationPlan::build(copies).expect("copy plan");
            assert_parallel_semantics(copies, &plan);
        }
    }

    fn assert_parallel_semantics(copies: &[CopyPair], plan: &ReconciliationPlan) {
        let mut before = [0_u32; 32];
        for (index, value) in before.iter_mut().enumerate() {
            *value = 0x1000 + index as u32;
        }
        let mut expected = before;
        for copy in copies {
            if copy.destination != copy.source {
                expected[location_index(copy.destination)] = source_value(copy.source, &before, 0);
            }
        }

        let mut actual = before;
        let mut scratch = 0;
        for action in plan.actions() {
            match *action {
                ReconciliationAction::Save(location) => {
                    scratch = actual[location_index(location)];
                }
                ReconciliationAction::Move {
                    source,
                    destination,
                } => {
                    actual[location_index(destination)] = match source {
                        CopySource::Location(location) => source_value(location, &actual, scratch),
                        CopySource::Scratch => scratch,
                    };
                }
            }
        }

        for copy in copies {
            assert_eq!(
                actual[location_index(copy.destination)],
                expected[location_index(copy.destination)],
                "wrong result for {copy:?} with {plan:?}"
            );
        }
    }

    fn source_value(location: HostLocation, values: &[u32; 32], _scratch: u32) -> u32 {
        match location {
            HostLocation::Constant(value) => value,
            HostLocation::Register(_) | HostLocation::Spill(_) => values[location_index(location)],
            HostLocation::Empty => panic!("empty copy source"),
        }
    }

    fn location_index(location: HostLocation) -> usize {
        match location {
            HostLocation::Register(register) => register as usize,
            HostLocation::Spill(slot) => 16 + usize::from(slot),
            HostLocation::Constant(_) | HostLocation::Empty => panic!("not a storage location"),
        }
    }

    const fn pair(destination: HostLocation, source: HostLocation) -> CopyPair {
        CopyPair {
            source,
            destination,
        }
    }

    const fn reg(register: Gpr) -> HostLocation {
        HostLocation::Register(register)
    }

    const fn spill(slot: u8) -> HostLocation {
        HostLocation::Spill(slot)
    }
}
