use super::{
    error::{AdmissionError, AllocationExhaustion, GuestTrap, Outcome, VmFault},
    external_roots::ExternalRootTable,
    fixtures,
    heap::{free_size_class, request_size_class, AllocationRequest, BlockOffset, Heap, SizeClass},
    heap_ops::{PendingAllocation, PendingState},
    image::{deduplicate_literal_ranges, ExecutionImage},
    layout::{
        array_layout as array_layout_with_header, empty_object_layout,
        object_layout as object_layout_with_header, string_layout as string_layout_with_header,
        ArrayLayout, FieldSpec, HeaderFormat, ObjectLayout, RuntimeTypeLayout, StoragePlan,
        StringEncoding, StringLayout, ValueWidth,
    },
    machine::{Frame, Machine, TypeInitializationState},
    task::TaskScheduler,
    value::{EntryArgument, RuntimeValue},
    TypeKey,
};
use crate::artifact::ByteRange;

#[test]
fn reference_identity_compares_typed_arrays_and_null_without_casts() {
    for same_type in [false, true] {
        for right_is_null in [false, true] {
            for not_equal in [false, true] {
                let mut machine = fixtures::started(
                    fixtures::reference_identity_artifact(same_type, right_is_null, not_equal),
                    &[],
                );
                let expected = (same_type && !right_is_null) != not_equal;
                loop {
                    match machine.run_slice(32, 8).unwrap() {
                        Outcome::SliceExhausted => {}
                        outcome => {
                            assert_eq!(
                                Outcome::Halted(Some(RuntimeValue::Bool(expected))),
                                outcome
                            );
                            break;
                        }
                    }
                }
            }
        }
    }
}

fn object_layout(
    superclass: Option<&ObjectLayout>,
    fields: &[FieldSpec],
) -> Result<ObjectLayout, AdmissionError> {
    object_layout_with_header(superclass, fields, HeaderFormat::Legacy)
}

fn array_layout(element: ValueWidth, length: i32) -> Result<ArrayLayout, AdmissionError> {
    array_layout_with_header(element, length, HeaderFormat::Legacy)
}

fn string_layout(encoding: StringEncoding, length: u32) -> Result<StringLayout, AdmissionError> {
    string_layout_with_header(encoding, length, HeaderFormat::Legacy)
}

#[test]
fn allocator_size_classes_map_free_blocks_down_and_requests_up() {
    let class = |first, second| Some(SizeClass { first, second });
    for (size, expected) in [
        (16, class(4, 0)),
        (24, class(4, 1)),
        (32, class(5, 0)),
        (48, class(5, 2)),
        (64, class(6, 0)),
        (112, class(6, 6)),
        (128, class(7, 0)),
        (240, class(7, 7)),
        (256, class(8, 0)),
        (272, class(8, 0)),
        (288, class(8, 1)),
    ] {
        assert_eq!(expected, free_size_class(size), "free size {size}");
    }
    assert_eq!(class(8, 0), request_size_class(256));
    assert_eq!(class(8, 1), request_size_class(272));
    assert_eq!(class(8, 1), request_size_class(288));
    assert_eq!(None, free_size_class(8));
    assert_eq!(None, free_size_class(33));
}

fn allocator_plan(heap_bytes: u32) -> StoragePlan {
    StoragePlan::heap_only(u64::from(heap_bytes))
}

fn allocator_request(block_bytes: u32) -> AllocationRequest {
    AllocationRequest {
        block_bytes,
        type_id: 1,
    }
}

#[test]
fn allocator_splits_and_reuses_a_16_byte_tail() -> Result<(), AdmissionError> {
    let mut heap = Heap::new(&allocator_plan(128))?;
    let first = heap.reserve(allocator_request(48)).unwrap().unwrap();
    assert_eq!(BlockOffset(0), first.block);
    assert_eq!(80, heap.diagnostic().total_free);

    let second = heap.reserve(allocator_request(64)).unwrap().unwrap();
    assert_eq!(BlockOffset(48), second.block);
    assert_eq!(16, heap.diagnostic().total_free);
    assert!(heap.reserve(allocator_request(32)).unwrap().is_none());
    let tail = heap.reserve(allocator_request(16)).unwrap().unwrap();
    assert_eq!(BlockOffset(112), tail.block);
    assert_eq!(0, heap.diagnostic().total_free);
    heap.abort(tail).unwrap();
    heap.abort(second).unwrap();
    heap.abort(first).unwrap();
    assert_eq!(128, heap.diagnostic().largest_free_block);
    Ok(())
}

#[test]
fn allocator_absorbs_only_tails_below_16_bytes() -> Result<(), AdmissionError> {
    let mut heap = Heap::new(&allocator_plan(32))?;
    let reservation = heap.reserve(allocator_request(24)).unwrap().unwrap();
    assert_eq!(0, heap.diagnostic().total_free);
    heap.abort(reservation).unwrap();
    assert_eq!(32, heap.diagnostic().largest_free_block);
    Ok(())
}

#[test]
fn allocator_uses_exact_fit_and_lifo_free_lists() -> Result<(), AdmissionError> {
    let mut fragmented = Heap::new(&allocator_plan(272))?;
    let exact = fragmented.reserve(allocator_request(272)).unwrap().unwrap();
    assert_eq!(BlockOffset(0), exact.block);
    assert_eq!(0, fragmented.diagnostic().total_free);
    fragmented.abort(exact).unwrap();
    assert_eq!(
        BlockOffset(0),
        fragmented
            .reserve(allocator_request(256))
            .unwrap()
            .unwrap()
            .block
    );

    let mut heap = Heap::new(&allocator_plan(160))?;
    let mut references = Vec::new();
    for _ in 0..5 {
        let reserved = heap.reserve(allocator_request(32)).unwrap().unwrap();
        references.push(heap.commit(reserved).unwrap());
    }
    assert!(heap.free(references[1]).unwrap());
    assert!(heap.free(references[3]).unwrap());
    assert_eq!(
        BlockOffset(96),
        heap.reserve(allocator_request(32)).unwrap().unwrap().block
    );
    Ok(())
}

