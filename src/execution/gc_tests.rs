use super::{
    error::{AllocationExhaustion, Outcome},
    fixtures,
    gc::{Collector, CollectorAction, CollectorPhase},
    heap::{AllocationRequest, Heap},
    heap_ops::store_value,
    image::ExecutionImage,
    layout::{array_layout, RuntimeTypeLayout, ValueWidth},
    machine::Frame,
    value::{RegisterValue, RuntimeValue},
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
fn machine_stops_guest_for_collection_and_retries_once_after_sweep() {
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
            requested_block_bytes: 32,
            total_free: 0,
            largest_free_block: 0,
            collection_attempted: true,
        }),
        outcome
    );
    assert_eq!(outcome, machine.run_slice(minimum, 1).unwrap());
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
