use super::{
    error::{AllocationExhaustion, Outcome, VmFault},
    external_roots::ExternalRootTable,
    fixtures,
    frame::FrameArena,
    gc::{Collector, CollectorAction, CollectorPhase, RootSet},
    heap::{AllocationRequest, Heap},
    heap_ops::store_value,
    image::ExecutionImage,
    layout::{array_layout, RuntimeTypeLayout, ValueWidth},
    machine::{write_frame_value, Frame},
    value::{ReferenceDomain, RegisterValue, RuntimeValue},
    TypeKey,
};

fn allocate(heap: &mut Heap, ty: TypeKey, block_bytes: u32) -> super::value::Ref32 {
    let reservation = heap
        .reserve(AllocationRequest {
            block_bytes,
            type_id: ty.ty,
        })
        .unwrap()
        .unwrap();
    heap.commit(reservation).unwrap()
}

fn compact_roots(
    image: &ExecutionImage,
    frames: &[Frame],
    registers: &[RegisterValue],
) -> (Box<[Frame]>, FrameArena) {
    let capacity = image
        .functions()
        .iter()
        .map(|function| (function.frame_layout.byte_len + 7) & !7)
        .max()
        .unwrap_or(0)
        .saturating_mul(frames.len() as u32);
    let mut arena = FrameArena::new(capacity).unwrap();
    let width = image.registers_per_frame();
    let mut compact_frames = frames.to_vec();
    for (frame_index, frame) in compact_frames.iter_mut().enumerate() {
        let function = image.function(frame.function).unwrap();
        let reservation = arena.push(&function.frame_layout).unwrap();
        frame.base = reservation.base;
        frame.byte_len = reservation.byte_len;
        for register in 0..function.register_count {
            if let Some(RegisterValue::Initialized(value)) =
                registers.get(frame_index * width + register)
            {
                write_frame_value(&mut arena, *frame, function, register as u16, *value).unwrap();
            }
        }
    }
    (compact_frames.into_boxed_slice(), arena)
}

fn collect(
    collector: &mut Collector,
    heap: &mut Heap,
    image: &ExecutionImage,
    statics: &[RuntimeValue],
    frames: &[Frame],
    registers: &[RegisterValue],
) -> Vec<CollectorAction> {
    let external_roots = ExternalRootTable::new(0).unwrap();
    collect_with_external_roots(
        collector,
        heap,
        image,
        statics,
        frames,
        registers,
        &external_roots,
    )
}

fn collect_with_external_roots(
    collector: &mut Collector,
    heap: &mut Heap,
    image: &ExecutionImage,
    statics: &[RuntimeValue],
    frames: &[Frame],
    registers: &[RegisterValue],
    external_roots: &ExternalRootTable,
) -> Vec<CollectorAction> {
    let (frames, frame_arena) = compact_roots(image, frames, registers);
    let mut actions = Vec::new();
    collector.start();
    while collector.is_active() {
        assert_eq!(
            1,
            collector
                .step(
                    heap,
                    image,
                    RootSet {
                        static_slots: statics,
                        frames: &frames,
                        frame_arena: &frame_arena,
                        frame_depth: frames.len(),
                        external: external_roots,
                    },
                )
                .unwrap()
        );
        actions.push(collector.test_last_action().unwrap());
    }
    actions
}

