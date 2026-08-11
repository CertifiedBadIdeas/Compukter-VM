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
pub(crate) struct ExecutableScratch {
    capacity: usize,
    emitted: usize,
    state: Option<MappingState>,
}

impl ExecutableScratch {
    pub(crate) fn new(capacity: usize) -> Result<Self, DbtFault> {
        if capacity == 0 || !capacity.is_multiple_of(PAGE_BYTES) {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                "executable scratch capacity must be a positive multiple of 4096 bytes",
            ));
        }
        let mapping = MmapMut::map_anon(capacity).map_err(|error| {
            Self::fault(
                DbtFaultKind::ExecutableMemory,
                format!("failed to reserve executable scratch mapping: {error}"),
            )
        })?;
        Ok(Self {
            capacity,
            emitted: 0,
            state: Some(MappingState::Writable(mapping)),
        })
    }

    pub(crate) fn publish(&mut self, code: &[u8]) -> Result<(), DbtFault> {
        if code.is_empty() {
            return Err(Self::fault(
                DbtFaultKind::Translation,
                "cannot publish an empty native code block",
            ));
        }
        if code.len() > self.capacity {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                format!(
                    "native block requires {} bytes but scratch capacity is {} bytes",
                    code.len(),
                    self.capacity
                ),
            ));
        }

        let state = self.state.take().ok_or_else(|| {
            Self::fault(
                DbtFaultKind::ExecutableMemory,
                "executable scratch mapping is unavailable after a failed permission transition",
            )
        })?;
        let mut writable = match state {
            MappingState::Writable(mapping) => mapping,
            MappingState::Executable(mapping) => mapping.make_mut().map_err(|error| {
                Self::fault(
                    DbtFaultKind::ExecutableMemory,
                    format!("failed to transition executable scratch from RX to RW: {error}"),
                )
            })?,
        };
        writable[..code.len()].copy_from_slice(code);
        writable.flush().map_err(|error| {
            Self::fault(
                DbtFaultKind::ExecutableMemory,
                format!("failed to flush executable scratch code: {error}"),
            )
        })?;
        let executable = writable.make_exec().map_err(|error| {
            Self::fault(
                DbtFaultKind::ExecutableMemory,
                format!("failed to transition executable scratch from RW to RX: {error}"),
            )
        })?;
        self.emitted = code.len();
        self.state = Some(MappingState::Executable(executable));
        Ok(())
    }

    pub(crate) fn entry_address(&self) -> Option<*const u8> {
        match self.state.as_ref()? {
            MappingState::Executable(mapping) => Some(mapping.as_ptr()),
            MappingState::Writable(_) => None,
        }
    }

    pub(crate) const fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) const fn reserved_bytes(&self) -> usize {
        self.capacity
    }

    pub(crate) const fn emitted_bytes(&self) -> usize {
        self.emitted
    }

    fn fault(kind: DbtFaultKind, message: impl Into<String>) -> DbtFault {
        DbtFault::new(kind, 0, None, message)
    }
}

#[cfg(test)]
mod tests {
    use super::ExecutableScratch;
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

        scratch.publish(&[0xb8, 9, 0, 0, 0, 0xc3]).unwrap();

        assert_eq!(scratch.reserved_bytes(), PAGE_BYTES);
        assert_eq!(scratch.emitted_bytes(), 6);
        let second_entry = scratch.entry_address().unwrap();
        assert_eq!(second_entry, first_entry);
        assert_eq!(unsafe { execute(second_entry) }, 9);
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