#[test]
fn allocator_accepts_fitting_requests_inside_size_class_boundaries() {
    for arena_bytes in (256..=512).step_by(16) {
        for request_bytes in (256..=arena_bytes).step_by(8) {
            let mut heap = Heap::new(&allocator_plan(arena_bytes)).unwrap();
            assert!(
                heap.reserve(allocator_request(request_bytes))
                    .unwrap()
                    .is_some(),
                "request {request_bytes}, arena {arena_bytes}"
            );
        }
    }
}

#[test]
fn allocator_accepts_the_reported_array_list_growth_request() {
    let format = HeaderFormat::select(256 * 1024, 2);
    let mut heap = Heap::new(&allocator_plan(256 * 1024).with_header_format(format)).unwrap();
    heap.reserve(allocator_request(104_688)).unwrap().unwrap();
    assert_eq!(157_456, heap.diagnostic().largest_free_block);
    let grown = heap.reserve(allocator_request(156_912)).unwrap().unwrap();
    assert_eq!(BlockOffset(104_688), grown.block);
    assert_eq!(544, heap.diagnostic().total_free);
}

#[test]
fn allocator_finds_a_fitting_block_behind_a_smaller_free_list_head() {
    let mut heap = Heap::new(&allocator_plan(1024)).unwrap();
    let mut references = Vec::new();
    for size in [272, 16, 256, 16, 464] {
        let reservation = heap.reserve(allocator_request(size)).unwrap().unwrap();
        references.push(heap.commit(reservation).unwrap());
    }
    heap.free(references[0]).unwrap();
    heap.free(references[2]).unwrap();
    assert_eq!(
        BlockOffset(0),
        heap.reserve(allocator_request(264)).unwrap().unwrap().block
    );
    assert!(heap.reserve(allocator_request(264)).unwrap().is_none());
}

#[test]
fn allocator_coalesces_both_neighbors_and_restores_the_arena() -> Result<(), AdmissionError> {
    let mut heap = Heap::new(&allocator_plan(128))?;
    let first = heap.reserve(allocator_request(32)).unwrap().unwrap();
    let second = heap.reserve(allocator_request(32)).unwrap().unwrap();
    let third = heap.reserve(allocator_request(32)).unwrap().unwrap();
    heap.abort(first).unwrap();
    heap.abort(third).unwrap();
    heap.abort(second).unwrap();

    assert_eq!(128, heap.diagnostic().total_free);
    assert_eq!(128, heap.diagnostic().largest_free_block);
    assert_eq!(
        BlockOffset(0),
        heap.reserve(allocator_request(128)).unwrap().unwrap().block
    );
    Ok(())
}

#[test]
fn allocator_commits_direct_offsets_and_reuses_freed_blocks() -> Result<(), AdmissionError> {
    let mut heap = Heap::new(&allocator_plan(64))?;
    let aborted = heap.reserve(allocator_request(32)).unwrap().unwrap();
    heap.abort(aborted).unwrap();
    let reused = heap.reserve(allocator_request(32)).unwrap().unwrap();
    assert_eq!(aborted.block, reused.block);

    let first = heap.commit(reused).unwrap();
    assert_eq!(8, first.payload());
    assert_eq!(Some(1), heap.runtime_type(first));
    assert!(heap.free(first).unwrap());
    assert_eq!(None, heap.runtime_type(first));

    let next = heap.reserve(allocator_request(32)).unwrap().unwrap();
    assert_eq!(BlockOffset(0), next.block);
    let next = heap.commit(next).unwrap();
    assert_eq!(first, next);
    assert_eq!(Some(1), heap.runtime_type(next));
    Ok(())
}

#[test]
fn allocator_has_no_managed_handle_capacity() -> Result<(), AdmissionError> {
    let mut heap = Heap::new(&allocator_plan(64))?;
    assert!(heap.reserve(allocator_request(32)).unwrap().is_some());
    Ok(())
}

#[test]
fn allocator_diagnostics_are_bounded_scalars() {
    assert!(core::mem::size_of::<super::heap::HeapDiagnostic>() <= 32);
    assert!(core::mem::size_of::<super::error::AllocationDiagnostic>() <= 40);
}

#[test]
fn allocator_arena_is_physically_sixteen_byte_aligned() -> Result<(), AdmissionError> {
    let heap = Heap::new(&allocator_plan(128))?;
    assert_eq!(0, heap.test_arena_address() % 16);
    Ok(())
}

#[test]
fn allocator_steady_state_allocates_nothing() -> Result<(), AdmissionError> {
    let mut heap = Heap::new(&allocator_plan(128))?;
    super::tests::allocation_counter::reset_and_enable();
    for _ in 0..1_000 {
        let reserved = heap.reserve(allocator_request(32)).unwrap().unwrap();
        let reference = heap.commit(reserved).unwrap();
        assert!(heap.free(reference).unwrap());
    }
    let allocations = super::tests::allocation_counter::disable_and_read();
    assert_eq!(0, allocations);
    Ok(())
}