#[test]
fn collector_keeps_a_managed_value_while_an_external_handle_is_active() {
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
    let retained = allocate(&mut heap, ty, layout.block_bytes);
    let dead = allocate(&mut heap, ty, layout.block_bytes);
    let mut external_roots = ExternalRootTable::new(1).unwrap();
    let handle = external_roots.retain(retained).unwrap();

    collect_with_external_roots(
        &mut Collector::new(),
        &mut heap,
        &image,
        &[],
        &[],
        &[],
        &external_roots,
    );
    assert!(heap.managed_type(retained).is_ok());
    assert!(heap.managed_type(dead).is_err());

    assert_eq!(Some(retained), external_roots.release(handle));
    collect_with_external_roots(
        &mut Collector::new(),
        &mut heap,
        &image,
        &[],
        &[],
        &[],
        &external_roots,
    );
    assert!(heap.managed_type(retained).is_err());
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
    let (frames, frame_arena) = compact_roots(&image, &frames, &registers);
    let mut collector = Collector::new();
    let external_roots = ExternalRootTable::new(0).unwrap();

    assert_eq!(
        0,
        collector
            .step(
                &mut heap,
                &image,
                RootSet {
                    static_slots: &[],
                    frames: &frames,
                    frame_arena: &frame_arena,
                    frame_depth: 1,
                    external: &external_roots,
                },
            )
            .unwrap()
    );
    collector.start();
    assert_eq!(CollectorPhase::Roots, collector.phase());

    while collector.is_active() {
        assert_eq!(
            1,
            collector
                .step(
                    &mut heap,
                    &image,
                    RootSet {
                        static_slots: &[],
                        frames: &frames,
                        frame_arena: &frame_arena,
                        frame_depth: 1,
                        external: &external_roots,
                    },
                )
                .unwrap()
        );
    }

    assert!(heap.managed_type(root).is_ok());
    assert!(heap.managed_type(child).is_ok());
    assert!(heap.managed_type(unreachable).is_err());
    assert_eq!(
        0,
        collector
            .step(
                &mut heap,
                &image,
                RootSet {
                    static_slots: &[],
                    frames: &frames,
                    frame_arena: &frame_arena,
                    frame_depth: 1,
                    external: &external_roots,
                },
            )
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
            exception: super::value::Ref32::reserved(0).unwrap(),
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
    assert_eq!(ReferenceDomain::Reserved, exhaustion.exception.domain());
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
    assert_eq!(ReferenceDomain::Reserved, exhaustion.exception.domain());
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
    profile.heap_bytes = 48;
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
            live: 48,
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
            type_id: ty.ty,
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
        vec![
            shared.payload(),
            root.payload(),
            left.payload(),
            right.payload(),
        ],
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
        12,
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
    let (frames, frame_arena) = compact_roots(&image, &frames, &registers);
    let mut collector = Collector::new();
    let external_roots = ExternalRootTable::new(0).unwrap();
    super::tests::allocation_counter::reset_and_enable();
    collector.start();
    while collector.is_active() {
        collector
            .step(
                &mut heap,
                &image,
                RootSet {
                    static_slots: &[],
                    frames: &frames,
                    frame_arena: &frame_arena,
                    frame_depth: 1,
                    external: &external_roots,
                },
            )
            .unwrap();
    }
    let allocations = super::tests::allocation_counter::disable_and_read();
    assert_eq!(0, allocations);
}

#[test]
#[ignore = "records a hardware-specific managed-heap performance baseline"]
fn managed_heap_performance_gc_units() {
    use std::time::Instant;

    const CYCLES: u32 = 10_000;
    let image = ExecutionImage::admit(fixtures::gc_graph_artifact(), fixtures::profile()).unwrap();
    let mut heap = Heap::new(&image.storage_plan()).unwrap();
    let ty = TypeKey { module: 0, ty: 1 };
    let RuntimeTypeLayout::Object(layout) = image.type_layout(ty).unwrap() else {
        unreachable!()
    };
    let [first, second] = layout.reference_offsets.as_ref() else {
        unreachable!()
    };
    let root = allocate(&mut heap, ty, layout.block_bytes);
    let left = allocate(&mut heap, ty, layout.block_bytes);
    let right = allocate(&mut heap, ty, layout.block_bytes);
    let shared = allocate(&mut heap, ty, layout.block_bytes);
    for (owner, offset, value) in [
        (root, *first, left),
        (root, *second, right),
        (left, *first, shared),
        (right, *first, shared),
        (shared, *first, root),
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
    let frames = [Frame::test_entry(image.entry_index())];
    let registers = [RegisterValue::Initialized(RuntimeValue::Reference(root))];
    let (frames, frame_arena) = compact_roots(&image, &frames, &registers);
    let statics = [RuntimeValue::Reference(shared)];
    let mut collector = Collector::new();
    let external_roots = ExternalRootTable::new(0).unwrap();
    let mut counts = [0_u64; 6];

    let started = Instant::now();
    for _ in 0..CYCLES {
        collector.start();
        while collector.is_active() {
            assert_eq!(
                1,
                collector
                    .step(
                        &mut heap,
                        &image,
                        RootSet {
                            static_slots: &statics,
                            frames: &frames,
                            frame_arena: &frame_arena,
                            frame_depth: 1,
                            external: &external_roots,
                        },
                    )
                    .unwrap()
            );
            let index = match collector.test_last_action().unwrap() {
                CollectorAction::Root => 0,
                CollectorAction::Dequeue(_) => 1,
                CollectorAction::Edge => 2,
                CollectorAction::Leaf(_) => 3,
                CollectorAction::Sweep(_) => 4,
                CollectorAction::Transition => 5,
            };
            counts[index] += 1;
        }
    }
    let elapsed = started.elapsed();
    let units: u64 = counts.iter().sum();
    println!("workload\tcycles\telapsed_ns\tunits\tunits_per_s\troot\tdequeue\tedge\tleaf\tsweep\ttransition");
    println!(
        "gc_graph\t{CYCLES}\t{}\t{units}\t{:.0}\t{}\t{}\t{}\t{}\t{}\t{}",
        elapsed.as_nanos(),
        units as f64 / elapsed.as_secs_f64(),
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        counts[4],
        counts[5],
    );
    println!(
        "gc_pause_units\tminimum={}\tmaximum={}\tconfigured_slice=1",
        units / u64::from(CYCLES),
        units / u64::from(CYCLES)
    );

    let leaf_image =
        ExecutionImage::admit(fixtures::object_allocation_artifact(0), fixtures::profile())
            .unwrap();
    let mut leaf_heap = Heap::new(&leaf_image.storage_plan()).unwrap();
    let leaf = allocate(&mut leaf_heap, TypeKey { module: 0, ty: 1 }, 32);
    let leaf_frames = [Frame::test_entry(leaf_image.entry_index())];
    let leaf_registers = [RegisterValue::Initialized(RuntimeValue::Reference(leaf))];
    let (leaf_frames, leaf_frame_arena) = compact_roots(&leaf_image, &leaf_frames, &leaf_registers);
    let mut leaf_collector = Collector::new();
    let started = Instant::now();
    let mut leaf_units = 0_u64;
    for _ in 0..CYCLES {
        leaf_collector.start();
        while leaf_collector.is_active() {
            leaf_collector
                .step(
                    &mut leaf_heap,
                    &leaf_image,
                    RootSet {
                        static_slots: &[],
                        frames: &leaf_frames,
                        frame_arena: &leaf_frame_arena,
                        frame_depth: 1,
                        external: &external_roots,
                    },
                )
                .unwrap();
            leaf_units += u64::from(matches!(
                leaf_collector.test_last_action(),
                Some(CollectorAction::Leaf(_))
            ));
        }
    }
    let elapsed = started.elapsed();
    println!(
        "gc_leaf\t{CYCLES}\t{}\t{leaf_units}\t{:.0}",
        elapsed.as_nanos(),
        leaf_units as f64 / elapsed.as_secs_f64(),
    );
}
