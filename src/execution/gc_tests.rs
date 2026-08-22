use super::{
    error::{AllocationExhaustion, Outcome, VmFault},
    fixtures,
    gc::{Collector, CollectorAction, CollectorPhase},
    heap::{AllocationRequest, Heap},
    heap_ops::store_value,
    image::ExecutionImage,
    layout::{array_layout, RuntimeTypeLayout, ValueWidth},
    machine::Frame,
    value::{ReferenceDomain, RegisterValue, RuntimeValue},
    TypeKey,
};

fn allocate(heap: &mut Heap, ty: TypeKey, block_bytes: u32) -> super::value::ReferenceValue {
    let reservation = heap
        .reserve(AllocationRequest { block_bytes, ty })
        .unwrap()
        .unwrap();
    heap.commit(reservation).unwrap()
}

fn collect(
    collector: &mut Collector,
    heap: &mut Heap,
    image: &ExecutionImage,
    statics: &[RuntimeValue],
    frames: &[Frame],
    registers: &[RegisterValue],
) -> Vec<CollectorAction> {
    let mut actions = Vec::new();
    collector.start();
    while collector.is_active() {
        assert_eq!(
            1,
            collector
                .step(heap, image, statics, frames, registers, frames.len())
                .unwrap()
        );
        actions.push(collector.test_last_action().unwrap());
    }
    actions
}

#[test]
fn collector_advances_one_bounded_action_and_does_nothing_while_idle() {
    let image = ExecutionImage::admit(
        fixtures::reference_field_roundtrip_artifact(),
        fixtures::profile(),
    )
    .unwrap();
    let mut heap = Heap::new(&image.storage_plan()).unwrap();
    let ty = TypeKey { module: 0, ty: 1 };
    let RuntimeTypeLayout::Object(layout) = image.type_layout(ty).unwrap() else {
        unreachable!()
    };
    let root = allocate(&mut heap, ty, layout.block_bytes);
    let child = allocate(&mut heap, ty, layout.block_bytes);
    let unreachable = allocate(&mut heap, ty, layout.block_bytes);
    store_value(
        &mut heap,
        root,
        layout.reference_offsets[0],
        ValueWidth::Ref,
        RuntimeValue::Reference(child),
    )
    .unwrap();

    let frames = [Frame::test_entry(image.entry_index())];
    let mut registers = vec![RegisterValue::Uninitialized; image.registers_per_frame()];
    registers[0] = RegisterValue::Initialized(RuntimeValue::Reference(root));
    let mut collector = Collector::new();

    assert_eq!(
        0,
        collector
            .step(&mut heap, &image, &[], &frames, &registers, 1)
            .unwrap()
    );
    collector.start();
    assert_eq!(CollectorPhase::Roots, collector.phase());

    while collector.is_active() {
        assert_eq!(
            1,
            collector
                .step(&mut heap, &image, &[], &frames, &registers, 1)
                .unwrap()
        );
    }

    assert!(heap.managed_type(root).is_ok());
    assert!(heap.managed_type(child).is_ok());
    assert!(heap.managed_type(unreachable).is_err());
    assert_eq!(
        0,
        collector
            .step(&mut heap, &image, &[], &frames, &registers, 1)
            .unwrap()
    );
}

#[test]
fn oom_dropped_root_is_collected_and_the_retry_recovers() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 32;
    let image = ExecutionImage::admit(fixtures::gc_retry_artifact(), profile).unwrap();
    let minimum = image.minimum_slice_cost();
    let mut machine = super::machine::Machine::new(image).unwrap();
    machine.start(&[]).unwrap();

    while !machine.test_collector_active() {
        assert_eq!(
            Outcome::SliceExhausted,
            machine.run_slice(minimum, 0).unwrap()
        );
    }
    assert!(machine.test_collector_active());
    let guest_instructions = machine.executed_instructions();

    while machine.test_collector_active() {
        assert_eq!(
            Outcome::SliceExhausted,
            machine.run_slice(minimum, 1).unwrap()
        );
        assert_eq!(guest_instructions, machine.executed_instructions());
    }

    let outcome = machine.run_slice(minimum, 0).unwrap();
    assert_eq!(Outcome::Halted(machine.test_register(0)), outcome);
    assert_eq!(1, machine.test_heap_diagnostic().live_handles);
    assert!(machine.consumed_maintenance_cost() > 0);
}

