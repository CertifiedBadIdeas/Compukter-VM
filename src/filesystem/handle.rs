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

use super::{FileSystemError, VirtualPath};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileHandle {
    slot: u32,
    generation: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenFile {
    path: VirtualPath,
    mode: OpenMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenMode {
    Read,
    Write,
    ReadWrite,
}

impl OpenMode {
    pub(crate) fn readable(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    pub(crate) fn writable(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

impl OpenFile {
    pub(crate) fn new(path: VirtualPath, mode: OpenMode) -> Self {
        Self { path, mode }
    }

    pub(crate) fn path(&self) -> &VirtualPath {
        &self.path
    }

    pub(crate) fn mode(&self) -> OpenMode {
        self.mode
    }

    #[doc(hidden)]
    pub fn testing() -> Self {
        Self {
            path: VirtualPath::root(),
            mode: OpenMode::ReadWrite,
        }
    }
}

#[derive(Debug)]
struct HandleSlot {
    generation: u32,
    file: Option<OpenFile>,
    retired: bool,
}

/// A bounded slot table which never aliases a stale handle after reuse.
#[derive(Debug)]
pub struct HandleTable {
    maximum_handles: usize,
    slots: Vec<HandleSlot>,
}

impl HandleTable {
    pub fn new(maximum_handles: usize) -> Self {
        Self {
            maximum_handles,
            slots: Vec::new(),
        }
    }

    pub fn open(&mut self, file: OpenFile) -> Result<FileHandle, FileSystemError> {
        if let Some((slot, entry)) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, entry)| entry.file.is_none() && !entry.retired)
        {
            entry.file = Some(file);
            return Ok(FileHandle {
                slot: slot as u32,
                generation: entry.generation,
            });
        }
        if self.slots.len() >= self.maximum_handles || self.slots.len() > u32::MAX as usize {
            return Err(FileSystemError::QuotaExceeded);
        }
        let slot = self.slots.len();
        self.slots.push(HandleSlot {
            generation: 1,
            file: Some(file),
            retired: false,
        });
        Ok(FileHandle {
            slot: slot as u32,
            generation: 1,
        })
    }

    pub fn get(&self, handle: FileHandle) -> Result<&OpenFile, FileSystemError> {
        let slot = self
            .slots
            .get(handle.slot as usize)
            .ok_or(FileSystemError::StaleHandle)?;
        if slot.generation != handle.generation {
            return Err(FileSystemError::StaleHandle);
        }
        slot.file.as_ref().ok_or(FileSystemError::StaleHandle)
    }

    pub fn close(&mut self, handle: FileHandle) -> Result<(), FileSystemError> {
        let slot = self
            .slots
            .get_mut(handle.slot as usize)
            .ok_or(FileSystemError::StaleHandle)?;
        if slot.generation != handle.generation || slot.file.is_none() {
            return Err(FileSystemError::StaleHandle);
        }
        slot.file = None;
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
        } else {
            slot.retired = true;
        }
        Ok(())
    }

    pub(crate) fn open_count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.file.is_some()).count()
    }
}
