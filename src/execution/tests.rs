use super::{
    error::{GuestTrap, Outcome, RunError},
    fixtures,
    image::ExecutionImage,
    machine::Machine,
    value::{EntryArgument, ReferenceDomain, ReferenceValue, RuntimeValue},
};
use sha2::{Digest, Sha256};

#[test]
fn managed_heap_vertical_conformance() {
    fn record(machine: &Machine, digest: &mut Sha256, totals: &mut [u64; 3]) {
        let heap = machine.test_heap_diagnostic();
        digest.update(machine.trace_digest());
        for value in [
            machine.consumed_fixed_cost(),
            machine.consumed_dynamic_cost(),
            machine.consumed_maintenance_cost(),
            u64::from(heap.total_free),
            u64::from(heap.largest_free_block),
            u64::from(heap.live_handles),
            u64::from(heap.retired_handles),
        ] {
            digest.update(value.to_le_bytes());
        }
        totals[0] += machine.consumed_fixed_cost();
        totals[1] += machine.consumed_dynamic_cost();
        totals[2] += machine.consumed_maintenance_cost();
    }

    let mut digest = Sha256::new();
    let mut totals = [0_u64; 3];

    let mut inherited = fixtures::started_zero_arg(fixtures::field_roundtrip_artifact());
    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::I32(42))),
        inherited.run_slice(64, 0).unwrap()
    );
    assert_eq!(Some(RuntimeValue::Bool(true)), inherited.test_register(3));
    record(&inherited, &mut digest, &mut totals);

    let mut references = fixtures::started_zero_arg(fixtures::reference_array_roundtrip_artifact());
    assert!(matches!(
        references.run_slice(64, 0).unwrap(),
        Outcome::Halted(Some(RuntimeValue::Reference(_)))
    ));
    assert_eq!(references.test_register(2), references.test_register(4));
    record(&references, &mut digest, &mut totals);

    let static_image =
        ExecutionImage::admit(fixtures::static_roundtrip_artifact(), fixtures::profile()).unwrap();
    let mut static_root = Machine::new(static_image).unwrap();
    static_root
        .start(&[
            EntryArgument::unowned(RuntimeValue::Bool(true)),
            EntryArgument::unowned(RuntimeValue::I32(42)),
        ])
        .unwrap();
    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::I32(42))),
        static_root.run_slice(32, 0).unwrap()
    );
    record(&static_root, &mut digest, &mut totals);

    let mut text = fixtures::started_zero_arg(fixtures::repeated_concat_artifact(true));
    let mut outcome = text.run_slice(3, 0).unwrap();
    while outcome == Outcome::SliceExhausted {
        outcome = text.run_slice(8, 0).unwrap();
    }
    assert_eq!(Outcome::Halted(Some(RuntimeValue::Bool(true))), outcome);
    record(&text, &mut digest, &mut totals);

    let mut recovery_profile = fixtures::profile();
    recovery_profile.heap_bytes = 32;
    let recovery_image =
        ExecutionImage::admit(fixtures::gc_retry_artifact(), recovery_profile).unwrap();
    let recovery_budget = recovery_image.minimum_slice_cost();
    let mut recovery = Machine::new(recovery_image).unwrap();
    recovery.start(&[]).unwrap();
    let recovery_outcome = loop {
        let outcome = recovery.run_slice(recovery_budget, 1).unwrap();
        if outcome.is_terminal() {
            break outcome;
        }
    };
    assert!(matches!(
        recovery_outcome,
        Outcome::Halted(Some(RuntimeValue::Reference(_)))
    ));
    assert_eq!(1, recovery.test_heap_diagnostic().live_handles);
    record(&recovery, &mut digest, &mut totals);

    let mut oom_profile = fixtures::profile();
    oom_profile.heap_bytes = 32;
    let oom_image =
        ExecutionImage::admit(fixtures::gc_failed_retry_artifact(), oom_profile).unwrap();
    let oom_budget = oom_image.minimum_slice_cost();
    let mut oom = Machine::new(oom_image).unwrap();
    oom.start(&[]).unwrap();
    let oom_outcome = loop {
        let outcome = oom.run_slice(oom_budget, 1).unwrap();
        if outcome.is_terminal() {
            break outcome;
        }
    };
    assert!(matches!(oom_outcome, Outcome::AllocationExhausted(_)));
    record(&oom, &mut digest, &mut totals);

    let observation = (totals, <[u8; 32]>::from(digest.finalize()));
    assert_eq!(
        (
            [69, 7, 12],
            [
                163, 47, 70, 26, 245, 57, 36, 152, 93, 253, 243, 4, 30, 128, 51, 37, 254, 105, 211,
                242, 74, 160, 16, 41, 33, 135, 235, 21, 181, 67, 78, 100,
            ],
        ),
        observation
    );
}