#[test]
fn machine_reports_one_failed_post_collection_retry() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 32;
    let image = ExecutionImage::admit(fixtures::gc_failed_retry_artifact(), profile).unwrap();
    let minimum = image.minimum_slice_cost();
    let mut machine = super::machine::Machine::new(image).unwrap();
    machine.start(&[]).unwrap();

    while !machine.test_collector_active() {
        assert_eq!(
            Outcome::SliceExhausted,
            machine.run_slice(minimum, 0).unwrap()
        );
    }
    let outcome = loop {
        let outcome = machine.run_slice(minimum, 1).unwrap();
        if outcome.is_terminal() {
            break outcome;
        }
    };
    assert_eq!(
        Outcome::AllocationExhausted(AllocationExhaustion {
            exception: super::value::ReferenceValue::emergency(),
            diagnostic: super::error::AllocationDiagnostic {
                request_kind: super::error::AllocationRequestKind::Object,
                requested: 0,
                live: 32,
                total_free: 0,
                largest_free_block: 0,
                source: super::error::AllocationSource {
                    module: 0,
                    function: 0,
                    block: 1,
                    instruction: 0,
                },
            },
            collection_attempted: true,
        }),
        outcome
    );
    assert_eq!(outcome, machine.run_slice(minimum, 1).unwrap());
}

#[test]
fn oom_delivery_reuses_the_immortal_emergency_identity() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 32;
    let image = ExecutionImage::admit(fixtures::gc_failed_retry_artifact(), profile).unwrap();
    let minimum = image.minimum_slice_cost();
    let mut machine = super::machine::Machine::new(image).unwrap();
    machine.start(&[]).unwrap();

    let outcome = loop {
        let outcome = machine.run_slice(minimum, 1).unwrap();
        if outcome.is_terminal() {
            break outcome;
        }
    };
    let Outcome::AllocationExhausted(exhaustion) = outcome else {
        panic!("allocation did not deliver OOM");
    };
    assert_eq!(ReferenceDomain::Emergency, exhaustion.exception.domain());
    let Outcome::AllocationExhausted(repeated) = machine.run_slice(minimum, 1).unwrap() else {
        panic!("terminal OOM was not stable");
    };
    assert_eq!(exhaustion.exception, repeated.exception);
}

#[test]
fn oom_missing_emergency_state_faults_instead_of_allocating() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 64;
    let image = ExecutionImage::admit(fixtures::array_allocation_artifact(100), profile).unwrap();
    let minimum = image.minimum_slice_cost();
    let mut machine = super::machine::Machine::new(image).unwrap();
    machine.start(&[]).unwrap();
    machine.test_remove_emergency_oom();

    assert_eq!(
        Outcome::SliceExhausted,
        machine.run_slice(minimum, 0).unwrap()
    );
    assert_eq!(
        Outcome::Faulted(VmFault::InvalidStoragePlan),
        machine.run_slice(minimum, 0).unwrap()
    );
    assert_eq!(0, machine.consumed_maintenance_cost());
}

#[test]
fn oom_delivery_allocates_nothing() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 64;
    let image = ExecutionImage::admit(fixtures::array_allocation_artifact(100), profile).unwrap();
    let minimum = image.minimum_slice_cost();
    let mut machine = super::machine::Machine::new(image).unwrap();
    machine.start(&[]).unwrap();
    assert_eq!(
        Outcome::SliceExhausted,
        machine.run_slice(minimum, 0).unwrap()
    );

    super::tests::allocation_counter::reset_and_enable();
    let outcome = machine.run_slice(minimum, 0).unwrap();
    let allocations = super::tests::allocation_counter::disable_and_read();

    let Outcome::AllocationExhausted(exhaustion) = outcome else {
        panic!("oversized request did not deliver OOM");
    };
    assert_eq!(ReferenceDomain::Emergency, exhaustion.exception.domain());
    assert_eq!(0, allocations);
    assert_eq!(0, machine.consumed_maintenance_cost());
}

