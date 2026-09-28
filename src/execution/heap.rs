use super::{
    error::{AdmissionError, ResidentStorageComponent, VmFault},
    layout::{HeaderFormat, StoragePlan, BLOCK_ALIGNMENT, BLOCK_HEADER_BYTES, MINIMUM_BLOCK_BYTES},
    value::{Ref32, ReferenceDomain},
};

const NULL_OFFSET: u32 = u32::MAX;
const CLASS_COUNT: usize = 32 * 8;
const ALLOCATED: u32 = 1;
const MARKED: u32 = 2;
const LIVE: u32 = 4;
const SIZE_MASK: u32 = !(BLOCK_ALIGNMENT - 1);
const SIZE_FLAGS: u32 = 0;
// In the legacy format, this word holds phase-exclusive predecessor size or gray link.
// The compact format packs the same phase-exclusive value into the 64-bit header.
// Sweep restores predecessor sizes before any block is coalesced.
const PREVIOUS_SIZE: u32 = 4;
const NEXT_FREE: u32 = 8;
const PREVIOUS_FREE: u32 = 12;
// Managed identity is the non-moving Ref32 offset, not an allocation token.
// Free-list links overlap user payload only while the block is free.
const OBJECT_TYPE_ID: u32 = BLOCK_HEADER_BYTES;

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct HeapDiagnostic {
    pub total_free: u32,
    pub largest_free_block: u32,
    pub live_handles: u32,
    pub retired_handles: u32,
}