#[test]
#[ignore = "records a hardware-specific managed-heap performance baseline"]
fn managed_heap_performance_allocator_and_fragmentation() {
    use std::time::Instant;

    const ITERATIONS: u32 = 100_000;
    println!("workload\titerations\telapsed_ns\toperations_per_s\ttotal_free\tlargest_free");
    for block_bytes in [32, 64, 256] {
        let mut heap = Heap::new(&allocator_plan(4_096)).unwrap();
        let started = Instant::now();
        for _ in 0..ITERATIONS {
            let reservation = heap
                .reserve(allocator_request(block_bytes))
                .unwrap()
                .unwrap();
            let reference = heap.commit(reservation).unwrap();
            assert!(heap.free(reference).unwrap());
        }
        let elapsed = started.elapsed();
        let diagnostic = heap.diagnostic();
        println!(
            "allocate_free_{block_bytes}\t{ITERATIONS}\t{}\t{:.0}\t{}\t{}",
            elapsed.as_nanos(),
            f64::from(ITERATIONS) / elapsed.as_secs_f64(),
            diagnostic.total_free,
            diagnostic.largest_free_block,
        );
    }

    let started = Instant::now();
    for _ in 0..ITERATIONS {
        let mut heap = Heap::new(&allocator_plan(128)).unwrap();
        let values = [
            heap.reserve(allocator_request(32)).unwrap().unwrap(),
            heap.reserve(allocator_request(32)).unwrap().unwrap(),
            heap.reserve(allocator_request(32)).unwrap().unwrap(),
            heap.reserve(allocator_request(32)).unwrap().unwrap(),
        ];
        for reservation in values {
            heap.abort(reservation).unwrap();
        }
    }
    let elapsed = started.elapsed();
    println!(
        "fragment_coalesce\t{ITERATIONS}\t{}\t{:.0}\t128\t128",
        elapsed.as_nanos(),
        f64::from(ITERATIONS) / elapsed.as_secs_f64(),
    );
}

#[test]
fn portable_minimum_and_representative_layouts() -> Result<(), AdmissionError> {
    assert_eq!(16, empty_object_layout()?.block_bytes);
    assert_eq!(40, array_layout(ValueWidth::Char, 9)?.block_bytes);
    let references = array_layout(ValueWidth::Ref, 3)?;
    assert_eq!(4, references.element_bytes);
    assert_eq!(32, references.block_bytes);
    assert_eq!(32, string_layout(StringEncoding::Latin1, 8)?.block_bytes);
    assert_eq!(40, string_layout(StringEncoding::Utf16, 8)?.block_bytes);
    Ok(())
}

#[test]
fn compact_header_selects_only_representable_heap_and_type_bounds() {
    assert_eq!(
        HeaderFormat::Compact {
            size_bits: 16,
            link_bits: 15
        },
        HeaderFormat::select(256 * 1024, 2),
    );
    assert_eq!(
        HeaderFormat::Compact {
            size_bits: 22,
            link_bits: 21
        },
        HeaderFormat::select(16 * 1024 * 1024, 2),
    );
    assert_eq!(
        HeaderFormat::Legacy,
        HeaderFormat::select(16 * 1024 * 1024, 262_145)
    );
}

#[test]
fn compact_two_int_records_use_16_bytes_and_keep_type_payload_and_identity(
) -> Result<(), AdmissionError> {
    let format = HeaderFormat::select(256 * 1024, 2);
    let fields = [
        FieldSpec {
            field: 0,
            width: ValueWidth::I32,
        },
        FieldSpec {
            field: 1,
            width: ValueWidth::I32,
        },
    ];
    let layout = object_layout_with_header(None, &fields, format)?;
    assert_eq!(16, layout.block_bytes);
    let mut heap = Heap::new(&allocator_plan(256 * 1024).with_header_format(format))?;
    let before = heap.total_free_bytes();
    let mut references = Vec::new();
    for value in 0_u32..4096 {
        let type_id = if value == 0 { (1 << 30) - 1 } else { 7 };
        let reservation = heap
            .reserve(AllocationRequest {
                block_bytes: layout.block_bytes,
                type_id,
            })
            .unwrap()
            .unwrap();
        heap.write_reserved_u32(reservation, 0, value).unwrap();
        heap.write_reserved_u32(reservation, 4, !value).unwrap();
        references.push(heap.commit(reservation).unwrap());
    }
    assert_eq!(64 * 1024, before - heap.total_free_bytes());
    for (value, reference) in references.iter().copied().enumerate() {
        let expected_type = if value == 0 { (1 << 30) - 1 } else { 7 };
        assert_eq!(Some(expected_type), heap.runtime_type(reference));
        assert_eq!(
            (value as u32).to_le_bytes(),
            heap.read_payload(reference, 0, 4).unwrap()[..4]
        );
        assert_eq!(
            (!(value as u32)).to_le_bytes(),
            heap.read_payload(reference, 4, 4).unwrap()[..4]
        );
        if value != 0 {
            assert_eq!(16, reference.payload() - references[value - 1].payload());
        }
    }
    let first = references[0];
    let mut head = None;
    let mut tail = None;
    heap.enqueue_gray(first, 0, &mut head, &mut tail).unwrap();
    assert_eq!(
        Some((first, (1 << 30) - 1)),
        heap.dequeue_gray(&mut head, &mut tail).unwrap()
    );
    assert_eq!(Some((1 << 30) - 1), heap.runtime_type(first));
    assert_eq!(
        0_u32.to_le_bytes(),
        heap.read_payload(first, 0, 4).unwrap()[..4]
    );
    for index in (0..references.len()).step_by(2) {
        assert!(heap.free(references[index]).unwrap());
    }
    for index in (1..references.len()).step_by(2) {
        assert!(heap.free(references[index]).unwrap());
    }
    assert_eq!(before, heap.total_free_bytes());
    assert_eq!(before, heap.diagnostic().largest_free_block);
    Ok(())
}

