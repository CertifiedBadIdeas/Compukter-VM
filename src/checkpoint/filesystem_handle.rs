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

use super::*;
use crate::execution::checkpoint::{checkpoint_enum, checkpoint_struct, CheckpointError, Result};
checkpoint_struct!(FileHandle { slot, generation });
checkpoint_struct!(OpenFile { path, mode });
checkpoint_enum!(OpenMode { 0 => Read; 1 => Write; 2 => ReadWrite; });
checkpoint_struct!(HandleSlot {
    generation,
    file,
    retired
});
checkpoint_struct!(HandleTable {
    maximum_handles,
    slots
});
impl HandleTable {
    pub(crate) fn validate_checkpoint(&self, limits: &crate::FileSystemLimits) -> Result<()> {
        if self.maximum_handles != limits.maximum_open_handles as usize
            || self.slots.len() > self.maximum_handles
            || self.slots.iter().any(|slot| {
                slot.generation == 0
                    || slot.retired && (slot.generation != u32::MAX || slot.file.is_some())
                    || slot.file.as_ref().is_some_and(|file| {
                        let path = file.path.to_string();
                        VirtualPath::parse_utf8(&path, limits).is_err()
                            || !matches!(file.path.components().next(), Some("home" | "rom"))
                            || file.mode.writable() && file.path.components().next() == Some("rom")
                    })
            })
        {
            return Err(CheckpointError::InvalidState);
        }
        Ok(())
    }
}