pub(super) mod allocation_counter {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };

    thread_local! {
        static ENABLED: Cell<bool> = const { Cell::new(false) };
        static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
    }

    pub(super) struct CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count();
            // SAFETY: the request is delegated unchanged to the system allocator.
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count();
            // SAFETY: the request is delegated unchanged to the system allocator.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            // SAFETY: the pointer and layout came from the delegated allocator.
            unsafe { System.dealloc(pointer, layout) }
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            if new_size > layout.size() {
                count();
            }
            // SAFETY: the request is delegated unchanged to the system allocator.
            unsafe { System.realloc(pointer, layout, new_size) }
        }
    }

    fn count() {
        ENABLED.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|allocations| allocations.set(allocations.get() + 1));
            }
        });
    }

    pub(crate) fn reset_and_enable() {
        ALLOCATIONS.with(|allocations| allocations.set(0));
        ENABLED.with(|enabled| enabled.set(true));
    }

    pub(crate) fn disable_and_read() -> u64 {
        ENABLED.with(|enabled| enabled.set(false));
        ALLOCATIONS.with(Cell::get)
    }
}

#[global_allocator]
static TEST_ALLOCATOR: allocation_counter::CountingAllocator =
    allocation_counter::CountingAllocator;

#[test]
fn compact_reference_token_is_eight_bytes() {
    assert_eq!(8, core::mem::size_of::<ReferenceValue>());

    let managed = ReferenceValue::managed(1, 7).unwrap();
    let literal = ReferenceValue::literal(1).unwrap();
    let emergency = ReferenceValue::emergency();
    let host = ReferenceValue::host(1, 7).unwrap();
    assert_eq!(ReferenceDomain::Managed, managed.domain());
    assert_eq!(ReferenceDomain::Literal, literal.domain());
    assert_eq!(ReferenceDomain::Emergency, emergency.domain());
    assert_eq!(ReferenceDomain::Host, host.domain());
    assert_ne!(managed, literal);
    assert_ne!(managed, host);
    assert_ne!(managed, ReferenceValue::managed(1, 8).unwrap());
    assert_eq!(1, managed.slot());
    assert_eq!(7, managed.generation());
    assert!(ReferenceValue::managed(ReferenceValue::MAX_SLOT, 0).is_some());
    assert!(ReferenceValue::managed(ReferenceValue::MAX_SLOT + 1, 0).is_none());
}

#[test]
fn direct_calls_copy_arguments_and_publish_results_on_return() {
    let mut machine = fixtures::started_zero_arg(fixtures::nested_call_artifact());
    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::I32(42))),
        machine.run_slice(128, 0).unwrap()
    );
    assert_eq!(3, machine.maximum_observed_frame_depth_for_test());
}

#[test]
fn stack_overflow_happens_before_a_new_frame_exists() {
    let mut profile = fixtures::profile();
    profile.maximum_call_depth = 3;
    let mut machine = fixtures::started_with_profile(fixtures::recursive_artifact(3), profile);
    assert_eq!(
        Outcome::Crashed(GuestTrap::StackOverflow),
        machine.run_slice(128, 0).unwrap()
    );
    assert_eq!(3, machine.frame_depth());
    assert_eq!(
        fixtures::recursive_pre_call_state(),
        machine.test_active_registers()
    );
}

#[test]
fn scalar_vectors_match_kotlin_jvm_semantics() {
    for case in fixtures::scalar_cases() {
        let profile = fixtures::profile();
        let budget = profile.maximum_slice_budget;
        let image = ExecutionImage::admit(case.artifact, profile).unwrap();
        let mut machine = Machine::new(image).unwrap();
        machine.start(&case.args).unwrap();
        let outcome = machine.run_slice(budget, 0).unwrap();
        match case.expected {
            Ok(value) => assert_eq!(Outcome::Halted(Some(value)), outcome, "{}", case.name),
            Err(trap) => assert_eq!(Outcome::Crashed(trap), outcome, "{}", case.name),
        }
        assert_eq!(
            case.expected_fixed_cost,
            machine.consumed_fixed_cost(),
            "{}",
            case.name
        );
    }
}

