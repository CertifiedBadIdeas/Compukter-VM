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
    reason = "the direct DBT dispatcher consumes executable scratch memory in a later issue #498 task"
)]

use super::{DbtFault, DbtFaultKind};
use memmap2::{Mmap, MmapMut};

const PAGE_BYTES: usize = 4096;

#[derive(Debug)]
enum MappingState {
    Writable(MmapMut),
    Executable(Mmap),
}

#[derive(Debug)]
pub(super) struct ExecutableMapping {
    capacity: usize,
    state: Option<MappingState>,
}

impl ExecutableMapping {
    pub(super) fn new(capacity: usize) -> Result<Self, DbtFault> {
        if capacity == 0 || !capacity.is_multiple_of(PAGE_BYTES) {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                "executable mapping capacity must be a positive multiple of 4096 bytes",
            ));
        }
        let mapping = MmapMut::map_anon(capacity).map_err(|error| {
            Self::fault(
                DbtFaultKind::ExecutableMemory,
                format!("failed to reserve executable mapping: {error}"),
            )
        })?;
        Ok(Self {
            capacity,
            state: Some(MappingState::Writable(mapping)),
        })
    }

    pub(super) fn publish_at(&mut self, offset: usize, code: &[u8]) -> Result<(), DbtFault> {
        if code.is_empty() {
            return Err(Self::fault(
                DbtFaultKind::Translation,
                "cannot publish an empty native code block",
            ));
        }
        let end = offset.checked_add(code.len()).ok_or_else(|| {
            Self::fault(
                DbtFaultKind::Capacity,
                "native code publication range overflows host address space",
            )
        })?;
        if end > self.capacity {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                format!(
                    "native code range {offset}..{end} exceeds mapping capacity {} bytes",
                    self.capacity
                ),
            ));
        }

        let state = self.state.take().ok_or_else(|| {
            Self::fault(
                DbtFaultKind::ExecutableMemory,
                "executable mapping is unavailable after a failed permission transition",
            )
        })?;
        let mut writable = match state {
            MappingState::Writable(mapping) => mapping,
            MappingState::Executable(mapping) => mapping.make_mut().map_err(|error| {
                Self::fault(
                    DbtFaultKind::ExecutableMemory,
                    format!("failed to transition executable mapping from RX to RW: {error}"),
                )
            })?,
        };
        writable[offset..end].copy_from_slice(code);
        // x86_64 has coherent instruction and data caches. A future backend for a host without
        // that guarantee must perform explicit instruction-cache maintenance before make_exec.
        let executable = writable.make_exec().map_err(|error| {
            Self::fault(
                DbtFaultKind::ExecutableMemory,
                format!("failed to transition executable mapping from RW to RX: {error}"),
            )
        })?;
        self.state = Some(MappingState::Executable(executable));
        Ok(())
    }

    pub(super) fn entry_address(&self, offset: usize) -> Option<*const u8> {
        if offset >= self.capacity {
            return None;
        }
        match self.state.as_ref()? {
            MappingState::Executable(mapping) => Some(unsafe { mapping.as_ptr().add(offset) }),
            MappingState::Writable(_) => None,
        }
    }

    pub(super) const fn capacity(&self) -> usize {
        self.capacity
    }

    pub(super) const fn reserved_bytes(&self) -> usize {
        self.capacity
    }

    fn fault(kind: DbtFaultKind, message: impl Into<String>) -> DbtFault {
        DbtFault::new(kind, 0, None, message)
    }
}

#[derive(Debug)]
pub(crate) struct ExecutableScratch {
    mapping: ExecutableMapping,
    emitted: usize,
}

impl ExecutableScratch {
    pub(crate) fn new(capacity: usize) -> Result<Self, DbtFault> {
        Ok(Self {
            mapping: ExecutableMapping::new(capacity)?,
            emitted: 0,
        })
    }

    pub(crate) fn publish(&mut self, code: &[u8]) -> Result<(), DbtFault> {
        self.mapping.publish_at(0, code)?;
        self.emitted = code.len();
        Ok(())
    }

    pub(crate) fn entry_address(&self) -> Option<*const u8> {
        self.mapping.entry_address(0)
    }

    pub(crate) const fn capacity(&self) -> usize {
        self.mapping.capacity()
    }

    pub(crate) const fn reserved_bytes(&self) -> usize {
        self.mapping.reserved_bytes()
    }

    pub(crate) const fn emitted_bytes(&self) -> usize {
        self.emitted
    }
}

#[cfg(test)]
mod tests {
    use super::{ExecutableMapping, ExecutableScratch};
    use crate::rv32_dbt::DbtFaultKind;

    const PAGE_BYTES: usize = 4096;

    #[cfg(target_arch = "x86_64")]
    unsafe fn execute(entry: *const u8) -> u32 {
        let entry: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(entry) };
        unsafe { entry() }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn republishes_one_fixed_mapping_with_new_code() {
        let mut scratch = ExecutableScratch::new(PAGE_BYTES).unwrap();
        scratch.publish(&[0xb8, 7, 0, 0, 0, 0xc3]).unwrap();

        assert_eq!(scratch.capacity(), PAGE_BYTES);
        assert_eq!(scratch.reserved_bytes(), PAGE_BYTES);
        assert_eq!(scratch.emitted_bytes(), 6);
        let first_entry = scratch.entry_address().unwrap();
        assert_eq!(unsafe { execute(first_entry) }, 7);

        // Republishing the same virtual address proves x86_64 instruction visibility without a
        // file-durability flush; this is the contract future host backends must preserve.
        scratch.publish(&[0xb8, 9, 0, 0, 0, 0xc3]).unwrap();

        assert_eq!(scratch.reserved_bytes(), PAGE_BYTES);
        assert_eq!(scratch.emitted_bytes(), 6);
        let second_entry = scratch.entry_address().unwrap();
        assert_eq!(second_entry, first_entry);
        assert_eq!(unsafe { execute(second_entry) }, 9);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn publishes_and_executes_independent_offsets() {
        let mut mapping = ExecutableMapping::new(PAGE_BYTES).unwrap();
        mapping.publish_at(0, &[0xb8, 7, 0, 0, 0, 0xc3]).unwrap();
        mapping.publish_at(16, &[0xb8, 9, 0, 0, 0, 0xc3]).unwrap();

        assert_eq!(unsafe { execute(mapping.entry_address(0).unwrap()) }, 7);
        assert_eq!(unsafe { execute(mapping.entry_address(16).unwrap()) }, 9);
        assert!(mapping.publish_at(PAGE_BYTES - 2, &[0x90; 4]).is_err());
    }

    #[test]
    fn rejects_invalid_capacity_and_oversized_code() {
        for capacity in [0, 1, PAGE_BYTES - 1, PAGE_BYTES + 1] {
            assert_eq!(
                ExecutableScratch::new(capacity).unwrap_err().kind(),
                DbtFaultKind::Capacity
            );
        }

        let mut scratch = ExecutableScratch::new(PAGE_BYTES).unwrap();
        assert_eq!(
            scratch.publish(&[]).unwrap_err().kind(),
            DbtFaultKind::Translation
        );
        assert_eq!(
            scratch
                .publish(&vec![0xcc; PAGE_BYTES + 1])
                .unwrap_err()
                .kind(),
            DbtFaultKind::Capacity
        );
        assert_eq!(scratch.emitted_bytes(), 0);
        assert!(scratch.entry_address().is_none());
    }
}
