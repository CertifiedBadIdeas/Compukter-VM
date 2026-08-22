use super::{
    error::{GuestTrap, Outcome, RunError},
    fixtures,
    image::ExecutionImage,
    machine::Machine,
    value::{EntryArgument, RuntimeValue},
};
use sha2::{Digest, Sha256};

mod allocation_counter {
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

    pub(super) fn reset_and_enable() {
        ALLOCATIONS.with(|allocations| allocations.set(0));
        ENABLED.with(|enabled| enabled.set(true));
    }

    pub(super) fn disable_and_read() -> u64 {
        ENABLED.with(|enabled| enabled.set(false));
        ALLOCATIONS.with(Cell::get)
    }
}

#[global_allocator]
static TEST_ALLOCATOR: allocation_counter::CountingAllocator =
    allocation_counter::CountingAllocator;

#[test]
fn direct_calls_copy_arguments_and_publish_results_on_return() {
    let mut machine = fixtures::started_zero_arg(fixtures::nested_call_artifact());
    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::I32(42))),
        machine.run_slice(128).unwrap()
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
        machine.run_slice(128).unwrap()
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
        let outcome = machine.run_slice(budget).unwrap();
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
        let outcome = machine.run_slice(case.budget).unwrap();
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
            166, 84, 34, 161, 100, 88, 173, 25, 105, 181, 10, 183, 116, 180, 193, 205, 85, 40, 175,
            96, 101, 147, 151, 218, 69, 87, 171, 59, 106, 147, 61, 44
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
                machine.run_slice(4_096).unwrap(),
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
            assert_eq!(Outcome::SliceExhausted, machine.run_slice(BUDGET).unwrap());
        }
        let blocks_before = machine.entered_blocks();
        let instructions_before = machine.executed_instructions();
        let started = Instant::now();
        for _ in 0..MEASURED_SLICES {
            assert_eq!(Outcome::SliceExhausted, machine.run_slice(BUDGET).unwrap());
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
fn block_cost_is_atomic_and_slice_remainder_is_discarded() {
    let mut exact = fixtures::started_zero_arg(fixtures::two_block_artifact(3, 5));
    assert_eq!(Outcome::Halted(None), exact.run_slice(8).unwrap());
    assert_eq!(8, exact.consumed_fixed_cost());

    let mut short = fixtures::started_zero_arg(fixtures::two_block_artifact(3, 5));
    assert_eq!(Outcome::SliceExhausted, short.run_slice(7).unwrap());
    assert_eq!(3, short.consumed_fixed_cost());
    assert_eq!(Outcome::Halted(None), short.run_slice(5).unwrap());
    assert_eq!(8, short.consumed_fixed_cost());
}

#[test]
fn while_true_executes_floor_budget_over_block_cost_iterations() {
    let mut machine = fixtures::started_zero_arg(fixtures::empty_loop_artifact(3));
    assert_eq!(Outcome::SliceExhausted, machine.run_slice(10).unwrap());
    assert_eq!(9, machine.consumed_fixed_cost());
    assert_eq!(3, machine.entered_blocks());
    assert_eq!(Outcome::SliceExhausted, machine.run_slice(4).unwrap());
    assert_eq!(12, machine.consumed_fixed_cost());
    assert_eq!(4, machine.entered_blocks());
}

#[test]
fn trap_keeps_the_full_containing_block_charge() {
    let mut machine = fixtures::started_zero_arg(fixtures::trap_after_write_artifact(7));
    assert_eq!(
        Outcome::Crashed(super::error::GuestTrap::DivisionByZero),
        machine.run_slice(7).unwrap()
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
            machine.run_slice(64).unwrap()
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
        .start(&[EntryArgument(RuntimeValue::I64(1))])
        .is_err());
    assert_eq!(before, machine.test_snapshot());
    machine
        .start(&[EntryArgument(RuntimeValue::I32(1))])
        .unwrap();
    assert_eq!(1, machine.frame_depth());
    assert_eq!(RuntimeValue::I32(1), machine.test_register(0).unwrap());
}

#[test]
fn references_require_matching_image_type_liveness_and_generation() {
    let (image, valid, foreign, dead, stale) = fixtures::reference_entry_case();
    assert!(Machine::new(image.clone())
        .unwrap()
        .start(&[EntryArgument(valid)])
        .is_ok());
    assert!(Machine::new(image.clone())
        .unwrap()
        .start(&[EntryArgument(foreign)])
        .is_err());
    assert!(Machine::new(image.clone())
        .unwrap()
        .start(&[EntryArgument(dead)])
        .is_err());
    assert!(Machine::new(image)
        .unwrap()
        .start(&[EntryArgument(stale)])
        .is_err());
}

#[test]
fn failed_start_is_retryable_but_successful_start_is_one_shot() {
    let image =
        ExecutionImage::admit(fixtures::typed_entry_artifact(), fixtures::profile()).unwrap();
    let mut machine = Machine::new(image).unwrap();
    assert!(machine.start(&[]).is_err());
    machine
        .start(&[EntryArgument(RuntimeValue::I32(7))])
        .unwrap();
    assert_eq!(
        Err(RunError::AlreadyStarted),
        machine.start(&[EntryArgument(RuntimeValue::I32(8))])
    );
}