#[test]
fn block_boundary_trace_digests_are_stable() {
    for case in fixtures::trace_cases() {
        let mut machine = fixtures::started(case.artifact, &case.args);
        let outcome = machine.run_slice(case.budget, 0).unwrap();
        assert_eq!(case.outcome, outcome, "{}", case.name);
        assert_eq!(case.digest, machine.trace_digest(), "{}", case.name);
        assert_eq!(
            case.fixed_cost,
            machine.consumed_fixed_cost(),
            "{}",
            case.name
        );
        assert_eq!(0, machine.consumed_dynamic_cost(), "{}", case.name);
    }
}

#[test]
fn straight_line_trace_digest_matches_documented_field_encoding() {
    let case = fixtures::scalar_cases().remove(0);
    let content_hash = case.artifact.content_hash();
    let mut trace = Sha256::new();
    let field = |trace: &mut Sha256, bytes: &[u8]| {
        trace.update((bytes.len() as u32).to_le_bytes());
        trace.update(bytes);
    };
    field(&mut trace, &[1]);
    field(&mut trace, &content_hash);
    for value in [0_u32, 0, 0, 1, 0] {
        field(&mut trace, &value.to_le_bytes());
    }
    field(&mut trace, &2_u64.to_le_bytes());
    field(&mut trace, &0_u64.to_le_bytes());
    field(&mut trace, &1_u32.to_le_bytes());
    field(&mut trace, &[1, 1, 7, 0, 0, 0]);
    assert_eq!(
        [
            210, 97, 93, 138, 111, 54, 126, 53, 10, 37, 45, 198, 192, 27, 212, 174, 59, 165, 154,
            74, 150, 26, 207, 5, 120, 25, 252, 251, 187, 38, 243, 161
        ],
        <[u8; 32]>::from(trace.finalize())
    );
}

#[test]
fn scalar_control_steady_state_allocates_nothing() {
    for artifact in fixtures::allocation_workloads() {
        let mut machine = fixtures::started_zero_arg(artifact);
        allocation_counter::reset_and_enable();
        for _ in 0..1_000 {
            assert!(matches!(
                machine.run_slice(4_096, 0).unwrap(),
                Outcome::SliceExhausted
            ));
        }
        let allocations = allocation_counter::disable_and_read();
        assert_eq!(0, allocations);
    }
}

#[test]
#[ignore = "records a hardware-specific release performance baseline"]
fn tier0_performance_baseline() {
    use std::{fmt::Write, process::Command, time::Instant};

    const WARMUP_SLICES: usize = 100;
    const MEASURED_SLICES: usize = 1_000;
    const BUDGET: u32 = 4_096;

    let rustc = Command::new("rustc").arg("-Vv").output().unwrap();
    let rustc = String::from_utf8_lossy(&rustc.stdout);
    let release = rustc
        .lines()
        .find_map(|line| line.strip_prefix("release: "))
        .unwrap_or("unknown");
    let host = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap_or("unknown");
    let cpu = std::env::var("COMPUKTER_BENCH_CPU").unwrap_or_else(|_| "unspecified".into());
    println!("artifact\tworkload\tblocks\tinstructions\telapsed_ns\tblocks_per_s\tinstructions_per_s\trustc\thost\tcpu");

    for workload in fixtures::performance_workloads() {
        let hash = workload.artifact.content_hash();
        let mut machine = fixtures::started_zero_arg(workload.artifact);
        for _ in 0..WARMUP_SLICES {
            assert_eq!(
                Outcome::SliceExhausted,
                machine.run_slice(BUDGET, 0).unwrap()
            );
        }
        let blocks_before = machine.entered_blocks();
        let instructions_before = machine.executed_instructions();
        let started = Instant::now();
        for _ in 0..MEASURED_SLICES {
            assert_eq!(
                Outcome::SliceExhausted,
                machine.run_slice(BUDGET, 0).unwrap()
            );
        }
        let elapsed = started.elapsed();
        let blocks = machine.entered_blocks() - blocks_before;
        let instructions = machine.executed_instructions() - instructions_before;
        let elapsed_ns = elapsed.as_nanos();
        assert!(elapsed_ns > 0 && blocks > 0 && instructions > 0);
        let seconds = elapsed.as_secs_f64();
        let mut hash_text = String::with_capacity(64);
        for byte in hash {
            write!(&mut hash_text, "{byte:02x}").unwrap();
        }
        println!(
            "{}\t{}\t{}\t{}\t{}\t{:.0}\t{:.0}\t{}\t{}\t{}",
            hash_text,
            workload.name,
            blocks,
            instructions,
            elapsed_ns,
            blocks as f64 / seconds,
            instructions as f64 / seconds,
            release,
            host,
            cpu
        );
    }
}

