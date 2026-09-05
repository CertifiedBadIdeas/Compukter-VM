use super::{
    error::{AdmissionError, ResidentStorageComponent, VmFault},
    layout::StoragePlan,
    value::{Ref32, ReferenceDomain},
};

const NULL_OFFSET: u32 = u32::MAX;
const CLASS_COUNT: usize = 32 * 8;
const ALLOCATED: u32 = 1;
const MARKED: u32 = 2;
const LIVE: u32 = 4;
const SIZE_MASK: u32 = !15;
const SIZE_FLAGS: u32 = 0;
const PREVIOUS_SIZE: u32 = 4;
const NEXT_FREE: u32 = 8;
const PREVIOUS_FREE: u32 = 12;
const OBJECT_TYPE_ID: u32 = 16;
const OBJECT_IDENTITY_TOKEN: u32 = 20;
const USER_PAYLOAD: u32 = 24;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct ArenaUnit([u8; 16]);

impl ArenaUnit {
    const ZERO: Self = Self([0; 16]);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BlockOffset(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SizeClass {
    pub first: u8,
    pub second: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AllocationRequest {
    pub block_bytes: u32,
    pub type_id: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReservedAllocation {
    pub block: BlockOffset,
    pub type_id: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub(super) struct ManagedObjectHeader {
    pub type_id: u32,
    pub identity_token: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct HeapDiagnostic {
    pub total_free: u32,
    pub largest_free_block: u32,
    pub live_handles: u32,
    pub retired_handles: u32,
}

pub(super) struct Heap {
    arena: Box<[ArenaUnit]>,
    arena_bytes: u32,
    class_heads: Box<[u32]>,
    first_bitmap: u32,
    second_bitmaps: [u8; 32],
    next_ordinal: u64,
    total_free: u32,
    live_objects: u32,
}

impl Heap {
    pub(super) const fn allocator_resident_bytes() -> u64 {
        (CLASS_COUNT * core::mem::size_of::<u32>()) as u64
    }

    pub(super) fn new(plan: &StoragePlan) -> Result<Self, AdmissionError> {
        let heap_bytes = u32::try_from(plan.heap_arena_bytes).map_err(|_| {
            AdmissionError::ResidentStorageOverflow {
                component: ResidentStorageComponent::HeapArena,
            }
        })?;
        if heap_bytes < 32 || !heap_bytes.is_multiple_of(16) || heap_bytes > Ref32::MAX_PAYLOAD {
            return Err(AdmissionError::InvalidHeapSize {
                supplied: heap_bytes,
            });
        }
        let arena_len =
            usize::try_from(heap_bytes / 16).map_err(|_| AdmissionError::StoragePlanOverflow)?;
        let mut arena = Vec::new();
        arena
            .try_reserve_exact(arena_len)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        arena.resize(arena_len, ArenaUnit::ZERO);
        let mut class_heads = Vec::new();
        class_heads
            .try_reserve_exact(CLASS_COUNT)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        class_heads.resize(CLASS_COUNT, NULL_OFFSET);
        let mut heap = Self {
            arena: arena.into_boxed_slice(),
            arena_bytes: heap_bytes,
            class_heads: class_heads.into_boxed_slice(),
            first_bitmap: 0,
            second_bitmaps: [0; 32],
            next_ordinal: 1,
            total_free: heap_bytes,
            live_objects: 0,
        };
        heap.write_header(BlockOffset(0), heap_bytes, 0, false)
            .map_err(|_| AdmissionError::StoragePlanOverflow)?;
        heap.insert_free(BlockOffset(0))
            .map_err(|_| AdmissionError::StoragePlanOverflow)?;
        Ok(heap)
    }

    pub(super) fn reserve(
        &mut self,
        request: AllocationRequest,
    ) -> Result<Option<ReservedAllocation>, VmFault> {
        if request.block_bytes < 32 || !request.block_bytes.is_multiple_of(16) {
            return Err(VmFault::InvalidStoragePlan);
        }
        let Some(block) = self.find_suitable(request.block_bytes)? else {
            return Ok(None);
        };
        let block_size = self.block_size(block)?;
        if block_size < request.block_bytes {
            return Err(VmFault::CorruptHeap);
        }
        let previous_size = self.read_word(block, PREVIOUS_SIZE)?;
        self.remove_free(block)?;
        let remainder = block_size - request.block_bytes;
        let allocated_size = if remainder >= 32 {
            let remainder_offset = BlockOffset(
                block
                    .0
                    .checked_add(request.block_bytes)
                    .ok_or(VmFault::CorruptHeap)?,
            );
            self.write_header(remainder_offset, remainder, request.block_bytes, false)?;
            self.update_next_previous_size(remainder_offset, remainder)?;
            self.insert_free(remainder_offset)?;
            request.block_bytes
        } else {
            block_size
        };
        self.write_header(block, allocated_size, previous_size, true)?;
        self.total_free = self
            .total_free
            .checked_sub(allocated_size)
            .ok_or(VmFault::CorruptHeap)?;
        Ok(Some(ReservedAllocation {
            block,
            type_id: request.type_id,
        }))
    }

    pub(super) fn commit(&mut self, reservation: ReservedAllocation) -> Result<Ref32, VmFault> {
        self.validate_reservation(reservation)?;
        let header = ManagedObjectHeader {
            type_id: reservation.type_id,
            identity_token: splitmix64(self.next_ordinal) as u32,
        };
        self.write_object_header(reservation.block, header)?;
        let flags = self.read_word(reservation.block, SIZE_FLAGS)?;
        self.write_word(reservation.block, SIZE_FLAGS, flags | LIVE)?;
        self.next_ordinal = self.next_ordinal.wrapping_add(1);
        self.live_objects = self
            .live_objects
            .checked_add(1)
            .ok_or(VmFault::CorruptHeap)?;
        let object_header = reservation
            .block
            .0
            .checked_add(16)
            .ok_or(VmFault::CorruptHeap)?;
        Ref32::managed(object_header).ok_or(VmFault::CorruptHeap)
    }

    pub(super) fn abort(&mut self, reservation: ReservedAllocation) -> Result<(), VmFault> {
        self.validate_reservation(reservation)?;
        self.free_block(reservation.block)
    }

    pub(super) fn zero_reserved_payload(
        &mut self,
        reservation: ReservedAllocation,
        offset: u32,
        length: u32,
    ) -> Result<(), VmFault> {
        self.validate_reservation(reservation)?;
        let capacity = self
            .block_size(reservation.block)?
            .checked_sub(USER_PAYLOAD)
            .ok_or(VmFault::CorruptHeap)?;
        let end = offset.checked_add(length).ok_or(VmFault::CorruptHeap)?;
        if end > capacity {
            return Err(VmFault::CorruptHeap);
        }
        let start = reservation
            .block
            .0
            .checked_add(USER_PAYLOAD)
            .and_then(|value| value.checked_add(offset))
            .ok_or(VmFault::CorruptHeap)?;
        for byte in start..start + length {
            let unit = self
                .arena
                .get_mut((byte / 16) as usize)
                .ok_or(VmFault::CorruptHeap)?;
            unit.0[(byte % 16) as usize] = 0;
        }
        Ok(())
    }

    pub(super) fn write_reserved_u32(
        &mut self,
        reservation: ReservedAllocation,
        offset: u32,
        value: u32,
    ) -> Result<(), VmFault> {
        self.validate_reservation(reservation)?;
        let capacity = self
            .block_size(reservation.block)?
            .checked_sub(USER_PAYLOAD)
            .ok_or(VmFault::CorruptHeap)?;
        if offset.checked_add(4).ok_or(VmFault::CorruptHeap)? > capacity {
            return Err(VmFault::CorruptHeap);
        }
        let start = reservation
            .block
            .0
            .checked_add(USER_PAYLOAD)
            .and_then(|base| base.checked_add(offset))
            .ok_or(VmFault::CorruptHeap)?;
        for (index, byte) in value.to_le_bytes().into_iter().enumerate() {
            let position = start
                .checked_add(index as u32)
                .ok_or(VmFault::CorruptHeap)?;
            let unit = self
                .arena
                .get_mut((position / 16) as usize)
                .ok_or(VmFault::CorruptHeap)?;
            unit.0[(position % 16) as usize] = byte;
        }
        Ok(())
    }

    pub(super) fn write_reserved(
        &mut self,
        reservation: ReservedAllocation,
        offset: u32,
        bytes: &[u8],
    ) -> Result<(), VmFault> {
        self.validate_reservation(reservation)?;
        let capacity = self
            .block_size(reservation.block)?
            .checked_sub(USER_PAYLOAD)
            .ok_or(VmFault::CorruptHeap)?;
        let length = u32::try_from(bytes.len()).map_err(|_| VmFault::CorruptHeap)?;
        if offset.checked_add(length).ok_or(VmFault::CorruptHeap)? > capacity {
            return Err(VmFault::CorruptHeap);
        }
        let start = reservation
            .block
            .0
            .checked_add(USER_PAYLOAD)
            .and_then(|base| base.checked_add(offset))
            .ok_or(VmFault::CorruptHeap)?;
        for (index, byte) in bytes.iter().copied().enumerate() {
            let position = start + index as u32;
            let unit = self
                .arena
                .get_mut((position / 16) as usize)
                .ok_or(VmFault::CorruptHeap)?;
            unit.0[(position % 16) as usize] = byte;
        }
        Ok(())
    }

    fn validate_reservation(&self, reservation: ReservedAllocation) -> Result<(), VmFault> {
        if self.block_allocated(reservation.block)?
            && self.read_word(reservation.block, SIZE_FLAGS)? & LIVE == 0
        {
            Ok(())
        } else {
            Err(VmFault::CorruptHeap)
        }
    }

    pub(super) fn free(&mut self, reference: Ref32) -> Result<bool, VmFault> {
        if reference.domain() != ReferenceDomain::Managed {
            return Ok(false);
        }
        let Ok(block) = self.live_block(reference) else {
            return Ok(false);
        };
        self.live_objects = self
            .live_objects
            .checked_sub(1)
            .ok_or(VmFault::CorruptHeap)?;
        self.free_block(block)?;
        Ok(true)
    }

    pub(super) fn runtime_type(&self, reference: Ref32) -> Option<u32> {
        self.managed_type(reference).ok()
    }

    pub(super) fn managed_type(&self, reference: Ref32) -> Result<u32, VmFault> {
        let block = self.live_block(reference)?;
        self.read_word(block, OBJECT_TYPE_ID)
    }

    pub(super) fn read_payload(
        &self,
        reference: Ref32,
        offset: u32,
        length: u32,
    ) -> Result<[u8; 8], VmFault> {
        let block = self.live_block(reference)?;
        let capacity = self
            .block_size(block)?
            .checked_sub(USER_PAYLOAD)
            .ok_or(VmFault::CorruptHeap)?;
        let end = offset.checked_add(length).ok_or(VmFault::CorruptHeap)?;
        if length > 8 || end > capacity {
            return Err(VmFault::CorruptHeap);
        }
        let start = block
            .0
            .checked_add(USER_PAYLOAD)
            .and_then(|base| base.checked_add(offset))
            .ok_or(VmFault::CorruptHeap)?;
        let mut bytes = [0; 8];
        for index in 0..length {
            let position = start + index;
            let unit = self
                .arena
                .get((position / 16) as usize)
                .ok_or(VmFault::CorruptHeap)?;
            bytes[index as usize] = unit.0[(position % 16) as usize];
        }
        Ok(bytes)
    }

    pub(super) fn write_payload(
        &mut self,
        reference: Ref32,
        offset: u32,
        bytes: &[u8],
    ) -> Result<(), VmFault> {
        let block = self.live_block(reference)?;
        let length = u32::try_from(bytes.len()).map_err(|_| VmFault::CorruptHeap)?;
        let capacity = self
            .block_size(block)?
            .checked_sub(USER_PAYLOAD)
            .ok_or(VmFault::CorruptHeap)?;
        let end = offset.checked_add(length).ok_or(VmFault::CorruptHeap)?;
        if end > capacity {
            return Err(VmFault::CorruptHeap);
        }
        let start = block
            .0
            .checked_add(USER_PAYLOAD)
            .and_then(|base| base.checked_add(offset))
            .ok_or(VmFault::CorruptHeap)?;
        for (index, byte) in bytes.iter().copied().enumerate() {
            let position = start + index as u32;
            let unit = self
                .arena
                .get_mut((position / 16) as usize)
                .ok_or(VmFault::CorruptHeap)?;
            unit.0[(position % 16) as usize] = byte;
        }
        Ok(())
    }

    fn live_block(&self, reference: Ref32) -> Result<BlockOffset, VmFault> {
        if reference.domain() != ReferenceDomain::Managed
            || reference.payload() < OBJECT_TYPE_ID
            || !reference.payload().is_multiple_of(16)
        {
            return Err(VmFault::InvalidReference);
        }
        let block = BlockOffset(
            reference
                .payload()
                .checked_sub(16)
                .ok_or(VmFault::InvalidReference)?,
        );
        if block.0 >= self.arena_bytes
            || !self.block_allocated(block)?
            || self.read_word(block, SIZE_FLAGS)? & LIVE == 0
        {
            return Err(VmFault::InvalidReference);
        }
        Ok(block)
    }

    pub(super) fn identity_hash(&self, reference: Ref32) -> Option<u32> {
        self.live_block(reference)
            .ok()
            .and_then(|block| self.read_word(block, OBJECT_IDENTITY_TOKEN).ok())
    }

    pub(super) fn diagnostic(&self) -> HeapDiagnostic {
        let mut largest_free_block = 0;
        let mut offset = BlockOffset(0);
        while offset.0 < self.arena_bytes {
            let Ok(size) = self.block_size(offset) else {
                largest_free_block = 0;
                break;
            };
            if !self.block_allocated(offset).unwrap_or(true) {
                largest_free_block = largest_free_block.max(size);
            }
            let Some(next) = offset.0.checked_add(size) else {
                largest_free_block = 0;
                break;
            };
            offset = BlockOffset(next);
        }
        HeapDiagnostic {
            total_free: self.total_free,
            largest_free_block,
            live_handles: self.live_objects,
            retired_handles: 0,
        }
    }

    pub(super) fn enqueue_gray(
        &mut self,
        reference: Ref32,
        _epoch: u32,
        head: &mut Option<u32>,
        tail: &mut Option<u32>,
    ) -> Result<(), VmFault> {
        if reference.domain() != ReferenceDomain::Managed {
            return Ok(());
        }
        let block = self.live_block(reference)?;
        let flags = self.read_word(block, SIZE_FLAGS)?;
        if flags & MARKED != 0 {
            return Ok(());
        }
        self.write_word(block, SIZE_FLAGS, flags | MARKED)?;
        self.write_word(block, NEXT_FREE, NULL_OFFSET)?;
        if let Some(previous) = *tail {
            let previous = Ref32::managed(previous).ok_or(VmFault::CorruptHeap)?;
            let previous_block = self.live_block(previous)?;
            self.write_word(previous_block, NEXT_FREE, reference.payload())?;
        } else {
            *head = Some(reference.payload());
        }
        *tail = Some(reference.payload());
        Ok(())
    }

    pub(super) fn dequeue_gray(
        &mut self,
        head: &mut Option<u32>,
        tail: &mut Option<u32>,
    ) -> Result<Option<(Ref32, u32)>, VmFault> {
        let Some(payload) = *head else {
            return Ok(None);
        };
        let reference = Ref32::managed(payload).ok_or(VmFault::CorruptHeap)?;
        let block = self.live_block(reference)?;
        let next = self.read_word(block, NEXT_FREE)?;
        self.write_word(block, NEXT_FREE, NULL_OFFSET)?;
        *head = (next != NULL_OFFSET).then_some(next);
        if head.is_none() {
            *tail = None;
        }
        Ok(Some((reference, self.managed_type(reference)?)))
    }

    pub(super) fn arena_bytes(&self) -> u32 {
        self.arena_bytes
    }

    pub(super) fn sweep_block(&mut self, offset: u32, _epoch: u32) -> Result<u32, VmFault> {
        if offset >= self.arena_bytes {
            return Err(VmFault::CorruptHeap);
        }
        let block = BlockOffset(offset);
        let size = self.block_size(block)?;
        if !self.block_allocated(block)? {
            return offset.checked_add(size).ok_or(VmFault::CorruptHeap);
        }
        let flags = self.read_word(block, SIZE_FLAGS)?;
        if flags & LIVE == 0 {
            return Err(VmFault::CorruptHeap);
        }
        if flags & MARKED != 0 {
            self.write_word(block, SIZE_FLAGS, flags & !MARKED)?;
            self.write_word(block, NEXT_FREE, NULL_OFFSET)?;
            return offset.checked_add(size).ok_or(VmFault::CorruptHeap);
        }

        let previous_size = self.read_word(block, PREVIOUS_SIZE)?;
        let merged = if offset != 0 {
            let previous = BlockOffset(
                offset
                    .checked_sub(previous_size)
                    .ok_or(VmFault::CorruptHeap)?,
            );
            if self.block_allocated(previous)? {
                block
            } else {
                previous
            }
        } else {
            block
        };
        let reference = Ref32::managed(offset.checked_add(16).ok_or(VmFault::CorruptHeap)?)
            .ok_or(VmFault::CorruptHeap)?;
        if !self.free(reference)? {
            return Err(VmFault::CorruptHeap);
        }
        merged
            .0
            .checked_add(self.block_size(merged)?)
            .ok_or(VmFault::CorruptHeap)
    }

    #[cfg(test)]
    pub(super) fn test_arena_address(&self) -> usize {
        self.arena.as_ptr() as usize
    }

    #[cfg(test)]
    pub(super) fn test_reserved_bytes(&self) -> usize {
        self.arena.len() * core::mem::size_of::<u128>()
            + self.class_heads.len() * core::mem::size_of::<u32>()
    }

    #[cfg(test)]
    pub(super) fn test_set_next_ordinal(&mut self, ordinal: u64) {
        self.next_ordinal = ordinal;
    }

    #[cfg(test)]
    pub(super) fn test_managed_payload(&self, reference: Ref32) -> Option<Box<[u8]>> {
        let block = self.live_block(reference).ok()?;
        let length = self.block_size(block).ok()?.checked_sub(USER_PAYLOAD)?;
        let start = block.0.checked_add(USER_PAYLOAD)?;
        let mut bytes = Vec::with_capacity(length as usize);
        for position in start..start + length {
            let unit = self.arena.get((position / 16) as usize)?;
            bytes.push(unit.0[(position % 16) as usize]);
        }
        Some(bytes.into_boxed_slice())
    }

    fn find_suitable(&self, size: u32) -> Result<Option<BlockOffset>, VmFault> {
        let Some(class) = request_size_class(size) else {
            return Ok(None);
        };
        let first = class.first as usize;
        let second_mask = self.second_bitmaps[first] & (u8::MAX << class.second);
        let selected = if second_mask != 0 {
            SizeClass {
                first: class.first,
                second: second_mask.trailing_zeros() as u8,
            }
        } else {
            let higher_first = if class.first == 31 {
                0
            } else {
                self.first_bitmap & (u32::MAX << (u32::from(class.first) + 1))
            };
            if higher_first == 0 {
                return Ok(None);
            }
            let selected_first = higher_first.trailing_zeros() as u8;
            let selected_second = self.second_bitmaps[selected_first as usize];
            if selected_second == 0 {
                return Err(VmFault::CorruptHeap);
            }
            SizeClass {
                first: selected_first,
                second: selected_second.trailing_zeros() as u8,
            }
        };
        let head = *self
            .class_heads
            .get(class_index(selected))
            .ok_or(VmFault::CorruptHeap)?;
        (head != NULL_OFFSET)
            .then_some(BlockOffset(head))
            .map(Some)
            .ok_or(VmFault::CorruptHeap)
    }

    fn insert_free(&mut self, block: BlockOffset) -> Result<(), VmFault> {
        let class = free_size_class(self.block_size(block)?).ok_or(VmFault::CorruptHeap)?;
        let index = class_index(class);
        let head = *self.class_heads.get(index).ok_or(VmFault::CorruptHeap)?;
        self.write_word(block, NEXT_FREE, head)?;
        self.write_word(block, PREVIOUS_FREE, NULL_OFFSET)?;
        if head != NULL_OFFSET {
            self.write_word(BlockOffset(head), PREVIOUS_FREE, block.0)?;
        }
        self.class_heads[index] = block.0;
        self.second_bitmaps[class.first as usize] |= 1 << class.second;
        self.first_bitmap |= 1 << class.first;
        Ok(())
    }

    fn remove_free(&mut self, block: BlockOffset) -> Result<(), VmFault> {
        let class = free_size_class(self.block_size(block)?).ok_or(VmFault::CorruptHeap)?;
        let index = class_index(class);
        let next = self.read_word(block, NEXT_FREE)?;
        let previous = self.read_word(block, PREVIOUS_FREE)?;
        if previous == NULL_OFFSET {
            if self.class_heads[index] != block.0 {
                return Err(VmFault::CorruptHeap);
            }
            self.class_heads[index] = next;
        } else {
            self.write_word(BlockOffset(previous), NEXT_FREE, next)?;
        }
        if next != NULL_OFFSET {
            self.write_word(BlockOffset(next), PREVIOUS_FREE, previous)?;
        }
        if self.class_heads[index] == NULL_OFFSET {
            self.second_bitmaps[class.first as usize] &= !(1 << class.second);
            if self.second_bitmaps[class.first as usize] == 0 {
                self.first_bitmap &= !(1 << class.first);
            }
        }
        self.write_word(block, NEXT_FREE, NULL_OFFSET)?;
        self.write_word(block, PREVIOUS_FREE, NULL_OFFSET)?;
        Ok(())
    }

    fn free_block(&mut self, block: BlockOffset) -> Result<(), VmFault> {
        if !self.block_allocated(block)? {
            return Err(VmFault::CorruptHeap);
        }
        let original_size = self.block_size(block)?;
        let mut merged = block;
        let mut merged_size = original_size;
        let mut previous_size = self.read_word(block, PREVIOUS_SIZE)?;

        if block.0 != 0 {
            let previous = BlockOffset(
                block
                    .0
                    .checked_sub(previous_size)
                    .ok_or(VmFault::CorruptHeap)?,
            );
            if !self.block_allocated(previous)? {
                self.remove_free(previous)?;
                merged = previous;
                merged_size = merged_size
                    .checked_add(self.block_size(previous)?)
                    .ok_or(VmFault::CorruptHeap)?;
                previous_size = self.read_word(previous, PREVIOUS_SIZE)?;
            }
        }

        let next_offset = merged
            .0
            .checked_add(merged_size)
            .ok_or(VmFault::CorruptHeap)?;
        if next_offset < self.arena_bytes {
            let next = BlockOffset(next_offset);
            if !self.block_allocated(next)? {
                self.remove_free(next)?;
                merged_size = merged_size
                    .checked_add(self.block_size(next)?)
                    .ok_or(VmFault::CorruptHeap)?;
            }
        }

        // A direct managed reference names the object header at block + 16. If
        // this block is absorbed into its free predecessor, its old allocator
        // header becomes interior storage and must no longer look allocated.
        if merged != block {
            self.write_word(block, SIZE_FLAGS, 0)?;
        }

        self.write_header(merged, merged_size, previous_size, false)?;
        self.update_next_previous_size(merged, merged_size)?;
        self.insert_free(merged)?;
        self.total_free = self
            .total_free
            .checked_add(original_size)
            .ok_or(VmFault::CorruptHeap)?;
        Ok(())
    }

    fn write_header(
        &mut self,
        block: BlockOffset,
        size: u32,
        previous_size: u32,
        allocated: bool,
    ) -> Result<(), VmFault> {
        if size < 32 || !size.is_multiple_of(16) {
            return Err(VmFault::CorruptHeap);
        }
        self.write_word(block, SIZE_FLAGS, size | u32::from(allocated))?;
        self.write_word(block, PREVIOUS_SIZE, previous_size)?;
        self.write_word(block, NEXT_FREE, NULL_OFFSET)?;
        self.write_word(block, PREVIOUS_FREE, NULL_OFFSET)
    }

    fn update_next_previous_size(&mut self, block: BlockOffset, size: u32) -> Result<(), VmFault> {
        let next = block.0.checked_add(size).ok_or(VmFault::CorruptHeap)?;
        if next < self.arena_bytes {
            self.write_word(BlockOffset(next), PREVIOUS_SIZE, size)?;
        }
        Ok(())
    }

    fn block_size(&self, block: BlockOffset) -> Result<u32, VmFault> {
        let size = self.read_word(block, SIZE_FLAGS)? & SIZE_MASK;
        if size < 32 || !size.is_multiple_of(16) {
            return Err(VmFault::CorruptHeap);
        }
        Ok(size)
    }

    fn block_allocated(&self, block: BlockOffset) -> Result<bool, VmFault> {
        Ok(self.read_word(block, SIZE_FLAGS)? & ALLOCATED != 0)
    }

    fn read_word(&self, block: BlockOffset, field: u32) -> Result<u32, VmFault> {
        let start = block.0.checked_add(field).ok_or(VmFault::CorruptHeap)?;
        let unit = usize::try_from(start / 16).map_err(|_| VmFault::CorruptHeap)?;
        let within = usize::try_from(start % 16).map_err(|_| VmFault::CorruptHeap)?;
        let end = within.checked_add(4).ok_or(VmFault::CorruptHeap)?;
        let bytes: [u8; 4] = self
            .arena
            .get(unit)
            .and_then(|unit| unit.0.get(within..end))
            .ok_or(VmFault::CorruptHeap)?
            .try_into()
            .map_err(|_| VmFault::CorruptHeap)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn write_object_header(
        &mut self,
        block: BlockOffset,
        header: ManagedObjectHeader,
    ) -> Result<(), VmFault> {
        self.write_word(block, OBJECT_TYPE_ID, header.type_id)?;
        self.write_word(block, OBJECT_IDENTITY_TOKEN, header.identity_token)
    }

    fn write_word(&mut self, block: BlockOffset, field: u32, value: u32) -> Result<(), VmFault> {
        let start = block.0.checked_add(field).ok_or(VmFault::CorruptHeap)?;
        let unit = usize::try_from(start / 16).map_err(|_| VmFault::CorruptHeap)?;
        let within = usize::try_from(start % 16).map_err(|_| VmFault::CorruptHeap)?;
        let end = within.checked_add(4).ok_or(VmFault::CorruptHeap)?;
        self.arena
            .get_mut(unit)
            .and_then(|unit| unit.0.get_mut(within..end))
            .ok_or(VmFault::CorruptHeap)?
            .copy_from_slice(&value.to_le_bytes());
        Ok(())
    }
}

pub(super) fn free_size_class(size: u32) -> Option<SizeClass> {
    let (first, second, _) = downward_class(size)?;
    Some(SizeClass { first, second })
}

pub(super) fn request_size_class(size: u32) -> Option<SizeClass> {
    let (mut first, mut second, lower_bound) = downward_class(size)?;
    if size != lower_bound {
        second += 1;
        if second == 8 {
            first = first.checked_add(1)?;
            second = 0;
        }
    }
    (first < 32).then_some(SizeClass { first, second })
}

pub(super) fn splitmix64(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn downward_class(size: u32) -> Option<(u8, u8, u32)> {
    if size < 32 || !size.is_multiple_of(16) {
        return None;
    }
    let first = (31 - size.leading_zeros()) as u8;
    let base = 1_u32.checked_shl(first.into())?;
    let width = if first >= 3 {
        1_u32.checked_shl(u32::from(first - 3))?.max(16)
    } else {
        16
    };
    let second = u8::try_from((size - base) / width).ok()?;
    let lower_bound = base.checked_add(u32::from(second).checked_mul(width)?)?;
    (second < 8).then_some((first, second, lower_bound))
}

fn class_index(class: SizeClass) -> usize {
    usize::from(class.first) * 8 + usize::from(class.second)
}