#[test]
fn oom_diagnostic_records_request_heap_and_source_scalars() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 64;
    let image = ExecutionImage::admit(fixtures::array_allocation_artifact(100), profile).unwrap();
    let minimum = image.minimum_slice_cost();
    let mut machine = super::machine::Machine::new(image).unwrap();
    machine.start(&[]).unwrap();
    assert_eq!(
        Outcome::SliceExhausted,
        machine.run_slice(minimum, 0).unwrap()
    );
    let Outcome::AllocationExhausted(exhaustion) = machine.run_slice(minimum, 0).unwrap() else {
        panic!("oversized array did not deliver OOM");
    };

    assert_eq!(
        super::error::AllocationRequestKind::Array,
        exhaustion.diagnostic.request_kind
    );
    assert_eq!(108, exhaustion.diagnostic.requested);
    assert_eq!(0, exhaustion.diagnostic.live);
    assert_eq!(64, exhaustion.diagnostic.total_free);
    assert_eq!(64, exhaustion.diagnostic.largest_free_block);
    assert_eq!(0, exhaustion.diagnostic.source.module);
    assert_eq!(0, exhaustion.diagnostic.source.function);
    assert_eq!(1, exhaustion.diagnostic.source.block);
    assert_eq!(0, exhaustion.diagnostic.source.instruction);
}

#[test]
fn oom_oversized_string_request_skips_collection_and_reports_its_source() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 64;
    let code_units = [u16::from(b'x'); 25];
    let image = ExecutionImage::admit(
        fixtures::literal_string_concat_units_artifact(&code_units),
        profile,
    )
    .unwrap();
    let minimum = image.minimum_slice_cost();
    let mut machine = super::machine::Machine::new(image).unwrap();
    machine.start(&[]).unwrap();

    let outcome = loop {
        let outcome = machine.run_slice(minimum, 0).unwrap();
        if outcome.is_terminal() {
            break outcome;
        }
    };
    let Outcome::AllocationExhausted(exhaustion) = outcome else {
        panic!("oversized string did not deliver OOM");
    };
    assert_eq!(
        super::error::AllocationDiagnostic {
            request_kind: super::error::AllocationRequestKind::String,
            requested: 58,
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
        exhaustion.diagnostic
    );
    assert!(!exhaustion.collection_attempted);
    assert_eq!(0, machine.consumed_maintenance_cost());
}

#[test]
fn oom_string_allocation_collects_once_then_reports_a_failed_retry() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 32;
    let image = ExecutionImage::admit(fixtures::repeated_concat_artifact(false), profile).unwrap();
    let minimum = image.minimum_slice_cost();
    let mut machine = super::machine::Machine::new(image).unwrap();
    machine.start(&[]).unwrap();

    while !machine.test_collector_active() {
        assert_eq!(
            Outcome::SliceExhausted,
            machine.run_slice(minimum, 0).unwrap()
        );
    }
    let outcome = loop {
        let outcome = machine.run_slice(minimum, 1).unwrap();
        if outcome.is_terminal() {
            break outcome;
        }
    };
    let Outcome::AllocationExhausted(exhaustion) = outcome else {
        panic!("string retry did not deliver OOM");
    };
    assert_eq!(
        super::error::AllocationDiagnostic {
            request_kind: super::error::AllocationRequestKind::String,
            requested: 12,
            live: 32,
            total_free: 0,
            largest_free_block: 0,
            source: super::error::AllocationSource {
                module: 0,
                function: 0,
                block: 2,
                instruction: 0,
            },
        },
        exhaustion.diagnostic
    );
    assert!(exhaustion.collection_attempted);
    assert!(machine.consumed_maintenance_cost() > 0);
}

