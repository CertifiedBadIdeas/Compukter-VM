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
use crate::execution::checkpoint::{
    checkpoint_struct, Checkpoint, CheckpointError, Reader, Result, Writer,
};

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

/// Validate the allocator independently of object layouts. Object references and
/// partial collector cursors are checked by the image-aware machine validator.
impl Heap {
    pub(in crate::execution) fn checkpoint_reservation_capacity(
        &self,
        reservation: ReservedAllocation,
    ) -> Result<u32> {
        let mut offset = 0_u32;
        while offset < self.arena_bytes {
            let block = BlockOffset(offset);
            let size = self
                .block_size(block)
                .map_err(|_| CheckpointError::InvalidState)?;
            if block == reservation.block {
                self.validate_reservation(reservation)
                    .map_err(|_| CheckpointError::InvalidState)?;
                return size
                    .checked_sub(self.payload_offset())
                    .ok_or(CheckpointError::InvalidState);
            }
            offset = offset
                .checked_add(size)
                .ok_or(CheckpointError::InvalidState)?;
        }
        Err(CheckpointError::InvalidState)
    }

    pub(crate) fn validate_checkpoint(&self, plan: &StoragePlan) -> Result<()> {
        let invalid = || CheckpointError::InvalidState;
        if self.header_format != plan.header_format
            || u64::from(self.arena_bytes) != plan.heap_arena_bytes
            || self.arena.len().checked_mul(16) != Some(self.arena_bytes as usize)
            || self.class_heads.len() != CLASS_COUNT
        {
            return Err(invalid());
        }
        let mut blocks = Vec::new();
        let mut offset = 0_u32;
        let mut total_free = 0_u32;
        let mut live_objects = 0_u32;
        while offset < self.arena_bytes {
            let block = BlockOffset(offset);
            let size = self.block_size(block).map_err(|_| invalid())?;
            let flags = self.read_flags(block).map_err(|_| invalid())?;
            if flags & ALLOCATED == 0 && flags != 0 || flags & MARKED != 0 && flags & LIVE == 0 {
                return Err(invalid());
            }
            blocks
                .try_reserve(1)
                .map_err(|_| CheckpointError::Allocation)?;
            blocks.push((offset, size, flags));
            offset = offset
                .checked_add(size)
                .filter(|end| *end <= self.arena_bytes)
                .ok_or_else(invalid)?;
            if flags == 0 {
                total_free = total_free.checked_add(size).ok_or_else(invalid)?;
            } else if flags & LIVE != 0 {
                live_objects = live_objects.checked_add(1).ok_or_else(invalid)?;
            }
        }
        if offset != self.arena_bytes
            || total_free != self.total_free
            || live_objects != self.live_objects
        {
            return Err(invalid());
        }
        let mut visited = Vec::new();
        visited
            .try_reserve_exact(blocks.len())
            .map_err(|_| CheckpointError::Allocation)?;
        visited.resize(blocks.len(), false);
        let mut first_bitmap = 0_u32;
        let mut second_bitmaps = [0_u8; 32];
        for (class, head) in self.class_heads.iter().copied().enumerate() {
            if head != NULL_OFFSET {
                second_bitmaps[class / 8] |= 1 << (class % 8);
                first_bitmap |= 1 << (class / 8);
            }
            let mut cursor = head;
            let mut previous = NULL_OFFSET;
            while cursor != NULL_OFFSET {
                let index = blocks
                    .binary_search_by_key(&cursor, |block| block.0)
                    .map_err(|_| invalid())?;
                let (_, size, flags) = blocks[index];
                if visited[index]
                    || flags != 0
                    || free_size_class(size).map(class_index) != Some(class)
                    || self
                        .read_word(BlockOffset(cursor), PREVIOUS_FREE)
                        .map_err(|_| invalid())?
                        != previous
                {
                    return Err(invalid());
                }
                visited[index] = true;
                previous = cursor;
                cursor = self
                    .read_word(BlockOffset(cursor), NEXT_FREE)
                    .map_err(|_| invalid())?;
            }
        }
        if first_bitmap != self.first_bitmap
            || second_bitmaps != self.second_bitmaps
            || blocks
                .iter()
                .zip(&visited)
                .any(|(block, visited)| (block.2 == 0) != *visited)
            || self.largest_free_hint != NULL_OFFSET
                && !blocks
                    .binary_search_by_key(&self.largest_free_hint, |block| block.0)
                    .is_ok_and(|index| blocks[index].2 == 0)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl Heap {
    pub(in crate::execution) fn checkpoint_live_references(&self) -> Result<Vec<Ref32>> {
        let mut references = Vec::new();
        references
            .try_reserve_exact(self.live_objects as usize)
            .map_err(|_| CheckpointError::Allocation)?;
        let mut offset = 0_u32;
        while offset < self.arena_bytes {
            let block = BlockOffset(offset);
            let size = self
                .block_size(block)
                .map_err(|_| CheckpointError::InvalidState)?;
            if self
                .read_flags(block)
                .map_err(|_| CheckpointError::InvalidState)?
                & LIVE
                != 0
            {
                references.push(
                    Ref32::managed(
                        offset
                            .checked_add(OBJECT_TYPE_ID)
                            .ok_or(CheckpointError::InvalidState)?,
                    )
                    .ok_or(CheckpointError::InvalidState)?,
                );
            }
            offset = offset
                .checked_add(size)
                .filter(|end| *end <= self.arena_bytes)
                .ok_or(CheckpointError::InvalidState)?;
        }
        Ok(references)
    }

    pub(in crate::execution) fn checkpoint_gray_next(
        &self,
        reference: Ref32,
    ) -> Result<Option<u32>> {
        let block = self
            .live_block(reference)
            .map_err(|_| CheckpointError::InvalidState)?;
        if self
            .read_flags(block)
            .map_err(|_| CheckpointError::InvalidState)?
            & MARKED
            == 0
        {
            return Err(CheckpointError::InvalidState);
        }
        let next = self
            .read_previous_or_gray(block)
            .map_err(|_| CheckpointError::InvalidState)?;
        Ok((next != NULL_OFFSET && next != 0).then_some(next))
    }

    pub(in crate::execution) fn checkpoint_sweep_boundary(
        &self,
        cursor: u32,
        previous: u32,
    ) -> Result<()> {
        let mut offset = 0_u32;
        let mut previous_size = 0_u32;
        while offset < cursor {
            previous_size = self
                .block_size(BlockOffset(offset))
                .map_err(|_| CheckpointError::InvalidState)?;
            offset = offset
                .checked_add(previous_size)
                .filter(|end| *end <= self.arena_bytes)
                .ok_or(CheckpointError::InvalidState)?;
        }
        if offset != cursor || previous != previous_size {
            return Err(CheckpointError::InvalidState);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(heap: &Heap, plan: &StoragePlan) -> Heap {
        let mut writer = Writer::new(16384);
        heap.write(&mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 16384, 16384).unwrap();
        let restored = Heap::read(&mut reader).unwrap();
        reader.finish().unwrap();
        restored.validate_checkpoint(plan).unwrap();
        restored
    }

    #[test]
    fn checkpoint_retains_heap_identity_and_allocation_order() {
        for format in [HeaderFormat::Legacy, HeaderFormat::select(4096, 4)] {
            let plan = StoragePlan::heap_only(4096).with_header_format(format);
            let mut original = Heap::new(&plan).unwrap();
            let request = AllocationRequest {
                block_bytes: 32,
                type_id: 2,
            };
            let first = original.reserve(request).unwrap().unwrap();
            let reference = original.commit(first).unwrap();
            let hole = original.reserve(request).unwrap().unwrap();
            let hole = original.commit(hole).unwrap();
            let reserved = original.reserve(request).unwrap().unwrap();
            original.free(hole).unwrap();
            let mut restored = round_trip(&original, &plan);
            assert_eq!(2, restored.managed_type(reference).unwrap());
            for heap in [&mut original, &mut restored] {
                heap.abort(reserved).unwrap();
            }
            assert_eq!(
                original.reserve(request).unwrap(),
                restored.reserve(request).unwrap()
            );
            restored.validate_checkpoint(&plan).unwrap();
        }
    }

    #[test]
    fn checkpoint_retains_intrusive_gc_queue() {
        for format in [HeaderFormat::Legacy, HeaderFormat::select(4096, 4)] {
            let plan = StoragePlan::heap_only(4096).with_header_format(format);
            let mut heap = Heap::new(&plan).unwrap();
            let mut head = None;
            let mut tail = None;
            for type_id in 0..3 {
                let reserved = heap
                    .reserve(AllocationRequest {
                        block_bytes: 32,
                        type_id,
                    })
                    .unwrap()
                    .unwrap();
                let reference = heap.commit(reserved).unwrap();
                heap.enqueue_gray(reference, 1, &mut head, &mut tail)
                    .unwrap();
            }
            let mut restored = round_trip(&heap, &plan);
            while head.is_some() {
                let (mut restored_head, mut restored_tail) = (head, tail);
                assert_eq!(
                    heap.dequeue_gray(&mut head, &mut tail).unwrap(),
                    restored
                        .dequeue_gray(&mut restored_head, &mut restored_tail)
                        .unwrap()
                );
                assert_eq!((head, tail), (restored_head, restored_tail));
            }
        }
    }

    #[test]
    fn checkpoint_rejects_allocator_cycles_and_wrong_storage_contract() {
        let plan = StoragePlan::heap_only(4096);
        let mut heap = Heap::new(&plan).unwrap();
        heap.write_word(BlockOffset(0), NEXT_FREE, 0).unwrap();
        assert_eq!(
            Err(CheckpointError::InvalidState),
            heap.validate_checkpoint(&plan)
        );
        heap.write_word(BlockOffset(0), NEXT_FREE, NULL_OFFSET)
            .unwrap();
        heap.first_bitmap ^= 1;
        assert_eq!(
            Err(CheckpointError::InvalidState),
            heap.validate_checkpoint(&plan)
        );
        heap.first_bitmap ^= 1;
        assert_eq!(
            Err(CheckpointError::InvalidState),
            heap.validate_checkpoint(&StoragePlan::heap_only(8192))
        );
    }
}
