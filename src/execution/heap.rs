use super::{
    error::{AdmissionError, VmFault},
    layout::StoragePlan,
    value::{ReferenceDomain, ReferenceValue},
    TypeKey,
};

const NULL_OFFSET: u32 = u32::MAX;
const CLASS_COUNT: usize = 32 * 8;
const ALLOCATED: u32 = 1;
const SIZE_MASK: u32 = !15;
const SIZE_FLAGS: u32 = 0;
const PREVIOUS_SIZE: u32 = 4;
const NEXT_FREE: u32 = 8;
const PREVIOUS_FREE: u32 = 12;

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
    pub ty: TypeKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReservedAllocation {
    pub block: BlockOffset,
    pub slot: u32,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct HeapDiagnostic {
    pub total_free: u32,
    pub largest_free_block: u32,
    pub live_handles: u32,
    pub retired_handles: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HandleState {
    Free,
    Reserved,
    Live,
    Retired,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct HandleEntry {
    pub state: HandleState,
    pub generation: u32,
    pub block: BlockOffset,
    pub ty: TypeKey,
    pub identity_hash: u32,
    pub next_free: u32,
    pub gray_next: u32,
    pub mark_epoch: u32,
}

impl HandleEntry {
    const EMPTY: Self = Self {
        state: HandleState::Free,
        generation: 1,
        block: BlockOffset(NULL_OFFSET),
        ty: TypeKey {
            module: u32::MAX,
            ty: u32::MAX,
        },
        identity_hash: 0,
        next_free: NULL_OFFSET,
        gray_next: NULL_OFFSET,
        mark_epoch: 0,
    };
}

pub(super) struct Heap {
    arena: Box<[ArenaUnit]>,
    arena_bytes: u32,
    class_heads: Box<[u32]>,
    first_bitmap: u32,
    second_bitmaps: [u8; 32],
    handles: Box<[HandleEntry]>,
    free_handle: u32,
    next_ordinal: u64,
    total_free: u32,
    live_handles: u32,
    retired_handles: u32,
}

impl Heap {
    pub(super) fn new(plan: &StoragePlan) -> Result<Self, AdmissionError> {
        if plan.heap_bytes < 32
            || !plan.heap_bytes.is_multiple_of(16)
            || plan.handle_capacity != plan.heap_bytes / 32
        {
            return Err(AdmissionError::InvalidHeapSize {
                supplied: plan.heap_bytes,
            });
        }
        let arena_len = usize::try_from(plan.heap_bytes / 16)
            .map_err(|_| AdmissionError::StoragePlanOverflow)?;
        let handle_count = usize::try_from(plan.handle_capacity)
            .map_err(|_| AdmissionError::StoragePlanOverflow)?;
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
        let mut handles = Vec::new();
        handles
            .try_reserve_exact(handle_count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        handles.resize(handle_count, HandleEntry::EMPTY);
        for (slot, entry) in handles.iter_mut().enumerate() {
            entry.next_free = if slot + 1 < handle_count {
                u32::try_from(slot + 1).map_err(|_| AdmissionError::StoragePlanOverflow)?
            } else {
                NULL_OFFSET
            };
        }

        let mut heap = Self {
            arena: arena.into_boxed_slice(),
            arena_bytes: plan.heap_bytes,
            class_heads: class_heads.into_boxed_slice(),
            first_bitmap: 0,
            second_bitmaps: [0; 32],
            handles: handles.into_boxed_slice(),
            free_handle: 0,
            next_ordinal: 1,
            total_free: plan.heap_bytes,
            live_handles: 0,
            retired_handles: 0,
        };
        heap.write_header(BlockOffset(0), plan.heap_bytes, 0, false)
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
        if self.free_handle == NULL_OFFSET {
            return Err(VmFault::HandleExhausted);
        }
        let slot = self.free_handle;
        let entry = *self
            .handles
            .get(slot as usize)
            .ok_or(VmFault::CorruptHeap)?;
        if entry.state != HandleState::Free {
            return Err(VmFault::CorruptHeap);
        }

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

        self.free_handle = entry.next_free;
        let handle = self
            .handles
            .get_mut(slot as usize)
            .ok_or(VmFault::CorruptHeap)?;
        handle.state = HandleState::Reserved;
        handle.block = block;
        handle.ty = request.ty;
        handle.identity_hash = 0;
        handle.next_free = NULL_OFFSET;
        Ok(Some(ReservedAllocation {
            block,
            slot,
            generation: handle.generation,
        }))
    }

    pub(super) fn commit(
        &mut self,
        reservation: ReservedAllocation,
    ) -> Result<ReferenceValue, VmFault> {
        let entry = self
            .handles
            .get_mut(reservation.slot as usize)
            .ok_or(VmFault::CorruptHeap)?;
        if entry.state != HandleState::Reserved
            || entry.generation != reservation.generation
            || entry.block != reservation.block
        {
            return Err(VmFault::CorruptHeap);
        }
        entry.state = HandleState::Live;
        entry.identity_hash = splitmix64(self.next_ordinal) as u32;
        self.next_ordinal = self.next_ordinal.wrapping_add(1);
        self.live_handles = self
            .live_handles
            .checked_add(1)
            .ok_or(VmFault::CorruptHeap)?;
        ReferenceValue::managed(reservation.slot, reservation.generation)
            .ok_or(VmFault::CorruptHeap)
    }

    pub(super) fn abort(&mut self, reservation: ReservedAllocation) -> Result<(), VmFault> {
        let entry = self
            .handles
            .get(reservation.slot as usize)
            .copied()
            .ok_or(VmFault::CorruptHeap)?;
        if entry.state != HandleState::Reserved
            || entry.generation != reservation.generation
            || entry.block != reservation.block
        {
            return Err(VmFault::CorruptHeap);
        }
        self.release_unpublished_handle(reservation.slot)?;
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
            .checked_sub(16)
            .ok_or(VmFault::CorruptHeap)?;
        let end = offset.checked_add(length).ok_or(VmFault::CorruptHeap)?;
        if end > capacity {
            return Err(VmFault::CorruptHeap);
        }
        let start = reservation
            .block
            .0
            .checked_add(16)
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
            .checked_sub(16)
            .ok_or(VmFault::CorruptHeap)?;
        if offset.checked_add(4).ok_or(VmFault::CorruptHeap)? > capacity {
            return Err(VmFault::CorruptHeap);
        }
        let start = reservation
            .block
            .0
            .checked_add(16)
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

    fn validate_reservation(&self, reservation: ReservedAllocation) -> Result<(), VmFault> {
        let entry = self
            .handles
            .get(reservation.slot as usize)
            .ok_or(VmFault::CorruptHeap)?;
        if entry.state == HandleState::Reserved
            && entry.generation == reservation.generation
            && entry.block == reservation.block
        {
            Ok(())
        } else {
            Err(VmFault::CorruptHeap)
        }
    }

    pub(super) fn free(&mut self, reference: ReferenceValue) -> Result<bool, VmFault> {
        if reference.domain() != ReferenceDomain::Managed {
            return Ok(false);
        }
        let slot = reference.slot();
        let entry = self
            .handles
            .get(slot as usize)
            .copied()
            .ok_or(VmFault::CorruptHeap)?;
        if entry.state != HandleState::Live || entry.generation != reference.generation() {
            return Ok(false);
        }
        self.live_handles = self
            .live_handles
            .checked_sub(1)
            .ok_or(VmFault::CorruptHeap)?;
        let next_generation = entry.generation.checked_add(1);
        if let Some(generation) = next_generation {
            let free_head = self.free_handle;
            let handle = self
                .handles
                .get_mut(slot as usize)
                .ok_or(VmFault::CorruptHeap)?;
            *handle = HandleEntry {
                generation,
                next_free: free_head,
                ..HandleEntry::EMPTY
            };
            self.free_handle = slot;
        } else {
            let handle = self
                .handles
                .get_mut(slot as usize)
                .ok_or(VmFault::CorruptHeap)?;
            handle.state = HandleState::Retired;
            handle.next_free = NULL_OFFSET;
            self.retired_handles = self
                .retired_handles
                .checked_add(1)
                .ok_or(VmFault::CorruptHeap)?;
        }
        self.free_block(entry.block)?;
        Ok(true)
    }

    pub(super) fn runtime_type(&self, reference: ReferenceValue) -> Option<TypeKey> {
        if reference.domain() != ReferenceDomain::Managed {
            return None;
        }
        self.handles
            .get(reference.slot() as usize)
            .and_then(|entry| {
                (entry.state == HandleState::Live && entry.generation == reference.generation())
                    .then_some(entry.ty)
            })
    }

    pub(super) fn identity_hash(&self, reference: ReferenceValue) -> Option<u32> {
        if reference.domain() != ReferenceDomain::Managed {
            return None;
        }
        self.handles
            .get(reference.slot() as usize)
            .and_then(|entry| {
                (entry.state == HandleState::Live && entry.generation == reference.generation())
                    .then_some(entry.identity_hash)
            })
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
            live_handles: self.live_handles,
            retired_handles: self.retired_handles,
        }
    }

    #[cfg(test)]
    pub(super) fn test_set_generation(&mut self, slot: u32, generation: u32) {
        if let Some(entry) = self.handles.get_mut(slot as usize) {
            if entry.state == HandleState::Free {
                entry.generation = generation;
            }
        }
    }

    #[cfg(test)]
    pub(super) fn test_arena_address(&self) -> usize {
        self.arena.as_ptr() as usize
    }

    #[cfg(test)]
    pub(super) fn test_managed_payload(&self, reference: ReferenceValue) -> Option<Box<[u8]>> {
        let entry = self.handles.get(reference.slot() as usize)?;
        if reference.domain() != ReferenceDomain::Managed
            || entry.state != HandleState::Live
            || entry.generation != reference.generation()
        {
            return None;
        }
        let length = self.block_size(entry.block).ok()?.checked_sub(16)?;
        let start = entry.block.0.checked_add(16)?;
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

        self.write_header(merged, merged_size, previous_size, false)?;
        self.update_next_previous_size(merged, merged_size)?;
        self.insert_free(merged)?;
        self.total_free = self
            .total_free
            .checked_add(original_size)
            .ok_or(VmFault::CorruptHeap)?;
        Ok(())
    }

    fn release_unpublished_handle(&mut self, slot: u32) -> Result<(), VmFault> {
        let free_head = self.free_handle;
        let entry = self
            .handles
            .get_mut(slot as usize)
            .ok_or(VmFault::CorruptHeap)?;
        let generation = entry.generation;
        *entry = HandleEntry {
            generation,
            next_free: free_head,
            ..HandleEntry::EMPTY
        };
        self.free_handle = slot;
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
