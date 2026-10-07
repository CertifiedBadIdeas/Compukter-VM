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
use crate::execution::checkpoint::{checkpoint_struct, Checkpoint, Reader, Result, Writer};

impl Checkpoint for ArenaUnit {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.0.write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Ok(Self(Checkpoint::read(reader)?))
    }
}

impl Checkpoint for BlockOffset {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.0.write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Ok(Self(Checkpoint::read(reader)?))
    }
}

checkpoint_struct!(AllocationRequest {
    block_bytes,
    type_id
});

checkpoint_struct!(ReservedAllocation { block, type_id });

checkpoint_struct!(Heap {
    header_format,
    arena,
    arena_bytes,
    class_heads,
    first_bitmap,
    second_bitmaps,
    largest_free_hint,
    total_free,
    live_objects
});
