use super::{
    error::{AdmissionError, AllocationExhaustion, GuestTrap, Outcome, VmFault},
    fixtures,
    heap::{
        free_size_class, request_size_class, splitmix64, AllocationRequest, BlockOffset, Heap,
        SizeClass,
    },
    heap_ops::{PendingAllocation, PendingState},
    image::{deduplicate_literal_ranges, ExecutionImage},
    layout::{
        array_layout, empty_object_layout, object_layout, string_layout, FieldSpec,
        RuntimeTypeLayout, StoragePlan, StringEncoding, ValueWidth,
    },
    machine::Machine,
    value::RuntimeValue,
    TypeKey,
};
use crate::artifact::ByteRange;

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
        ty: TypeKey { module: 0, ty: 1 },
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
fn allocator_handles_commit_abort_generation_and_retirement() -> Result<(), AdmissionError> {
    let mut heap = Heap::new(&allocator_plan(64))?;
    let aborted = heap.reserve(allocator_request(32)).unwrap().unwrap();
    heap.abort(aborted).unwrap();
    let reused = heap.reserve(allocator_request(32)).unwrap().unwrap();
    assert_eq!(aborted.slot, reused.slot);
    assert_eq!(aborted.generation, reused.generation);

    let first = heap.commit(reused).unwrap();
    assert_eq!(Some(TypeKey { module: 0, ty: 1 }), heap.runtime_type(first));
    assert_eq!(Some(splitmix64(1) as u32), heap.identity_hash(first));
    assert!(heap.free(first).unwrap());
    assert_eq!(None, heap.runtime_type(first));

    let next = heap.reserve(allocator_request(32)).unwrap().unwrap();
    assert_eq!(first.slot(), next.slot);
    assert_eq!(first.generation() + 1, next.generation);
    heap.abort(next).unwrap();

    heap.test_set_generation(0, u32::MAX);
    let retiring = heap.reserve(allocator_request(32)).unwrap().unwrap();
    let retiring = heap.commit(retiring).unwrap();
    assert!(heap.free(retiring).unwrap());
    assert_eq!(1, heap.diagnostic().retired_handles);
    let survivor = heap.reserve(allocator_request(32)).unwrap().unwrap();
    assert_eq!(1, survivor.slot);
    heap.abort(survivor).unwrap();
    heap.test_set_generation(1, u32::MAX);
    let retiring = heap.reserve(allocator_request(32)).unwrap().unwrap();
    let retiring = heap.commit(retiring).unwrap();
    assert_eq!(Some(splitmix64(3) as u32), heap.identity_hash(retiring));
    assert!(heap.free(retiring).unwrap());
    assert_eq!(2, heap.diagnostic().retired_handles);
    assert_eq!(
        Err(VmFault::HandleExhausted),
        heap.reserve(allocator_request(32))
    );
    Ok(())
}

#[test]
fn allocator_diagnostics_are_bounded_scalars() {
    assert!(core::mem::size_of::<super::heap::HeapDiagnostic>() <= 32);
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
fn portable_minimum_and_representative_layouts() -> Result<(), AdmissionError> {
    assert_eq!(32, empty_object_layout()?.block_bytes);
    assert_eq!(48, array_layout(ValueWidth::Char, 9)?.block_bytes);
    assert_eq!(48, array_layout(ValueWidth::Ref, 3)?.block_bytes);
    assert_eq!(32, string_layout(StringEncoding::Latin1, 8)?.block_bytes);
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
    assert_eq!(vec![(2, 0), (3, 8), (1, 16), (0, 18)], offsets);
    assert_eq!(&[8], layout.reference_offsets.as_ref());
    assert_eq!(19, layout.payload_bytes);
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
    assert_eq!(24, subclass.payload_bytes);
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
    for (field_count, expected) in [(15, 32), (16, 32), (17, 48)] {
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
    assert_eq!(vec![(0, 0), (1, 8), (3, 16), (2, 24)], offsets);
    assert_eq!(&[16], subclass.reference_offsets.as_ref());
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

    assert_eq!(Outcome::SliceExhausted, machine.run_slice(5).unwrap());
    assert_eq!(5, machine.consumed_fixed_cost());
    assert_eq!(0, machine.consumed_dynamic_cost());
    assert_eq!(None, machine.test_register(0));
    assert_eq!(0, machine.test_pending_initialized_bytes());

    assert_eq!(Outcome::SliceExhausted, machine.run_slice(1).unwrap());
    assert_eq!(16, machine.test_pending_initialized_bytes());
    assert_eq!(Outcome::SliceExhausted, machine.run_slice(1).unwrap());
    assert_eq!(32, machine.test_pending_initialized_bytes());
    assert_eq!(5, machine.consumed_fixed_cost());
    assert_eq!(2, machine.consumed_dynamic_cost());
    assert_eq!(None, machine.test_register(0));

    let Outcome::Halted(Some(RuntimeValue::Reference(reference))) = machine.run_slice(1).unwrap()
    else {
        panic!("allocation must publish and return its reference atomically");
    };
    assert_eq!(5, machine.consumed_fixed_cost());
    assert_eq!(3, machine.consumed_dynamic_cost());
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

    assert_eq!(Outcome::SliceExhausted, machine.run_slice(5).unwrap());
    assert_eq!(
        Outcome::Crashed(GuestTrap::NegativeArraySize),
        machine.run_slice(5).unwrap()
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

    assert_eq!(Outcome::SliceExhausted, machine.run_slice(5).unwrap());
    assert_eq!(
        Outcome::AllocationExhausted(AllocationExhaustion {
            requested_block_bytes: 128,
            total_free: 64,
            largest_free_block: 64,
            collection_attempted: false,
        }),
        machine.run_slice(5).unwrap()
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

    assert_eq!(Outcome::SliceExhausted, machine.run_slice(5).unwrap());
    assert_eq!(64, machine.test_heap_diagnostic().total_free);
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
