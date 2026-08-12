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
    reason = "the direct DBT dispatcher consumes executable scratch memory in a later issue #17 task"
)]

use super::{DbtFault, DbtFaultKind};
#[cfg(not(target_os = "linux"))]
compile_error!("the x86-64 DBT executable arena currently requires Linux memfd aliases");

use memmap2::{Mmap, MmapMut, MmapOptions};
use std::fs::File;
use std::os::fd::FromRawFd;
use std::sync::atomic::{compiler_fence, Ordering};

const PAGE_BYTES: usize = 4096;

#[derive(Debug)]
pub(super) struct ExecutableMapping {
    capacity: usize,
    writable: MmapMut,
    executable: Mmap,
    published: bool,
}

impl ExecutableMapping {
    pub(super) fn new(capacity: usize) -> Result<Self, DbtFault> {
        if capacity == 0 || !capacity.is_multiple_of(PAGE_BYTES) {
            return Err(Self::fault(
                DbtFaultKind::Capacity,
                "executable mapping capacity must be a positive multiple of 4096 bytes",
            ));
        }
        let descriptor =
            unsafe { libc::memfd_create(c"compukter-dbt".as_ptr(), libc::MFD_CLOEXEC) };
        if descriptor < 0 {
            return Err(Self::fault(
                DbtFaultKind::ExecutableMemory,
                format!(
                    "failed to create DBT memfd: {}",
                    std::io::Error::last_os_error()
                ),
            ));
        }
        let backing = unsafe { File::from_raw_fd(descriptor) };
        backing.set_len(capacity as u64).map_err(|error| {
            Self::fault(
                DbtFaultKind::ExecutableMemory,
                format!("failed to size DBT memfd to {capacity} bytes: {error}"),
            )
        })?;
        let mut options = MmapOptions::new();
        let writable = unsafe { options.len(capacity).map_mut(&backing) }.map_err(|error| {
            Self::fault(
                DbtFaultKind::ExecutableMemory,
                format!("failed to map DBT memfd as RW: {error}"),
            )
        })?;
        let executable = unsafe { options.len(capacity).map_exec(&backing) }.map_err(|error| {
            Self::fault(
                DbtFaultKind::ExecutableMemory,
                format!("failed to map DBT memfd as RX: {error}"),
            )
        })?;
        Ok(Self {
            capacity,
            writable,
            executable,
            published: false,
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

        self.writable[offset..end].copy_from_slice(code);
        // Linux MAP_SHARED aliases expose the same backing pages. x86-64 keeps instruction and
        // data caches coherent; publication only needs to keep the completed copy ordered before
        // the caller exposes the RX entry address.
        compiler_fence(Ordering::Release);
        self.published = true;
        Ok(())
    }

    pub(super) fn entry_address(&self, offset: usize) -> Option<*const u8> {
        if offset >= self.capacity {
            return None;
        }
        self.published
            .then(|| unsafe { self.executable.as_ptr().add(offset) })
    }

    pub(super) const fn capacity(&self) -> usize {
        self.capacity
    }

    pub(super) const fn reserved_bytes(&self) -> usize {
        self.capacity.saturating_mul(2)
    }

    #[cfg(test)]
    fn writable_address(&self) -> *const u8 {
        self.writable.as_ptr()
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
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    use std::fs;

    const PAGE_BYTES: usize = 4096;

    #[cfg(target_arch = "x86_64")]
    unsafe fn execute(entry: *const u8) -> u32 {
        let entry: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(entry) };
        unsafe { entry() }
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    fn mapping_permissions(address: *const u8) -> String {
        let address = address as usize;
        fs::read_to_string("/proc/self/maps")
            .unwrap()
            .lines()
            .find_map(|line| {
                let mut fields = line.split_whitespace();
                let range = fields.next()?;
                let permissions = fields.next()?;
                let (start, end) = range.split_once('-')?;
                let start = usize::from_str_radix(start, 16).ok()?;
                let end = usize::from_str_radix(end, 16).ok()?;
                (start <= address && address < end).then(|| permissions.to_string())
            })
            .expect("alias address must appear in /proc/self/maps")
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn uses_distinct_non_rwx_aliases() {
        let mut mapping = ExecutableMapping::new(PAGE_BYTES).unwrap();
        mapping.publish_at(0, &[0xb8, 7, 0, 0, 0, 0xc3]).unwrap();

        let writable = mapping.writable_address();
        let executable = mapping.entry_address(0).unwrap();
        assert_ne!(writable, executable);
        assert_eq!(mapping_permissions(writable), "rw-s");
        assert_eq!(mapping_permissions(executable), "r-xs");
        assert_eq!(unsafe { execute(executable) }, 7);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn republishes_one_fixed_mapping_with_new_code() {
        let mut scratch = ExecutableScratch::new(PAGE_BYTES).unwrap();
        scratch.publish(&[0xb8, 7, 0, 0, 0, 0xc3]).unwrap();

        assert_eq!(scratch.capacity(), PAGE_BYTES);
        assert_eq!(scratch.reserved_bytes(), PAGE_BYTES * 2);
        assert_eq!(scratch.emitted_bytes(), 6);
        let first_entry = scratch.entry_address().unwrap();
        assert_eq!(unsafe { execute(first_entry) }, 7);

        // Republishing the same virtual address proves x86_64 instruction visibility without a
        // file-durability flush; this is the contract future host backends must preserve.
        scratch.publish(&[0xb8, 9, 0, 0, 0, 0xc3]).unwrap();

        assert_eq!(scratch.reserved_bytes(), PAGE_BYTES * 2);
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