#[test]
fn oom_capacity_failure_collects_once_and_preserves_free_space_diagnostics() {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 64;
    let image = ExecutionImage::admit(fixtures::object_allocation_artifact(0), profile).unwrap();
    let minimum = image.minimum_slice_cost();
    let mut machine = super::machine::Machine::new(image).unwrap();
    machine.start(&[]).unwrap();
    machine.test_exhaust_handle_capacity();

    let outcome = loop {
        let outcome = machine.run_slice(minimum, 1).unwrap();
        if outcome.is_terminal() {
            break outcome;
        }
    };
    let Outcome::AllocationExhausted(exhaustion) = outcome else {
        panic!("handle-capacity failure did not deliver OOM");
    };
    assert_eq!(
        super::error::AllocationRequestKind::Object,
        exhaustion.diagnostic.request_kind
    );
    assert_eq!(64, exhaustion.diagnostic.total_free);
    assert_eq!(64, exhaustion.diagnostic.largest_free_block);
    assert!(exhaustion.collection_attempted);
}

#[test]
fn oom_fragmentation_survives_full_collection_with_sufficient_total_free() {
    let image = ExecutionImage::admit(
        fixtures::reference_field_roundtrip_artifact(),
        fixtures::profile(),
    )
    .unwrap();
    let mut heap = Heap::new(&super::layout::StoragePlan {
        heap_bytes: 128,
        handle_capacity: 4,
        ..image.storage_plan()
    })
    .unwrap();
    let ty = TypeKey { module: 0, ty: 1 };
    let references = [
        allocate(&mut heap, ty, 32),
        allocate(&mut heap, ty, 32),
        allocate(&mut heap, ty, 32),
        allocate(&mut heap, ty, 32),
    ];
    let frames = [Frame::test_entry(image.entry_index())];
    let mut registers = vec![RegisterValue::Uninitialized; image.registers_per_frame()];
    registers[0] = RegisterValue::Initialized(RuntimeValue::Reference(references[1]));
    registers[1] = RegisterValue::Initialized(RuntimeValue::Reference(references[3]));
    let mut collector = Collector::new();

    collect(&mut collector, &mut heap, &image, &[], &frames, &registers);

    assert_eq!(64, heap.diagnostic().total_free);
    assert_eq!(32, heap.diagnostic().largest_free_block);
    assert!(heap
        .reserve(AllocationRequest {
            block_bytes: 48,
            ty,
        })
        .unwrap()
        .is_none());
}

#[test]
fn collector_handles_cycles_diamonds_duplicate_roots_statics_and_multiple_frames_fifo() {
    let image = ExecutionImage::admit(fixtures::gc_graph_artifact(), fixtures::profile()).unwrap();
    let mut heap = Heap::new(&image.storage_plan()).unwrap();
    let ty = TypeKey { module: 0, ty: 1 };
    let RuntimeTypeLayout::Object(layout) = image.type_layout(ty).unwrap() else {
        unreachable!()
    };
    let [first, second] = layout.reference_offsets.as_ref() else {
        panic!("graph fixture needs two reference fields")
    };
    let root = allocate(&mut heap, ty, layout.block_bytes);
    let left = allocate(&mut heap, ty, layout.block_bytes);
    let right = allocate(&mut heap, ty, layout.block_bytes);
    let shared = allocate(&mut heap, ty, layout.block_bytes);
    let island_a = allocate(&mut heap, ty, layout.block_bytes);
    let island_b = allocate(&mut heap, ty, layout.block_bytes);
    for (owner, offset, value) in [
        (root, *first, left),
        (root, *second, right),
        (left, *first, shared),
        (right, *first, shared),
        (shared, *first, root),
        (island_a, *first, island_b),
        (island_b, *first, island_a),
    ] {
        store_value(
            &mut heap,
            owner,
            offset,
            ValueWidth::Ref,
            RuntimeValue::Reference(value),
        )
        .unwrap();
    }

    let frames = [
        Frame::test_entry(image.entry_index()),
        Frame::test_entry(image.entry_index()),
    ];
    let registers = [
        RegisterValue::Initialized(RuntimeValue::Reference(root)),
        RegisterValue::Initialized(RuntimeValue::Reference(root)),
    ];
    let statics = [RuntimeValue::Reference(shared)];
    let mut collector = Collector::new();
    let actions = collect(
        &mut collector,
        &mut heap,
        &image,
        &statics,
        &frames,
        &registers,
    );

    assert_eq!(
        3,
        actions
            .iter()
            .filter(|action| **action == CollectorAction::Root)
            .count()
    );
    let dequeued: Vec<_> = actions
        .iter()
        .filter_map(|action| match action {
            CollectorAction::Dequeue(slot) => Some(*slot),
            _ => None,
        })
        .collect();
    assert_eq!(
        vec![shared.slot(), root.slot(), left.slot(), right.slot()],
        dequeued
    );
    let swept: Vec<_> = actions
        .iter()
        .filter_map(|action| match action {
            CollectorAction::Sweep(offset) => Some(*offset),
            _ => None,
        })
        .collect();
    assert!(swept.windows(2).all(|pair| pair[0] < pair[1]));
    for live in [root, left, right, shared] {
        assert!(heap.managed_type(live).is_ok());
    }
    for dead in [island_a, island_b] {
        assert!(heap.managed_type(dead).is_err());
    }

    let first_epoch = collector.test_epoch();
    collect(
        &mut collector,
        &mut heap,
        &image,
        &statics,
        &frames,
        &registers,
    );
    assert_ne!(first_epoch, collector.test_epoch());
}