#[test]
fn compact_records_keep_independent_mutations_and_shared_aliases() -> Result<(), AdmissionError> {
    let fields: Vec<_> = (0..3)
        .map(|field| FieldSpec {
            field,
            width: ValueWidth::I32,
        })
        .collect();
    let layout = object_layout(None, &fields)?;
    let mut heap = Heap::new(&allocator_plan(48))?;
    let mut records = Vec::new();
    for _ in 0..2 {
        let reservation = heap
            .reserve(AllocationRequest {
                block_bytes: layout.block_bytes,
                type_id: 7,
            })
            .unwrap()
            .expect("both three-Int records fit in 48 bytes");
        for field in &layout.fields {
            heap.write_reserved_u32(reservation, field.offset, 100 + field.field)
                .unwrap();
        }
        records.push(heap.commit(reservation).unwrap());
    }
    let before = records[0];
    let after = records[1];
    let alias = before;
    assert_ne!(before, after);
    assert_eq!(0, heap.diagnostic().total_free);
    super::heap_ops::store_value(
        &mut heap,
        before,
        0,
        ValueWidth::I32,
        RuntimeValue::I32(200),
    )
    .unwrap();
    super::heap_ops::store_value(&mut heap, after, 8, ValueWidth::I32, RuntimeValue::I32(300))
        .unwrap();
    for (reference, expected) in [(alias, [200, 101, 102]), (after, [100, 101, 300])] {
        for (field, value) in layout.fields.iter().zip(expected) {
            assert_eq!(
                Ok(RuntimeValue::I32(value)),
                super::heap_ops::load_value(&heap, reference, field.offset, field.width)
            );
        }
    }
    assert_eq!(Some(7), heap.runtime_type(before));
    assert_eq!(Some(7), heap.runtime_type(after));
    Ok(())
}

#[test]
fn one_int_records_fit_in_two_16_byte_blocks_and_preserve_aliases() -> Result<(), AdmissionError> {
    let layout = object_layout(
        None,
        &[FieldSpec {
            field: 0,
            width: ValueWidth::I32,
        }],
    )?;
    let mut heap = Heap::new(&allocator_plan(32))?;
    let mut records = Vec::new();
    for value in [100, 200] {
        let reservation = heap
            .reserve(AllocationRequest {
                block_bytes: layout.block_bytes,
                type_id: 7,
            })
            .unwrap()
            .expect("two one-Int records fit in 32 bytes");
        heap.write_reserved_u32(reservation, 0, value).unwrap();
        records.push(heap.commit(reservation).unwrap());
    }
    let alias = records[0];
    assert_ne!(records[0], records[1]);
    super::heap_ops::store_value(&mut heap, alias, 0, ValueWidth::I32, RuntimeValue::I32(300))
        .unwrap();
    for (reference, expected) in [(records[0], 300), (records[1], 200)] {
        assert_eq!(
            Ok(RuntimeValue::I32(expected)),
            super::heap_ops::load_value(&heap, reference, 0, ValueWidth::I32)
        );
        assert_eq!(Some(7), heap.runtime_type(reference));
    }
    assert_eq!(0, heap.diagnostic().total_free);
    assert!(heap.free(records[1]).unwrap());
    assert!(heap.free(records[0]).unwrap());
    assert_eq!(32, heap.diagnostic().largest_free_block);
    Ok(())
}

#[test]
fn compact_header_supports_wide_fields_across_arena_units() -> Result<(), AdmissionError> {
    let layout = object_layout(
        None,
        &[FieldSpec {
            field: 0,
            width: ValueWidth::I64,
        }],
    )?;
    let mut heap = Heap::new(&allocator_plan(48))?;
    for value in [i64::MIN + 123, i64::MAX - 456] {
        let reservation = heap
            .reserve(AllocationRequest {
                block_bytes: layout.block_bytes,
                type_id: 9,
            })
            .unwrap()
            .unwrap();
        heap.zero_reserved_payload(reservation, 0, layout.payload_bytes)
            .unwrap();
        let reference = heap.commit(reservation).unwrap();
        super::heap_ops::store_value(
            &mut heap,
            reference,
            0,
            ValueWidth::I64,
            RuntimeValue::I64(value),
        )
        .unwrap();
        assert_eq!(
            Ok(RuntimeValue::I64(value)),
            super::heap_ops::load_value(&heap, reference, 0, ValueWidth::I64)
        );
        assert_eq!(Some(9), heap.runtime_type(reference));
    }
    Ok(())
}

#[test]
fn portable_object_fields_use_stable_natural_alignment_groups() -> Result<(), AdmissionError> {
    let layout = object_layout(
        None,
        &[
            FieldSpec {
                field: 0,
                width: ValueWidth::Bool,
            },
            FieldSpec {
                field: 1,
                width: ValueWidth::Char,
            },
            FieldSpec {
                field: 2,
                width: ValueWidth::I64,
            },
            FieldSpec {
                field: 3,
                width: ValueWidth::Ref,
            },
        ],
    )?;

    let offsets: Vec<_> = layout
        .fields
        .iter()
        .map(|field| (field.field, field.offset))
        .collect();
    assert_eq!(vec![(2, 0), (3, 8), (1, 12), (0, 14)], offsets);
    assert_eq!(&[8], layout.reference_offsets.as_ref());
    assert_eq!(15, layout.payload_bytes);
    assert_eq!(32, layout.block_bytes);
    Ok(())
}

