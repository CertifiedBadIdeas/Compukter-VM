use super::{
    error::{AdmissionError, AllocationExhaustion, GuestTrap, Outcome, VmFault},
    fixtures,
    heap::{
        free_size_class, request_size_class, splitmix64, AllocationRequest, BlockOffset, Heap,
        ManagedObjectHeader, SizeClass,
    },
    heap_ops::{PendingAllocation, PendingState},
    image::{deduplicate_literal_ranges, ExecutionImage},
    layout::{
        array_layout, empty_object_layout, object_layout, string_layout, FieldSpec,
        RuntimeTypeLayout, StoragePlan, StringEncoding, ValueWidth,
    },
    machine::Machine,
    value::{EntryArgument, RuntimeValue},
    TypeKey,
};
use crate::artifact::ByteRange;

#[test]
fn managed_object_header_is_eight_bytes() {
    assert_eq!(8, core::mem::size_of::<ManagedObjectHeader>());
}

#[test]
fn allocator_size_classes_map_free_blocks_down_and_requests_up() {
    let class = |first, second| Some(SizeClass { first, second });
    for (size, expected) in [
        (32, class(5, 0)),
        (48, class(5, 1)),
        (64, class(6, 0)),
        (112, class(6, 3)),
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
    assert_eq!(None, free_size_class(16));
    assert_eq!(None, free_size_class(33));
}

fn allocator_plan(heap_bytes: u32) -> StoragePlan {
    StoragePlan {
        heap_bytes,
        handle_capacity: heap_bytes / 32,
        ..StoragePlan::default()
    }
}

fn allocator_request(block_bytes: u32) -> AllocationRequest {
    AllocationRequest {
        block_bytes,
        type_id: 1,
    }
}

#[test]
fn allocator_splits_exactly_and_absorbs_a_16_byte_tail() -> Result<(), AdmissionError> {
    let mut heap = Heap::new(&allocator_plan(128))?;
    let first = heap.reserve(allocator_request(48)).unwrap().unwrap();
    assert_eq!(BlockOffset(0), first.block);
    assert_eq!(80, heap.diagnostic().total_free);

    let second = heap.reserve(allocator_request(64)).unwrap().unwrap();
    assert_eq!(BlockOffset(48), second.block);
    assert_eq!(0, heap.diagnostic().total_free);
    assert!(heap.reserve(allocator_request(32)).unwrap().is_none());
    Ok(())
}

#[test]
fn allocator_uses_conservative_upward_search_and_lifo_free_lists() -> Result<(), AdmissionError> {
    let mut fragmented = Heap::new(&allocator_plan(272))?;
    assert!(fragmented
        .reserve(allocator_request(272))
        .unwrap()
        .is_none());
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
    assert_eq!(16, first.payload());
    assert_eq!(Some(1), heap.runtime_type(first));
    assert_eq!(Some(splitmix64(1) as u32), heap.identity_hash(first));
    assert!(heap.free(first).unwrap());
    assert_eq!(None, heap.runtime_type(first));

    let next = heap.reserve(allocator_request(32)).unwrap().unwrap();
    assert_eq!(BlockOffset(0), next.block);
    let next = heap.commit(next).unwrap();
    assert_eq!(first, next);
    assert_eq!(Some(splitmix64(2) as u32), heap.identity_hash(next));
    Ok(())
}

#[test]
fn identity_tokens_wrap_deterministically_and_allow_collisions() -> Result<(), AdmissionError> {
    let mut heap = Heap::new(&allocator_plan(32))?;
    heap.test_set_next_ordinal(u64::MAX);
    let before_wrap = heap.reserve(allocator_request(32)).unwrap().unwrap();
    let before_wrap = heap.commit(before_wrap).unwrap();
    assert_eq!(
        Some(splitmix64(u64::MAX) as u32),
        heap.identity_hash(before_wrap)
    );
    assert!(heap.free(before_wrap).unwrap());

    let after_wrap = heap.reserve(allocator_request(32)).unwrap().unwrap();
    let after_wrap = heap.commit(after_wrap).unwrap();
    assert_eq!(Some(splitmix64(0) as u32), heap.identity_hash(after_wrap));
    Ok(())
}

#[test]
fn allocator_has_no_managed_handle_capacity() -> Result<(), AdmissionError> {
    let mut plan = allocator_plan(64);
    plan.handle_capacity = 0;
    let mut heap = Heap::new(&plan)?;
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
    assert_eq!(32, empty_object_layout()?.block_bytes);
    assert_eq!(64, array_layout(ValueWidth::Char, 9)?.block_bytes);
    assert_eq!(48, array_layout(ValueWidth::Ref, 3)?.block_bytes);
    assert_eq!(48, string_layout(StringEncoding::Latin1, 8)?.block_bytes);
    assert_eq!(48, string_layout(StringEncoding::Utf16, 8)?.block_bytes);
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
    assert_eq!(48, layout.block_bytes);
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
    assert_eq!(48, subclass.block_bytes);

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
fn portable_block_alignment_covers_15_16_17_byte_edges() -> Result<(), AdmissionError> {
    for (field_count, expected) in [(15, 48), (16, 48), (17, 48)] {
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

    assert_eq!(
        StoragePlan {
            heap_bytes: 1024,
            handle_capacity: 32,
            type_count: 4,
            field_count: 5,
            static_slot_count: 1,
            literal_count: 1,
            literal_id_count: 1,
            reference_offset_count: 1,
        },
        image.storage_plan()
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
    assert_eq!(48, subclass.block_bytes);

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

    assert_eq!(Outcome::SliceExhausted, machine.run_slice(5, 0).unwrap());
    assert_eq!(5, machine.consumed_fixed_cost());
    assert_eq!(0, machine.consumed_dynamic_cost());
    assert_eq!(None, machine.test_register(0));
    assert_eq!(0, machine.test_pending_initialized_bytes());

    assert_eq!(Outcome::SliceExhausted, machine.run_slice(1, 0).unwrap());
    assert_eq!(16, machine.test_pending_initialized_bytes());
    let Outcome::Halted(Some(RuntimeValue::Reference(reference))) =
        machine.run_slice(1, 0).unwrap()
    else {
        panic!("allocation must publish and return its reference atomically");
    };
    assert_eq!(5, machine.consumed_fixed_cost());
    assert_eq!(2, machine.consumed_dynamic_cost());
    assert_eq!(1, machine.test_heap_diagnostic().live_handles);
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
    assert_eq!(80, machine.test_heap_diagnostic().total_free);
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