#[test]
fn collector_scans_reference_arrays() {
    let image = ExecutionImage::admit(
        fixtures::reference_array_roundtrip_artifact(),
        fixtures::profile(),
    )
    .unwrap();
    let mut heap = Heap::new(&image.storage_plan()).unwrap();
    let object_ty = TypeKey { module: 0, ty: 1 };
    let RuntimeTypeLayout::Object(object_layout) = image.type_layout(object_ty).unwrap() else {
        unreachable!()
    };
    let array_ty = TypeKey { module: 0, ty: 3 };
    let array_layout = array_layout(ValueWidth::Ref, 2).unwrap();
    let first = allocate(&mut heap, object_ty, object_layout.block_bytes);
    let second = allocate(&mut heap, object_ty, object_layout.block_bytes);
    let unreachable = allocate(&mut heap, object_ty, object_layout.block_bytes);
    let array = allocate(&mut heap, array_ty, array_layout.block_bytes);
    heap.write_payload(array, 0, &2_u32.to_le_bytes()).unwrap();
    store_value(
        &mut heap,
        array,
        8,
        ValueWidth::Ref,
        RuntimeValue::Reference(first),
    )
    .unwrap();
    store_value(
        &mut heap,
        array,
        16,
        ValueWidth::Ref,
        RuntimeValue::Reference(second),
    )
    .unwrap();
    let frames = [Frame::test_entry(image.entry_index())];
    let mut registers = vec![RegisterValue::Uninitialized; image.registers_per_frame()];
    registers[3] = RegisterValue::Initialized(RuntimeValue::Reference(array));
    let actions = collect(
        &mut Collector::new(),
        &mut heap,
        &image,
        &[],
        &frames,
        &registers,
    );
    assert_eq!(
        2,
        actions
            .iter()
            .filter(|action| **action == CollectorAction::Edge)
            .count()
    );
    assert!(heap.managed_type(first).is_ok());
    assert!(heap.managed_type(second).is_ok());
    assert!(heap.managed_type(array).is_ok());
    assert!(heap.managed_type(unreachable).is_err());
}

#[test]
fn collector_steady_state_allocates_nothing() {
    let image = ExecutionImage::admit(
        fixtures::reference_field_roundtrip_artifact(),
        fixtures::profile(),
    )
    .unwrap();
    let mut heap = Heap::new(&image.storage_plan()).unwrap();
    let frames = [Frame::test_entry(image.entry_index())];
    let registers = vec![RegisterValue::Uninitialized; image.registers_per_frame()];
    let mut collector = Collector::new();
    super::tests::allocation_counter::reset_and_enable();
    collector.start();
    while collector.is_active() {
        collector
            .step(&mut heap, &image, &[], &frames, &registers, 1)
            .unwrap();
    }
    let allocations = super::tests::allocation_counter::disable_and_read();
    assert_eq!(0, allocations);
}