#[test]
fn portable_subclass_preserves_the_superclass_prefix() -> Result<(), AdmissionError> {
    let superclass = object_layout(
        None,
        &[
            FieldSpec {
                field: 10,
                width: ValueWidth::Char,
            },
            FieldSpec {
                field: 11,
                width: ValueWidth::Bool,
            },
        ],
    )?;
    let subclass = object_layout(
        Some(&superclass),
        &[
            FieldSpec {
                field: 20,
                width: ValueWidth::I64,
            },
            FieldSpec {
                field: 21,
                width: ValueWidth::Ref,
            },
        ],
    )?;

    assert_eq!(&superclass.fields[..], &subclass.fields[..2]);
    assert_eq!(8, subclass.fields[2].offset);
    assert_eq!(16, subclass.fields[3].offset);
    assert_eq!(&[16], subclass.reference_offsets.as_ref());
    assert_eq!(20, subclass.payload_bytes);
    assert_eq!(32, subclass.block_bytes);

    Ok(())
}

#[test]
fn portable_literal_deduplication_uses_exact_raw_bytes() -> Result<(), AdmissionError> {
    let bytes = b"a\0b\0a\0b\0c\0";
    let ranges = [
        ByteRange { start: 0, end: 4 },
        ByteRange { start: 4, end: 8 },
        ByteRange { start: 8, end: 10 },
    ];
    let (literals, ids) = deduplicate_literal_ranges(bytes, &ranges)?;

    assert_eq!(2, literals.len());
    assert_eq!(&[0, 0, 1], ids.as_ref());
    assert_eq!(2, literals[0].code_units);
    assert_eq!(1, literals[1].code_units);
    Ok(())
}

#[test]
fn portable_block_alignment_covers_eight_byte_edges() -> Result<(), AdmissionError> {
    for (field_count, expected) in [
        (3, 16),
        (4, 16),
        (5, 24),
        (11, 24),
        (12, 24),
        (13, 32),
        (19, 32),
        (20, 32),
        (21, 40),
    ] {
        let fields: Vec<_> = (0..field_count)
            .map(|field| FieldSpec {
                field,
                width: ValueWidth::Bool,
            })
            .collect();
        assert_eq!(expected, object_layout(None, &fields)?.block_bytes);
    }
    Ok(())
}

#[test]
fn portable_lengths_are_checked() {
    assert_eq!(
        Err(AdmissionError::StoragePlanOverflow),
        array_layout(ValueWidth::I64, -1)
    );
    assert_eq!(
        Err(AdmissionError::StoragePlanOverflow),
        array_layout(ValueWidth::I64, i32::MAX)
    );
    assert_eq!(
        Err(AdmissionError::StoragePlanOverflow),
        string_layout(StringEncoding::Utf16, u32::MAX)
    );
}

#[test]
fn portable_admission_publishes_exact_layout_metadata() -> Result<(), AdmissionError> {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 1024;
    let image = ExecutionImage::admit(fixtures::portable_layout_artifact(), profile)?;

    let plan = image.storage_plan();
    assert_eq!(1024, plan.heap_arena_bytes);
    assert_eq!(Heap::allocator_resident_bytes(), plan.heap_allocator_bytes);
    assert_eq!(0, plan.frame_arena_bytes);
    assert_eq!(
        core::mem::size_of::<Frame>() as u64
            * image.maximum_call_depth() as u64
            * (image.maximum_coroutines() as u64 + 1),
        plan.frame_record_bytes,
    );
    assert_eq!(
        TaskScheduler::resident_bytes(image.maximum_coroutines() as u64).unwrap(),
        plan.task_scheduler_bytes,
    );
    assert_eq!(0, plan.channel_bytes);
    assert_eq!(8, plan.static_bytes);
    assert_eq!(
        core::mem::size_of::<TypeInitializationState>() as u64 * 4,
        plan.type_initialization_bytes,
    );
    assert_eq!(
        ExternalRootTable::resident_bytes(image.external_root_capacity()).unwrap(),
        plan.external_root_bytes,
    );
    assert_eq!(Machine::pending_state_bytes(), plan.pending_state_bytes);
    assert_eq!(Machine::fixed_state_bytes(), plan.machine_fixed_bytes);
    assert_eq!(
        plan.heap_arena_bytes
            + plan.heap_allocator_bytes
            + plan.frame_arena_bytes
            + plan.frame_record_bytes
            + plan.task_scheduler_bytes
            + plan.channel_bytes
            + plan.static_bytes
            + plan.type_initialization_bytes
            + plan.external_root_bytes
            + plan.pending_state_bytes
            + plan.machine_fixed_bytes,
        plan.mutable_resident_bytes(),
    );
    let machine = Machine::new(image.clone())?;
    assert_eq!(
        plan.mutable_resident_bytes(),
        machine.test_reserved_bytes() as u64
    );

    let RuntimeTypeLayout::Object(subclass) = image
        .type_layout(TypeKey { module: 0, ty: 2 })
        .expect("subclass layout")
    else {
        panic!("subclass must have an object layout");
    };
    let offsets: Vec<_> = subclass
        .fields
        .iter()
        .map(|field| (field.field, field.offset))
        .collect();
    assert_eq!(vec![(0, 0), (1, 8), (3, 12), (2, 16)], offsets);
    assert_eq!(&[12], subclass.reference_offsets.as_ref());
    assert_eq!(32, subclass.block_bytes);

    let static_field = image.field(4).expect("resolved static field");
    assert_eq!(None, static_field.offset);
    assert_eq!(Some(0), static_field.static_slot);

    let literal = *image.literal(0).expect("resolved literal");
    assert_eq!(2, literal.code_units);
    assert_eq!(4, image.literal_bytes(literal).len());

    assert_eq!(
        Some(&RuntimeTypeLayout::Array {
            element: ValueWidth::Char,
        }),
        image.type_layout(TypeKey { module: 0, ty: 3 })
    );
    Ok(())
}