pub(super) struct Heap {
    header_format: HeaderFormat,
    arena: Box<[ArenaUnit]>,
    arena_bytes: u32,
    class_heads: Box<[u32]>,
    first_bitmap: u32,
    second_bitmaps: [u8; 32],
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
            header_format: plan.header_format,
            arena: arena.into_boxed_slice(),
            arena_bytes: heap_bytes,
            class_heads: class_heads.into_boxed_slice(),
            first_bitmap: 0,
            second_bitmaps: [0; 32],
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
        if request.block_bytes < MINIMUM_BLOCK_BYTES
            || !request.block_bytes.is_multiple_of(BLOCK_ALIGNMENT)
        {
            return Err(VmFault::InvalidStoragePlan);
        }
        let Some(block) = self.find_suitable(request.block_bytes)? else {
            return Ok(None);
        };
        let block_size = self.block_size(block)?;
        if block_size < request.block_bytes {
            return Err(VmFault::CorruptHeap);
        }
        let previous_size = self.read_previous_or_gray(block)?;
        self.remove_free(block)?;
        let remainder = block_size - request.block_bytes;
        let allocated_size = if remainder >= MINIMUM_BLOCK_BYTES {
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
        self.write_type(reservation.block, reservation.type_id)?;
        let flags = self.read_flags(reservation.block)?;
        self.write_flags(reservation.block, flags | LIVE)?;
        self.live_objects = self
            .live_objects
            .checked_add(1)
            .ok_or(VmFault::CorruptHeap)?;
        let object_header = reservation
            .block
            .0
            .checked_add(OBJECT_TYPE_ID)
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
            .checked_sub(self.payload_offset())
            .ok_or(VmFault::CorruptHeap)?;
        let end = offset.checked_add(length).ok_or(VmFault::CorruptHeap)?;
        if end > capacity {
            return Err(VmFault::CorruptHeap);
        }
        let start = reservation
            .block
            .0
            .checked_add(self.payload_offset())
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
            .checked_sub(self.payload_offset())
            .ok_or(VmFault::CorruptHeap)?;
        if offset.checked_add(4).ok_or(VmFault::CorruptHeap)? > capacity {
            return Err(VmFault::CorruptHeap);
        }
        let start = reservation
            .block
            .0
            .checked_add(self.payload_offset())
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
            .checked_sub(self.payload_offset())
            .ok_or(VmFault::CorruptHeap)?;
        let length = u32::try_from(bytes.len()).map_err(|_| VmFault::CorruptHeap)?;
        if offset.checked_add(length).ok_or(VmFault::CorruptHeap)? > capacity {
            return Err(VmFault::CorruptHeap);
        }
        let start = reservation
            .block
            .0
            .checked_add(self.payload_offset())
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
            && self.read_flags(reservation.block)? & LIVE == 0
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
        self.read_type(block)
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
            .checked_sub(self.payload_offset())
            .ok_or(VmFault::CorruptHeap)?;
        let end = offset.checked_add(length).ok_or(VmFault::CorruptHeap)?;
        if length > 8 || end > capacity {
            return Err(VmFault::CorruptHeap);
        }
        let start = block
            .0
            .checked_add(self.payload_offset())
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
            .checked_sub(self.payload_offset())
            .ok_or(VmFault::CorruptHeap)?;
        let end = offset.checked_add(length).ok_or(VmFault::CorruptHeap)?;
        if end > capacity {
            return Err(VmFault::CorruptHeap);
        }
        let start = block
            .0
            .checked_add(self.payload_offset())
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
            || !reference.payload().is_multiple_of(BLOCK_ALIGNMENT)
        {
            return Err(VmFault::InvalidReference);
        }
        let block = BlockOffset(
            reference
                .payload()
                .checked_sub(OBJECT_TYPE_ID)
                .ok_or(VmFault::InvalidReference)?,
        );
        if block.0 >= self.arena_bytes
            || !self.block_allocated(block)?
            || self.read_flags(block)? & LIVE == 0
        {
            return Err(VmFault::InvalidReference);
        }
        Ok(block)
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

    pub(super) const fn total_free_bytes(&self) -> u32 {
        self.total_free
    }

    pub(super) const fn live_objects(&self) -> u32 {
        self.live_objects
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
        let flags = self.read_flags(block)?;
        if flags & MARKED != 0 {
            return Ok(());
        }
        self.write_flags(block, flags | MARKED)?;
        self.write_previous_or_gray(block, NULL_OFFSET)?;
        if let Some(previous) = *tail {
            let previous = Ref32::managed(previous).ok_or(VmFault::CorruptHeap)?;
            let previous_block = self.live_block(previous)?;
            self.write_previous_or_gray(previous_block, reference.payload())?;
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
        let next = self.read_previous_or_gray(block)?;
        self.write_previous_or_gray(block, NULL_OFFSET)?;
        *head = (next != NULL_OFFSET && next != 0).then_some(next);
        if head.is_none() {
            *tail = None;
        }
        Ok(Some((reference, self.managed_type(reference)?)))
    }

    pub(super) fn arena_bytes(&self) -> u32 {
        self.arena_bytes
    }

    pub(super) const fn payload_offset(&self) -> u32 {
        self.header_format.bytes()
    }

    pub(super) const fn header_format(&self) -> HeaderFormat {
        self.header_format
    }

    pub(super) fn sweep_block(
        &mut self,
        offset: u32,
        previous_size: u32,
    ) -> Result<(u32, u32), VmFault> {
        if offset >= self.arena_bytes {
            return Err(VmFault::CorruptHeap);
        }
        let block = BlockOffset(offset);
        let size = self.block_size(block)?;
        // Marking temporarily borrowed this word for the intrusive gray queue.
        // The forward sweep knows the effective predecessor size even after merging.
        self.write_previous_or_gray(block, previous_size)?;
        if !self.block_allocated(block)? {
            return Ok((offset.checked_add(size).ok_or(VmFault::CorruptHeap)?, size));
        }
        let flags = self.read_flags(block)?;
        if flags & LIVE == 0 {
            return Err(VmFault::CorruptHeap);
        }
        if flags & MARKED != 0 {
            self.write_flags(block, flags & !MARKED)?;
            return Ok((offset.checked_add(size).ok_or(VmFault::CorruptHeap)?, size));
        }

        let previous_size = self.read_previous_or_gray(block)?;
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
        let reference = Ref32::managed(
            offset
                .checked_add(OBJECT_TYPE_ID)
                .ok_or(VmFault::CorruptHeap)?,
        )
        .ok_or(VmFault::CorruptHeap)?;
        if !self.free(reference)? {
            return Err(VmFault::CorruptHeap);
        }
        let merged_size = self.block_size(merged)?;
        Ok((
            merged
                .0
                .checked_add(merged_size)
                .ok_or(VmFault::CorruptHeap)?,
            merged_size,
        ))
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
    pub(super) fn test_managed_payload(&self, reference: Ref32) -> Option<Box<[u8]>> {
        let block = self.live_block(reference).ok()?;
        let length = self
            .block_size(block)
            .ok()?
            .checked_sub(self.payload_offset())?;
        let start = block.0.checked_add(self.payload_offset())?;
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
        let mut previous_size = self.read_previous_or_gray(block)?;

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
                previous_size = self.read_previous_or_gray(previous)?;
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

        // A direct managed reference names the object header at block + 8. If
        // this block is absorbed into its free predecessor, its old allocator
        // header becomes interior storage and must no longer look allocated.
        if merged != block {
            self.write_flags(block, 0)?;
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
        if size < MINIMUM_BLOCK_BYTES || !size.is_multiple_of(BLOCK_ALIGNMENT) {
            return Err(VmFault::CorruptHeap);
        }
        self.write_block_header(block, size, previous_size, allocated)?;
        self.write_word(block, NEXT_FREE, NULL_OFFSET)?;
        self.write_word(block, PREVIOUS_FREE, NULL_OFFSET)
    }

    fn update_next_previous_size(&mut self, block: BlockOffset, size: u32) -> Result<(), VmFault> {
        let next = block.0.checked_add(size).ok_or(VmFault::CorruptHeap)?;
        if next < self.arena_bytes {
            self.write_previous_or_gray(BlockOffset(next), size)?;
        }
        Ok(())
    }

    fn block_size(&self, block: BlockOffset) -> Result<u32, VmFault> {
        let size = match self.header_format {
            HeaderFormat::Legacy => self.read_word(block, SIZE_FLAGS)? & SIZE_MASK,
            HeaderFormat::Compact { size_bits, .. } => {
                let units = (self.read_compact_header(block)? >> 3) & bit_mask(size_bits);
                u32::try_from(units * u64::from(BLOCK_ALIGNMENT))
                    .map_err(|_| VmFault::CorruptHeap)?
            }
        };
        if size < MINIMUM_BLOCK_BYTES || !size.is_multiple_of(BLOCK_ALIGNMENT) {
            return Err(VmFault::CorruptHeap);
        }
        Ok(size)
    }

    fn block_allocated(&self, block: BlockOffset) -> Result<bool, VmFault> {
        Ok(self.read_flags(block)? & ALLOCATED != 0)
    }

    fn read_flags(&self, block: BlockOffset) -> Result<u32, VmFault> {
        match self.header_format {
            HeaderFormat::Legacy => Ok(self.read_word(block, SIZE_FLAGS)? & !SIZE_MASK),
            HeaderFormat::Compact { .. } => Ok((self.read_compact_header(block)? & 7) as u32),
        }
    }

    fn write_flags(&mut self, block: BlockOffset, flags: u32) -> Result<(), VmFault> {
        if flags & !7 != 0 {
            return Err(VmFault::CorruptHeap);
        }
        match self.header_format {
            HeaderFormat::Legacy => {
                let size = self.read_word(block, SIZE_FLAGS)? & SIZE_MASK;
                self.write_word(block, SIZE_FLAGS, size | flags)
            }
            HeaderFormat::Compact { .. } => {
                let header = self.read_compact_header(block)?;
                self.write_compact_header(block, (header & !7) | u64::from(flags))
            }
        }
    }

    fn read_previous_or_gray(&self, block: BlockOffset) -> Result<u32, VmFault> {
        match self.header_format {
            HeaderFormat::Legacy => self.read_word(block, PREVIOUS_SIZE),
            HeaderFormat::Compact {
                size_bits,
                link_bits,
            } => {
                let units =
                    (self.read_compact_header(block)? >> (3 + size_bits)) & bit_mask(link_bits);
                u32::try_from(units * u64::from(BLOCK_ALIGNMENT)).map_err(|_| VmFault::CorruptHeap)
            }
        }
    }

    fn write_previous_or_gray(&mut self, block: BlockOffset, value: u32) -> Result<(), VmFault> {
        match self.header_format {
            HeaderFormat::Legacy => self.write_word(block, PREVIOUS_SIZE, value),
            HeaderFormat::Compact {
                size_bits,
                link_bits,
            } => {
                let units = if value == NULL_OFFSET {
                    0
                } else {
                    if !value.is_multiple_of(BLOCK_ALIGNMENT) {
                        return Err(VmFault::CorruptHeap);
                    }
                    u64::from(value / BLOCK_ALIGNMENT)
                };
                if units > bit_mask(link_bits) {
                    return Err(VmFault::CorruptHeap);
                }
                let shift = 3 + size_bits;
                let mask = bit_mask(link_bits) << shift;
                let header = self.read_compact_header(block)?;
                self.write_compact_header(block, (header & !mask) | (units << shift))
            }
        }
    }

    fn read_type(&self, block: BlockOffset) -> Result<u32, VmFault> {
        match self.header_format {
            HeaderFormat::Legacy => self.read_word(block, OBJECT_TYPE_ID),
            HeaderFormat::Compact {
                size_bits,
                link_bits,
            } => u32::try_from(self.read_compact_header(block)? >> (3 + size_bits + link_bits))
                .map_err(|_| VmFault::CorruptHeap),
        }
    }

    fn write_type(&mut self, block: BlockOffset, type_id: u32) -> Result<(), VmFault> {
        match self.header_format {
            HeaderFormat::Legacy => self.write_word(block, OBJECT_TYPE_ID, type_id),
            HeaderFormat::Compact {
                size_bits,
                link_bits,
            } => {
                let shift = 3 + size_bits + link_bits;
                let mask = bit_mask(64 - shift);
                if u64::from(type_id) > mask {
                    return Err(VmFault::CorruptHeap);
                }
                let header = self.read_compact_header(block)?;
                self.write_compact_header(
                    block,
                    (header & !(mask << shift)) | (u64::from(type_id) << shift),
                )
            }
        }
    }

    fn write_block_header(
        &mut self,
        block: BlockOffset,
        size: u32,
        previous_size: u32,
        allocated: bool,
    ) -> Result<(), VmFault> {
        match self.header_format {
            HeaderFormat::Legacy => {
                self.write_word(block, SIZE_FLAGS, size | u32::from(allocated))?;
                self.write_word(block, PREVIOUS_SIZE, previous_size)
            }
            HeaderFormat::Compact {
                size_bits,
                link_bits,
            } => {
                if !previous_size.is_multiple_of(BLOCK_ALIGNMENT) {
                    return Err(VmFault::CorruptHeap);
                }
                let size_units = u64::from(size / BLOCK_ALIGNMENT);
                let previous_units = u64::from(previous_size / BLOCK_ALIGNMENT);
                if size_units > bit_mask(size_bits) || previous_units > bit_mask(link_bits) {
                    return Err(VmFault::CorruptHeap);
                }
                let header =
                    u64::from(allocated) | (size_units << 3) | (previous_units << (3 + size_bits));
                self.write_compact_header(block, header)
            }
        }
    }

    fn read_compact_header(&self, block: BlockOffset) -> Result<u64, VmFault> {
        let unit = self
            .arena
            .get((block.0 / 16) as usize)
            .ok_or(VmFault::CorruptHeap)?;
        let within = (block.0 % 16) as usize;
        let bytes: [u8; 8] = unit
            .0
            .get(within..within + 8)
            .ok_or(VmFault::CorruptHeap)?
            .try_into()
            .map_err(|_| VmFault::CorruptHeap)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn write_compact_header(&mut self, block: BlockOffset, header: u64) -> Result<(), VmFault> {
        let unit = self
            .arena
            .get_mut((block.0 / 16) as usize)
            .ok_or(VmFault::CorruptHeap)?;
        let within = (block.0 % 16) as usize;
        unit.0
            .get_mut(within..within + 8)
            .ok_or(VmFault::CorruptHeap)?
            .copy_from_slice(&header.to_le_bytes());
        Ok(())
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

const fn bit_mask(bits: u32) -> u64 {
    u64::MAX >> (64 - bits)
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

fn downward_class(size: u32) -> Option<(u8, u8, u32)> {
    if size < MINIMUM_BLOCK_BYTES || !size.is_multiple_of(BLOCK_ALIGNMENT) {
        return None;
    }
    let first = (31 - size.leading_zeros()) as u8;
    let base = 1_u32.checked_shl(first.into())?;
    let width = if first >= 3 {
        1_u32
            .checked_shl(u32::from(first - 3))?
            .max(BLOCK_ALIGNMENT)
    } else {
        BLOCK_ALIGNMENT
    };
    let second = u8::try_from((size - base) / width).ok()?;
    let lower_bound = base.checked_add(u32::from(second).checked_mul(width)?)?;
    (second < 8).then_some((first, second, lower_bound))
}

fn class_index(class: SizeClass) -> usize {
    usize::from(class.first) * 8 + usize::from(class.second)
}