#[test]
#[ignore = "records a hardware-specific managed-heap performance baseline"]
fn managed_heap_performance_operations_and_idle_instances() {
    use std::time::Instant;

    const ITERATIONS: u32 = 10_000;
    println!("workload\titerations\telapsed_ns\toperations_per_s");

    let field_image =
        ExecutionImage::admit(fixtures::field_roundtrip_artifact(), fixtures::profile()).unwrap();
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        let mut machine = Machine::new(field_image.clone()).unwrap();
        machine.start(&[]).unwrap();
        assert_eq!(
            Outcome::Halted(Some(RuntimeValue::I32(42))),
            machine.run_slice(64, 0).unwrap()
        );
    }
    let elapsed = started.elapsed();
    println!(
        "inherited_field_roundtrip\t{ITERATIONS}\t{}\t{:.0}",
        elapsed.as_nanos(),
        f64::from(ITERATIONS) / elapsed.as_secs_f64(),
    );

    let array_image = ExecutionImage::admit(
        fixtures::reference_array_roundtrip_artifact(),
        fixtures::profile(),
    )
    .unwrap();
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        let mut machine = Machine::new(array_image.clone()).unwrap();
        machine.start(&[]).unwrap();
        assert!(matches!(
            machine.run_slice(64, 0).unwrap(),
            Outcome::Halted(Some(RuntimeValue::Reference(_)))
        ));
    }
    let elapsed = started.elapsed();
    println!(
        "reference_array_roundtrip\t{ITERATIONS}\t{}\t{:.0}",
        elapsed.as_nanos(),
        f64::from(ITERATIONS) / elapsed.as_secs_f64(),
    );

    let text_image = ExecutionImage::admit(
        fixtures::repeated_concat_artifact(true),
        fixtures::profile(),
    )
    .unwrap();
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        let mut machine = Machine::new(text_image.clone()).unwrap();
        machine.start(&[]).unwrap();
        assert_eq!(
            Outcome::Halted(Some(RuntimeValue::Bool(true))),
            machine.run_slice(128, 0).unwrap()
        );
    }
    let elapsed = started.elapsed();
    println!(
        "compact_string_concat_equals\t{ITERATIONS}\t{}\t{:.0}",
        elapsed.as_nanos(),
        f64::from(ITERATIONS) / elapsed.as_secs_f64(),
    );

    let mut idle_profile = fixtures::profile();
    idle_profile.heap_bytes = 32;
    let idle_heap_bytes = idle_profile.heap_bytes;
    let idle_image =
        ExecutionImage::admit(fixtures::object_allocation_artifact(0), idle_profile).unwrap();
    let started = Instant::now();
    let mut instances = Vec::with_capacity(ITERATIONS as usize);
    for _ in 0..ITERATIONS {
        instances.push(Machine::new(idle_image.clone()).unwrap());
    }
    let elapsed = started.elapsed();
    assert!(instances
        .iter()
        .all(|machine| machine.consumed_maintenance_cost() == 0));
    let resident_reserved_bytes: usize = instances.iter().map(Machine::test_reserved_bytes).sum();
    println!(
        "idle_instance_admission\t{ITERATIONS}\t{}\t{:.0}",
        elapsed.as_nanos(),
        f64::from(ITERATIONS) / elapsed.as_secs_f64(),
    );
    println!(
        "idle_zero_work\tinstances={ITERATIONS}\tmaintenance_units=0\tmachine_struct_bytes={}\theap_bytes_per_instance={}\tinstance_reserved_bytes={resident_reserved_bytes}\tshared_image=excluded\tallocator_overhead=excluded",
        core::mem::size_of::<Machine>(),
        idle_heap_bytes,
    );
}