#[test]
fn call_depth_only_grows_compact_frame_storage() -> Result<(), AdmissionError> {
    let shallow =
        ExecutionImage::admit(fixtures::recursive_artifact(1), fixtures::profile())?.storage_plan();
    let deep =
        ExecutionImage::admit(fixtures::recursive_artifact(4), fixtures::profile())?.storage_plan();

    assert_eq!(shallow.heap_arena_bytes, deep.heap_arena_bytes);
    assert_eq!(shallow.heap_allocator_bytes, deep.heap_allocator_bytes);
    assert_eq!(shallow.static_bytes, deep.static_bytes);
    assert_eq!(
        shallow.type_initialization_bytes,
        deep.type_initialization_bytes,
    );
    assert_eq!(shallow.external_root_bytes, deep.external_root_bytes);
    assert_eq!(shallow.pending_state_bytes, deep.pending_state_bytes);
    assert_eq!(shallow.machine_fixed_bytes, deep.machine_fixed_bytes);
    assert_eq!(shallow.frame_arena_bytes * 4, deep.frame_arena_bytes);
    assert_eq!(shallow.frame_record_bytes * 4, deep.frame_record_bytes);
    Ok(())
}

#[test]
fn portable_admission_rejects_unaligned_or_too_small_heap() {
    for heap_bytes in [0, 16, 31, 33, 47] {
        let mut profile = fixtures::profile();
        profile.heap_bytes = heap_bytes;
        assert_eq!(
            Err(AdmissionError::InvalidHeapSize {
                supplied: heap_bytes,
            }),
            ExecutionImage::admit(fixtures::scalar_artifact(), profile).map(|_| ())
        );
    }
}

#[test]
fn allocation_object_opcode_is_admitted() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 64;
    assert!(ExecutionImage::admit(fixtures::object_allocation_artifact(0), profile).is_ok());
}

#[test]
fn allocation_resumes_without_recharging_or_publishing_a_prefix() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 128;
    let image = ExecutionImage::admit(fixtures::object_allocation_artifact(5), profile).unwrap();
    let mut machine = Machine::new(image).unwrap();
    machine.start(&[]).unwrap();

    assert_eq!(
        Outcome::SliceExhausted,
        machine.run_slice_with_retirement_limit(5, 0, 1).unwrap()
    );
    assert_eq!(0, machine.retired_instructions());
    assert_eq!(5, machine.consumed_fixed_cost());
    assert_eq!(0, machine.consumed_dynamic_cost());
    assert_eq!(None, machine.test_register(0));
    assert_eq!(0, machine.test_pending_initialized_bytes());

    assert_eq!(
        Outcome::SliceExhausted,
        machine.run_slice_with_retirement_limit(1, 0, 1).unwrap()
    );
    assert_eq!(0, machine.retired_instructions());
    assert_eq!(16, machine.test_pending_initialized_bytes());
    let Outcome::Halted(Some(RuntimeValue::Reference(reference))) =
        machine.run_slice(1, 0).unwrap()
    else {
        panic!("allocation must publish and return its reference atomically");
    };
    assert_eq!(5, machine.consumed_fixed_cost());
    assert_eq!(2, machine.consumed_dynamic_cost());
    assert_eq!(1, machine.test_heap_diagnostic().live_handles);
    assert_eq!(
        machine.executed_instructions(),
        machine.retired_instructions()
    );
    assert!(machine
        .test_managed_payload(reference)
        .unwrap()
        .iter()
        .all(|byte| *byte == 0));
}

#[test]
fn allocation_negative_array_length_traps_before_heap_mutation() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 64;
    let image = ExecutionImage::admit(fixtures::array_allocation_artifact(-1), profile).unwrap();
    let mut machine = Machine::new(image).unwrap();
    machine.start(&[]).unwrap();

    assert_eq!(Outcome::SliceExhausted, machine.run_slice(5, 0).unwrap());
    assert_eq!(
        Outcome::Crashed(GuestTrap::NegativeArraySize),
        machine.run_slice(5, 0).unwrap()
    );
    assert_eq!(64, machine.test_heap_diagnostic().total_free);
    assert_eq!(0, machine.test_heap_diagnostic().live_handles);
}

#[test]
fn allocation_oversized_request_reports_immediate_exhaustion() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 64;
    let image = ExecutionImage::admit(fixtures::array_allocation_artifact(100), profile).unwrap();
    let mut machine = Machine::new(image).unwrap();
    machine.start(&[]).unwrap();

    assert_eq!(Outcome::SliceExhausted, machine.run_slice(5, 0).unwrap());
    assert_eq!(
        Outcome::AllocationExhausted(AllocationExhaustion {
            exception: super::value::Ref32::reserved(0).unwrap(),
            diagnostic: super::error::AllocationDiagnostic {
                request_kind: super::error::AllocationRequestKind::Array,
                requested: 108,
                live: 0,
                total_free: 64,
                largest_free_block: 64,
                source: super::error::AllocationSource {
                    module: 0,
                    function: 0,
                    block: 1,
                    instruction: 0,
                },
            },
            collection_attempted: false,
        }),
        machine.run_slice(5, 0).unwrap()
    );
    assert_eq!(64, machine.test_heap_diagnostic().total_free);
}

#[test]
fn allocation_cancellation_rolls_back_private_storage() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 128;
    let image = ExecutionImage::admit(fixtures::object_allocation_artifact(5), profile).unwrap();
    let mut machine = Machine::new(image).unwrap();
    machine.start(&[]).unwrap();

    assert_eq!(Outcome::SliceExhausted, machine.run_slice(5, 0).unwrap());
    assert_eq!(96, machine.test_heap_diagnostic().total_free);
    machine.test_cancel_pending().unwrap();
    assert_eq!(128, machine.test_heap_diagnostic().total_free);
    assert_eq!(None, machine.test_register(0));
}

