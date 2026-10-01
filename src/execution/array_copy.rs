/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

use super::{
    error::{GuestTrap, VmFault},
    heap::Heap,
    value::Ref32,
};

#[derive(Clone, Copy)]
pub(super) struct PendingArrayCopy {
    pub source: Ref32,
    pub destination: Ref32,
    source_start: u32,
    destination_start: u32,
    remaining: u32,
    width: u32,
    backward: bool,
}

impl PendingArrayCopy {
    pub(super) fn new(
        source: (Ref32, i32),
        destination: (Ref32, i32),
        source_start: i32,
        destination_start: i32,
        length: i32,
        width: u32,
    ) -> Result<Self, GuestTrap> {
        if source_start < 0
            || destination_start < 0
            || length < 0
            || source_start
                .checked_add(length)
                .is_none_or(|end| end > source.1)
            || destination_start
                .checked_add(length)
                .is_none_or(|end| end > destination.1)
        {
            return Err(GuestTrap::IndexOutOfBounds);
        }
        Ok(Self {
            source: source.0,
            destination: destination.0,
            source_start: source_start as u32,
            destination_start: destination_start as u32,
            remaining: length as u32,
            width,
            backward: source.0 == destination.0 && destination_start > source_start,
        })
    }

    pub(super) fn advance(&mut self, heap: &mut Heap, budget: u32) -> Result<(u32, bool), VmFault> {
        let used = self.remaining.min(budget);
        let mut left = used;
        while left != 0 {
            let count = left.min(256);
            let offset = if self.backward {
                self.remaining - count
            } else {
                0
            };
            let source_offset = self
                .source_start
                .checked_add(offset)
                .and_then(|index| index.checked_mul(self.width))
                .and_then(|bytes| bytes.checked_add(8))
                .ok_or(VmFault::CorruptHeap)?;
            let destination_offset = self
                .destination_start
                .checked_add(offset)
                .and_then(|index| index.checked_mul(self.width))
                .and_then(|bytes| bytes.checked_add(8))
                .ok_or(VmFault::CorruptHeap)?;
            heap.copy_payload_range(
                self.source,
                source_offset,
                self.destination,
                destination_offset,
                count * self.width,
            )?;
            self.remaining -= count;
            if !self.backward {
                self.source_start += count;
                self.destination_start += count;
            }
            left -= count;
        }
        Ok((used, self.remaining == 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{
        heap::AllocationRequest,
        layout::{HeaderFormat, StoragePlan},
    };

    #[test]
    fn bulk_array_instruction_resumes_with_exact_dynamic_cost_and_one_retirement() {
        use crate::execution::{error::Outcome, fixtures, value::RuntimeValue};
        let artifact = fixtures::array_copy_artifact();
        let mut profile = fixtures::profile();
        profile.heap_bytes = 16384;
        let mut machine = fixtures::started_with_profile(artifact.clone(), profile.clone());
        let mut slices = 0;
        loop {
            match machine.run_slice(11, 1).unwrap() {
                Outcome::SliceExhausted => {
                    slices += 1;
                    assert!(slices < 1000);
                }
                Outcome::Halted(value) => {
                    assert_eq!(Some(RuntimeValue::I32(42)), value);
                    break;
                }
                outcome => panic!("unexpected copy outcome: {outcome:?}"),
            }
        }
        assert!(slices > 60);
        assert_eq!(11, machine.retired_instructions());
        let mut large = fixtures::started_with_profile(artifact, profile);
        loop {
            if matches!(large.run_slice(4096, 1).unwrap(), Outcome::Halted(_)) {
                break;
            }
        }
        assert_eq!(
            large.consumed_dynamic_cost(),
            machine.consumed_dynamic_cost()
        );
        assert_eq!(large.retired_instructions(), machine.retired_instructions());
    }

    fn array(heap: &mut Heap, length: u32, width: u32) -> Ref32 {
        let bytes = (heap.payload_offset() + 8 + length * width + 7) & !7;
        let reservation = heap
            .reserve(AllocationRequest {
                block_bytes: bytes,
                type_id: 1,
            })
            .unwrap()
            .unwrap();
        heap.commit(reservation).unwrap()
    }

    #[test]
    fn bulk_array_copy_preserves_overlap_in_both_directions_across_tiny_budgets() {
        for format in [HeaderFormat::Legacy, HeaderFormat::select(16384, 2)] {
            for width in [1, 2, 4, 8] {
                for (start, destination) in [(0, 1), (1, 0), (0, 0)] {
                    let mut heap =
                        Heap::new(&StoragePlan::heap_only(16384).with_header_format(format))
                            .unwrap();
                    let reference = array(&mut heap, 700, width);
                    for index in 0..700_u32 {
                        heap.write_payload(
                            reference,
                            8 + index * width,
                            &index.to_le_bytes().repeat(2)[..width as usize],
                        )
                        .unwrap();
                    }
                    let before: Vec<_> = (0..700)
                        .map(|index| {
                            heap.read_payload(reference, 8 + index * width, width)
                                .unwrap()
                        })
                        .collect();
                    let mut pending = PendingArrayCopy::new(
                        (reference, 700),
                        (reference, 700),
                        start,
                        destination,
                        699,
                        width,
                    )
                    .unwrap();
                    assert_eq!((0, false), pending.advance(&mut heap, 0).unwrap());
                    let mut total = 0;
                    loop {
                        let (used, done) = pending.advance(&mut heap, 3).unwrap();
                        assert!(used <= 3);
                        total += used;
                        if done {
                            break;
                        }
                    }
                    assert_eq!(699, total);
                    for index in 0..699 {
                        assert_eq!(
                            before[index as usize + start as usize],
                            heap.read_payload(
                                reference,
                                8 + (index + destination as u32) * width,
                                width
                            )
                            .unwrap()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn bulk_array_copy_checks_all_ranges_before_writing_and_allocates_nothing() {
        let mut heap = Heap::new(&StoragePlan::heap_only(16384)).unwrap();
        let source = array(&mut heap, 700, 4);
        let destination = array(&mut heap, 700, 4);
        heap.write_payload(source, 8, &123_i32.to_le_bytes())
            .unwrap();
        for (start, offset, count) in [
            (-1, 0, 0),
            (0, -1, 0),
            (0, 0, -1),
            (701, 0, 0),
            (0, 701, 0),
            (699, 0, 2),
            (0, 699, 2),
            (i32::MAX, 0, 1),
        ] {
            assert!(matches!(
                PendingArrayCopy::new((source, 700), (destination, 700), start, offset, count, 4),
                Err(GuestTrap::IndexOutOfBounds)
            ));
        }
        let mut empty =
            PendingArrayCopy::new((source, 700), (destination, 700), 700, 700, 0, 4).unwrap();
        assert_eq!((0, true), empty.advance(&mut heap, 0).unwrap());
        let mut pending =
            PendingArrayCopy::new((source, 700), (destination, 700), 0, 0, 700, 4).unwrap();
        crate::execution::tests::allocation_counter::reset_and_enable();
        let result = pending.advance(&mut heap, 700);
        let allocations = crate::execution::tests::allocation_counter::disable_and_read();
        assert_eq!((700, true), result.unwrap());
        assert_eq!(0, allocations);
        assert_eq!(
            123_i32.to_le_bytes(),
            heap.read_payload(destination, 8, 4).unwrap()[..4]
        );
    }
}