#[test]
fn block_cost_is_atomic_and_slice_remainder_is_discarded() {
    let mut exact = fixtures::started_zero_arg(fixtures::two_block_artifact(3, 5));
    assert_eq!(Outcome::Halted(None), exact.run_slice(8, 0).unwrap());
    assert_eq!(8, exact.consumed_fixed_cost());

    let mut short = fixtures::started_zero_arg(fixtures::two_block_artifact(3, 5));
    assert_eq!(Outcome::SliceExhausted, short.run_slice(7, 0).unwrap());
    assert_eq!(3, short.consumed_fixed_cost());
    assert_eq!(Outcome::Halted(None), short.run_slice(5, 0).unwrap());
    assert_eq!(8, short.consumed_fixed_cost());
}

#[test]
fn while_true_executes_floor_budget_over_block_cost_iterations() {
    let mut machine = fixtures::started_zero_arg(fixtures::empty_loop_artifact(3));
    assert_eq!(Outcome::SliceExhausted, machine.run_slice(10, 0).unwrap());
    assert_eq!(9, machine.consumed_fixed_cost());
    assert_eq!(3, machine.entered_blocks());
    assert_eq!(Outcome::SliceExhausted, machine.run_slice(4, 0).unwrap());
    assert_eq!(12, machine.consumed_fixed_cost());
    assert_eq!(4, machine.entered_blocks());
}

#[test]
fn trap_keeps_the_full_containing_block_charge() {
    let mut machine = fixtures::started_zero_arg(fixtures::trap_after_write_artifact(7));
    assert_eq!(
        Outcome::Crashed(super::error::GuestTrap::DivisionByZero),
        machine.run_slice(7, 0).unwrap()
    );
    assert_eq!(7, machine.consumed_fixed_cost());
    assert_eq!(
        fixtures::pre_trap_registers(),
        machine.test_active_registers()
    );
}

#[test]
fn branch_and_switch_select_only_the_verified_target() {
    for (key, expected) in [(0, 10), (1, 20), (7, 30)] {
        let (artifact, args) = fixtures::branch_switch_artifact(key);
        let mut machine = fixtures::started(artifact, &args);
        assert_eq!(
            Outcome::Halted(Some(RuntimeValue::I32(expected))),
            machine.run_slice(64, 0).unwrap()
        );
    }
}

#[test]
fn start_validates_all_arguments_before_mutation() {
    let image =
        ExecutionImage::admit(fixtures::typed_entry_artifact(), fixtures::profile()).unwrap();
    let mut machine = Machine::new(image).unwrap();
    let before = machine.test_snapshot();
    assert!(machine
        .start(&[EntryArgument::unowned(RuntimeValue::I64(1))])
        .is_err());
    assert_eq!(before, machine.test_snapshot());
    machine
        .start(&[EntryArgument::unowned(RuntimeValue::I32(1))])
        .unwrap();
    assert_eq!(1, machine.frame_depth());
    assert_eq!(RuntimeValue::I32(1), machine.test_register(0).unwrap());
}

#[test]
fn references_require_matching_image_type_liveness_and_generation() {
    let (image, valid, foreign, dead, stale) = fixtures::reference_entry_case();
    assert!(Machine::new(image.clone()).unwrap().start(&[valid]).is_ok());
    assert_eq!(
        Err(RunError::ForeignReference { parameter: 0 }),
        Machine::new(image.clone()).unwrap().start(&[foreign])
    );
    assert_eq!(
        Err(RunError::DeadReference { parameter: 0 }),
        Machine::new(image.clone()).unwrap().start(&[dead])
    );
    assert_eq!(
        Err(RunError::DeadReference { parameter: 0 }),
        Machine::new(image).unwrap().start(&[stale])
    );
}

#[test]
fn failed_start_is_retryable_but_successful_start_is_one_shot() {
    let image =
        ExecutionImage::admit(fixtures::typed_entry_artifact(), fixtures::profile()).unwrap();
    let mut machine = Machine::new(image).unwrap();
    assert!(machine.start(&[]).is_err());
    machine
        .start(&[EntryArgument::unowned(RuntimeValue::I32(7))])
        .unwrap();
    assert_eq!(
        Err(RunError::AlreadyStarted),
        machine.start(&[EntryArgument::unowned(RuntimeValue::I32(8))])
    );
}