#[test]
fn allocation_zeroes_minimum_object_padding_without_dynamic_charge() {
    let mut heap = Heap::new(&allocator_plan(32)).unwrap();
    let dirty = heap.reserve(allocator_request(32)).unwrap().unwrap();
    heap.write_reserved_u32(dirty, 0, u32::MAX).unwrap();
    heap.abort(dirty).unwrap();

    let reservation = heap.reserve(allocator_request(32)).unwrap().unwrap();
    let mut pending = PendingAllocation::Object(PendingState {
        request: allocator_request(32),
        reservation,
        destination: 0,
        logical_bytes: 0,
        initialized_bytes: 0,
        fixed_cost_paid: true,
        collection_attempted: false,
    });
    let (used, reference) = pending.advance(&mut heap, 0).unwrap();

    assert_eq!(0, used);
    let payload = heap.test_managed_payload(reference.unwrap()).unwrap();
    assert!(payload.iter().all(|byte| *byte == 0));
}

#[test]
fn heap_instructions_static_opcodes_are_admitted() {
    assert!(
        ExecutionImage::admit(fixtures::static_roundtrip_artifact(), fixtures::profile(),).is_ok()
    );
}

#[test]
fn heap_instructions_statics_are_zeroed_and_isolated_per_instance() {
    let image =
        ExecutionImage::admit(fixtures::static_roundtrip_artifact(), fixtures::profile()).unwrap();
    let run = |write, value| {
        let mut machine = Machine::new(image.clone()).unwrap();
        machine
            .start(&[
                EntryArgument::unowned(RuntimeValue::Bool(write)),
                EntryArgument::unowned(RuntimeValue::I32(value)),
            ])
            .unwrap();
        machine.run_slice(32, 0).unwrap()
    };

    assert_eq!(Outcome::Halted(Some(RuntimeValue::I32(42))), run(true, 42));
    assert_eq!(Outcome::Halted(Some(RuntimeValue::I32(0))), run(false, 99));
}

#[test]
fn heap_instructions_use_inherited_fields_and_interface_closure() {
    let image = ExecutionImage::admit(fixtures::field_roundtrip_artifact(), fixtures::profile())
        .expect("field and type opcodes must be admitted");
    let mut machine = Machine::new(image).unwrap();
    machine.start(&[]).unwrap();

    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::I32(42))),
        machine.run_slice(64, 0).unwrap()
    );
    assert_eq!(Some(RuntimeValue::Bool(true)), machine.test_register(3));
}

#[test]
fn heap_instructions_round_trip_every_primitive_array_width() {
    for (artifact, expected) in fixtures::primitive_array_roundtrip_cases() {
        let image = ExecutionImage::admit(artifact, fixtures::profile())
            .expect("array instructions must be admitted");
        let mut machine = Machine::new(image).unwrap();
        machine.start(&[]).unwrap();
        assert_eq!(
            Outcome::Halted(Some(expected)),
            machine.run_slice(64, 0).unwrap()
        );
        assert_eq!(Some(RuntimeValue::I32(1)), machine.test_register(5));
    }
}

#[test]
fn heap_instructions_round_trip_reference_arrays() {
    let image = ExecutionImage::admit(
        fixtures::reference_array_roundtrip_artifact(),
        fixtures::profile(),
    )
    .unwrap();
    let mut machine = Machine::new(image).unwrap();
    machine.start(&[]).unwrap();
    let Outcome::Halted(Some(RuntimeValue::Reference(returned))) =
        machine.run_slice(64, 0).unwrap()
    else {
        panic!("reference array must return its stored reference")
    };
    let RuntimeValue::Reference(source) = machine.test_register(2).unwrap() else {
        unreachable!()
    };
    assert_eq!(source, returned);
}

#[test]
fn heap_instructions_bounds_fail_before_destination_publication() {
    let image =
        ExecutionImage::admit(fixtures::array_bounds_artifact(), fixtures::profile()).unwrap();
    for index in [-1, 1] {
        let mut machine = Machine::new(image.clone()).unwrap();
        machine
            .start(&[EntryArgument::unowned(RuntimeValue::I32(index))])
            .unwrap();
        assert_eq!(
            Outcome::Crashed(GuestTrap::IndexOutOfBounds),
            machine.run_slice(64, 0).unwrap()
        );
        assert_eq!(Some(RuntimeValue::I32(99)), machine.test_register(2));
    }
}

#[test]
fn heap_instructions_nonnull_zero_reference_traps_without_publication() {
    let image = ExecutionImage::admit(fixtures::nonnull_zero_field_artifact(), fixtures::profile())
        .unwrap();
    let mut machine = Machine::new(image).unwrap();
    machine.start(&[]).unwrap();
    assert_eq!(
        Outcome::Crashed(GuestTrap::NullReference),
        machine.run_slice(64, 0).unwrap()
    );
    assert_eq!(None, machine.test_register(1));
}

#[test]
fn heap_instructions_checked_cast_handles_nullability_and_incompatibility() {
    let nullable =
        ExecutionImage::admit(fixtures::nullable_cast_artifact(true), fixtures::profile()).unwrap();
    let mut machine = Machine::new(nullable).unwrap();
    machine
        .start(&[EntryArgument::unowned(RuntimeValue::Null)])
        .unwrap();
    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::Null)),
        machine.run_slice(32, 0).unwrap()
    );

    let nonnull =
        ExecutionImage::admit(fixtures::nullable_cast_artifact(false), fixtures::profile())
            .unwrap();
    let mut machine = Machine::new(nonnull).unwrap();
    machine
        .start(&[EntryArgument::unowned(RuntimeValue::Null)])
        .unwrap();
    assert_eq!(
        Outcome::Crashed(GuestTrap::NullReference),
        machine.run_slice(32, 0).unwrap()
    );
    assert_eq!(None, machine.test_register(1));

    let incompatible =
        ExecutionImage::admit(fixtures::incompatible_cast_artifact(), fixtures::profile()).unwrap();
    let mut machine = Machine::new(incompatible).unwrap();
    machine.start(&[]).unwrap();
    assert_eq!(
        Outcome::Crashed(GuestTrap::ClassCast),
        machine.run_slice(32, 0).unwrap()
    );
    assert_eq!(None, machine.test_register(1));
}

