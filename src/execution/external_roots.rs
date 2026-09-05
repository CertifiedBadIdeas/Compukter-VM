/*
 * The Compukters Developers
 *
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use super::{error::AdmissionError, value::Ref32};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExternalHandle {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug)]
struct ExternalRootEntry {
    generation: u32,
    value: Option<Ref32>,
}

pub(super) struct ExternalRootTable {
    entries: Box<[ExternalRootEntry]>,
}

impl ExternalRootTable {
    pub(super) fn new(capacity: u32) -> Result<Self, AdmissionError> {
        let capacity =
            usize::try_from(capacity).map_err(|_| AdmissionError::StoragePlanOverflow)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        entries.resize(
            capacity,
            ExternalRootEntry {
                generation: 1,
                value: None,
            },
        );
        Ok(Self {
            entries: entries.into_boxed_slice(),
        })
    }

    pub(super) fn retain(&mut self, value: Ref32) -> Option<ExternalHandle> {
        let (slot, entry) = self
            .entries
            .iter_mut()
            .enumerate()
            .find(|(_, entry)| entry.value.is_none() && entry.generation != 0)?;
        entry.value = Some(value);
        Some(ExternalHandle {
            slot: u32::try_from(slot).ok()?,
            generation: entry.generation,
        })
    }

    pub(super) fn get(&self, handle: ExternalHandle) -> Option<Ref32> {
        self.entries
            .get(handle.slot as usize)
            .filter(|entry| entry.generation == handle.generation)
            .and_then(|entry| entry.value)
    }

    pub(super) fn release(&mut self, handle: ExternalHandle) -> Option<Ref32> {
        let entry = self.entries.get_mut(handle.slot as usize)?;
        if entry.generation != handle.generation {
            return None;
        }
        let value = entry.value.take()?;
        entry.generation = entry.generation.checked_add(1).unwrap_or(0);
        Some(value)
    }

    pub(super) fn root(&self, index: usize) -> Option<Ref32> {
        self.entries.get(index).and_then(|entry| entry.value)
    }

    pub(super) const fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(super) fn reserved_bytes(&self) -> usize {
        self.entries.len() * core::mem::size_of::<ExternalRootEntry>()
    }
}

#[cfg(test)]
mod tests {
    use super::{ExternalRootTable, Ref32};

    #[test]
    fn release_invalidates_external_generation_before_slot_reuse() {
        let value = Ref32::managed(16).unwrap();
        let mut roots = ExternalRootTable::new(1).unwrap();
        let first = roots.retain(value).unwrap();
        assert_eq!(Some(value), roots.get(first));
        assert_eq!(Some(value), roots.release(first));
        assert_eq!(None, roots.get(first));

        let next = roots.retain(value).unwrap();
        assert_eq!(first.slot, next.slot);
        assert_eq!(first.generation + 1, next.generation);
    }
}