#[test]
fn heap_instructions_round_trip_reference_fields() {
    let image = ExecutionImage::admit(
        fixtures::reference_field_roundtrip_artifact(),
        fixtures::profile(),
    )
    .unwrap();
    let mut machine = Machine::new(image).unwrap();
    machine.start(&[]).unwrap();
    let Outcome::Halted(Some(RuntimeValue::Reference(returned))) =
        machine.run_slice(64, 0).unwrap()
    else {
        panic!("reference field must return its stored reference")
    };
    let RuntimeValue::Reference(source) = machine.test_register(1).unwrap() else {
        unreachable!()
    };
    assert_eq!(source, returned);
}

#[test]
fn heap_instructions_failed_array_store_is_atomic() {
    let image = ExecutionImage::admit(fixtures::failed_array_store_artifact(), fixtures::profile())
        .unwrap();
    for index in [-1, 1] {
        let mut machine = Machine::new(image.clone()).unwrap();
        machine
            .start(&[EntryArgument::unowned(RuntimeValue::I32(index))])
            .unwrap();
        assert_eq!(
            Outcome::Crashed(GuestTrap::IndexOutOfBounds),
            machine.run_slice(64, 0).unwrap()
        );
        let RuntimeValue::Reference(array) = machine.test_register(2).unwrap() else {
            unreachable!()
        };
        let payload = machine.test_managed_payload(array).unwrap();
        assert_eq!([0, 0, 0, 0], payload[8..12]);
    }
}

#[test]
fn heap_instructions_stale_managed_handles_fault() {
    let mut heap = Heap::new(&allocator_plan(32)).unwrap();
    let reservation = heap.reserve(allocator_request(32)).unwrap().unwrap();
    let reference = heap.commit(reservation).unwrap();
    assert!(heap.free(reference).unwrap());
    assert_eq!(
        Err(VmFault::InvalidReference),
        super::heap_ops::load_value(&heap, reference, 0, ValueWidth::I32)
    );
}

#[test]
fn heap_instructions_is_type_returns_false_for_null() {
    let image =
        ExecutionImage::admit(fixtures::null_is_type_artifact(), fixtures::profile()).unwrap();
    let mut machine = Machine::new(image).unwrap();
    machine.start(&[]).unwrap();
    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::Bool(false))),
        machine.run_slice(32, 0).unwrap()
    );
}

#[test]
fn gray_queue_restores_predecessors_for_repeated_sweep_and_reuse() {
    let mut heap = Heap::new(&allocator_plan(512)).unwrap();
    for cycle in 0..32_u32 {
        let mut references = Vec::new();
        for (index, size) in [16, 40, 32, 16, 48, 16, 40, 32].into_iter().enumerate() {
            let reservation = heap
                .reserve(AllocationRequest {
                    block_bytes: size,
                    type_id: 100 + index as u32,
                })
                .unwrap()
                .unwrap();
            heap.write_reserved(reservation, 0, &(cycle * 100 + index as u32).to_le_bytes())
                .unwrap();
            let reference = heap.commit(reservation).unwrap();
            references.push(reference);
        }
        assert!(references
            .iter()
            .any(|reference| reference.payload() % 16 == 8));
        // Mix existing free blocks with newly dead blocks around surviving objects.
        assert!(heap.free(references[2]).unwrap());
        assert!(heap.free(references[4]).unwrap());
        let mut head = None;
        let mut tail = None;
        for index in [5, 1, 7, 0] {
            heap.enqueue_gray(references[index], 1, &mut head, &mut tail)
                .unwrap();
        }
        for index in [5, 1, 7, 0] {
            assert_eq!(
                Some((references[index], 100 + index as u32)),
                heap.dequeue_gray(&mut head, &mut tail).unwrap()
            );
        }
        assert_eq!(None, heap.dequeue_gray(&mut head, &mut tail).unwrap());
        let mut offset = 0;
        let mut previous_size = 0;
        let mut steps = 0;
        while offset < heap.arena_bytes() {
            (offset, previous_size) = heap.sweep_block(offset, previous_size).unwrap();
            steps += 1;
            assert!(steps <= 9);
        }
        assert_eq!(512, offset);
        for index in [0, 1, 5, 7] {
            assert_eq!(
                Some(100 + index as u32),
                heap.runtime_type(references[index])
            );
            let payload = heap.read_payload(references[index], 0, 4).unwrap();
            assert_eq!((cycle * 100 + index as u32).to_le_bytes(), payload[..4]);
        }
        for index in [2, 3, 4, 6] {
            assert_eq!(None, heap.runtime_type(references[index]));
        }
        // Free in a different order to require restored backward-coalescing metadata.
        for index in [7, 1, 5, 0] {
            assert!(heap.free(references[index]).unwrap());
        }
        assert_eq!(512, heap.diagnostic().total_free);
        assert_eq!(512, heap.diagnostic().largest_free_block);
        let whole_arena = heap.reserve(allocator_request(512)).unwrap().unwrap();
        heap.abort(whole_arena).unwrap();
    }
}
