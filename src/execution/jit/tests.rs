use super::*;
use crate::execution::{
    checkpoint::{Reader, Writer},
    fixtures,
    frame::FrameReservation,
    image::ExecutionImage,
    machine::Machine,
    prepared::{
        tests::{backed_arena, simple_code},
        *,
    },
};

fn native(code: &PreparedCode, len: usize) -> Arc<lower::CompiledRun> {
    Arc::new(
        lower::compile(
            lower::Plan::read(code, 0, len).unwrap(),
            Reservation::acquire().unwrap(),
        )
        .unwrap(),
    )
}

fn signature(run: &SimpleRun) -> (usize, u64, String) {
    (
        run.instruction,
        run.attempts,
        match &run.end {
            RunEnd::Boundary => "boundary".into(),
            RunEnd::Branch(target) => format!("branch:{target}"),
            RunEnd::Failure(InstructionFailure::Fault(fault)) => format!("fault:{fault:?}"),
            RunEnd::Failure(InstructionFailure::Trap(trap)) => format!("trap:{trap:?}"),
        },
    )
}

fn compare_run(
    code: &PreparedCode,
    compiled: &lower::CompiledRun,
    input: &[u8],
    limit: u64,
    headroom: u64,
) {
    let mut a = backed_arena(input.len() as u32);
    let mut b = backed_arena(input.len() as u32);
    let reservation = FrameReservation {
        base: 0,
        byte_len: input.len() as u32,
    };
    let mut reference = a.prepared_frame(reservation).unwrap();
    let mut candidate = b.prepared_frame(reservation).unwrap();
    reference.jit_bytes().copy_from_slice(input);
    candidate.jit_bytes().copy_from_slice(input);
    let expected = execute_run(
        code,
        &mut reference,
        0,
        0,
        compiled.len,
        RunBudget { limit, headroom },
        |_, _| unreachable!(),
    );
    let actual = if limit.min(headroom) >= compiled.len as u64
        && input.len() >= compiled.required_bytes as usize
    {
        compiled.execute(&mut candidate, 0, None)
    } else {
        execute_run(
            code,
            &mut candidate,
            0,
            0,
            compiled.len,
            RunBudget { limit, headroom },
            |_, _| unreachable!(),
        )
    };
    assert_eq!(signature(&expected), signature(&actual));
    assert_eq!(reference.jit_bytes(), candidate.jit_bytes());
}

#[test]
fn generated_numeric_operations_match_interpreter_edge_bits_and_random_inputs() {
    for opcode in (1..=142).filter(|&op| lower::numeric_type(op).is_some()) {
        let code = simple_code(
            &(0..4)
                .map(|i| Operation {
                    opcode,
                    dst: 16 + i * 8,
                    lhs: 0,
                    rhs: 8,
                })
                .collect::<Vec<_>>(),
        );
        let compiled = native(&code, 4);
        let values = [
            0_u64,
            1,
            u64::MAX,
            0x8000_0000,
            0x8000_0000_0000_0000,
            0xffff_ffff,
            0x7f80_0000,
            0x7fc0_1234,
            0x7ff0_0000_0000_0000,
            0x7ff0_1234_5678_1234,
        ];
        for &lhs in &values {
            for &rhs in &values {
                let mut input = [0xa5; 64];
                input[..8].copy_from_slice(&lhs.to_le_bytes());
                input[8..16].copy_from_slice(&rhs.to_le_bytes());
                compare_run(&code, &compiled, &input, 4, u64::MAX);
            }
        }
        let mut random = 0x1234_5678_1234_5678_u64;
        for _ in 0..128 {
            let mut input = [0; 64];
            for chunk in input.chunks_exact_mut(8) {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                chunk.copy_from_slice(&random.to_le_bytes());
            }
            compare_run(&code, &compiled, &input, 4, u64::MAX);
        }
    }
}

#[test]
fn native_fault_pc_prior_effects_branches_and_all_budget_cuts_match() {
    for terminal in [
        Operation {
            opcode: COPY_BOOL,
            dst: 24,
            lhs: 0,
            rhs: 0,
        },
        Operation {
            opcode: COPY_CHAR,
            dst: 24,
            lhs: 0,
            rhs: 0,
        },
        Operation {
            opcode: 13,
            dst: 24,
            lhs: 0,
            rhs: 8,
        },
        Operation {
            opcode: BRANCH,
            dst: 0,
            lhs: 7,
            rhs: 8,
        },
        Operation {
            opcode: JUMP,
            dst: 11,
            lhs: 0,
            rhs: 0,
        },
    ] {
        let code = simple_code(&[
            Operation {
                opcode: IMMEDIATE32,
                dst: 16,
                lhs: 123,
                rhs: 0,
            },
            Operation {
                opcode: COPY64,
                dst: 32,
                lhs: 0,
                rhs: 0,
            },
            Operation {
                opcode: NOP,
                ..Operation::default()
            },
            terminal,
        ]);
        let compiled = native(&code, 4);
        for value in [0_u32, 1, 2, 65535, 65536, u32::MAX] {
            let mut input = [0; 64];
            input[..4].copy_from_slice(&value.to_le_bytes());
            for limit in 0..=5 {
                for headroom in 0..=5 {
                    compare_run(&code, &compiled, &input, limit, headroom);
                }
            }
        }
    }
}

#[test]
fn overlapping_component_copies_are_sequential_and_unaligned() {
    let mut code = simple_code(
        &[Operation {
            opcode: COPIES,
            dst: 0,
            lhs: 2,
            rhs: 0,
        }; 4],
    );
    code.copies = vec![
        Copy {
            src: 1,
            dst: 3,
            width: 4,
        },
        Copy {
            src: 3,
            dst: 5,
            width: 8,
        },
    ]
    .into_boxed_slice();
    let compiled = native(&code, 4);
    compare_run(&code, &compiled, &[0x5a; 64], 4, 4);
    let mut input = [0; 64];
    for (i, x) in input.iter_mut().enumerate() {
        *x = i as u8;
    }
    compare_run(&code, &compiled, &input, 4, 4);
}

#[test]
fn compiled_entry_checks_short_frames_budgets_and_accounting_headroom() {
    let code = simple_code(
        &[Operation {
            opcode: COPY64,
            dst: 32,
            lhs: 0,
            rhs: 0,
        }; 4],
    );
    let owner = code.jit.get_or_init(|| Arc::new(JitState::default()));
    let slot = &owner.slots[slot_index(0)];
    slot.key.store(0, Ordering::Release);
    slot.native.set(native(&code, 4)).unwrap();
    for capacity in [16, 64] {
        let mut arena = backed_arena(capacity);
        let mut frame = arena
            .prepared_frame(FrameReservation {
                base: 0,
                byte_len: capacity,
            })
            .unwrap();
        for (limit, headroom) in [(3, 8), (8, 3), (4, 4)] {
            let actual = try_execute(
                &code,
                &mut frame,
                Location {
                    code_start: 0,
                    instruction: 0,
                    block_len: 4,
                },
                &RunBudget { limit, headroom },
                None,
            );
            assert_eq!(
                actual.is_some(),
                capacity == 64 && limit >= 4 && headroom >= 4
            );
        }
    }
}

#[test]
fn hotness_publication_and_executable_lifetime_are_shared_across_threads() {
    let code = Arc::new(simple_code(
        &[Operation {
            opcode: IMMEDIATE32,
            dst: 0,
            lhs: 77,
            rhs: 0,
        }; 4],
    ));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let code = code.clone();
            scope.spawn(move || {
                let mut arena = backed_arena(64);
                let mut frame = arena
                    .prepared_frame(FrameReservation {
                        base: 0,
                        byte_len: 64,
                    })
                    .unwrap();
                for _ in 0..HOT {
                    let _ = try_execute(
                        &code,
                        &mut frame,
                        Location {
                            code_start: 0,
                            instruction: 0,
                            block_len: 4,
                        },
                        &RunBudget {
                            limit: 4,
                            headroom: 4,
                        },
                        None,
                    );
                }
            });
        }
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while statistics(&code).pending != 0 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(statistics(&code).compiled, 1);
    assert_eq!(statistics(&code).failures, 0);
    let native = code.jit.get().unwrap().slots[slot_index(0)]
        .native
        .get()
        .unwrap()
        .clone();
    let weak = Arc::downgrade(&native);
    let mapped = native.mapped_counter.clone();
    assert!(mapped.load(Ordering::Relaxed) > 0);
    drop(code);
    let mut arena = backed_arena(64);
    let mut frame = arena
        .prepared_frame(FrameReservation {
            base: 0,
            byte_len: 64,
        })
        .unwrap();
    assert_eq!(native.execute(&mut frame, 0, None).attempts, 4);
    assert_eq!(frame.read::<4>(0).unwrap(), 77_u32.to_le_bytes());
    drop(native);
    assert!(weak.upgrade().is_none());
    assert_eq!(mapped.load(Ordering::Relaxed), 0);
}

fn checkpoint(machine: &Machine) -> Vec<u8> {
    let mut writer = Writer::new(8 * 1024 * 1024);
    machine.write_checkpoint_state(&mut writer).unwrap();
    writer.finish()
}

#[test]
fn jit_machine_matches_traces_and_checkpoint_bytes_at_slice_boundaries() {
    // Real verified control flow, frame overlap, calls, tasks, exceptions and GC paths.
    let mut artifacts: Vec<_> = fixtures::performance_workloads()
        .into_iter()
        .map(|x| x.artifact)
        .collect();
    let heap_fixture = artifacts.len();
    artifacts.extend([
        fixtures::jit_heap_loop_artifact(),
        fixtures::jit_numeric_loop_artifact(),
        fixtures::nested_call_artifact(),
        fixtures::nested_fault_artifact(),
        fixtures::task_spawn_join_artifact(),
        fixtures::channel_handoff_artifact(),
        fixtures::exception_artifact(true, true, false),
    ]);
    let mut hits = 0;
    for (artifact_index, artifact) in artifacts.into_iter().enumerate() {
        for traced in [false, true] {
            let image = ExecutionImage::admit(artifact.clone(), fixtures::profile()).unwrap();
            let mut a = if traced {
                Machine::new(image.clone())
            } else {
                Machine::new_untraced(image.clone())
            }
            .unwrap();
            let mut b = if traced {
                Machine::new(image)
            } else {
                Machine::new_untraced(image)
            }
            .unwrap();
            b.jit_enabled = true;
            a.start(&[]).unwrap();
            b.start(&[]).unwrap();
            for i in 0..600 {
                if i == 300 {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                    while b.jit_statistics().pending != 0 {
                        assert!(std::time::Instant::now() < deadline);
                        std::thread::yield_now();
                    }
                }
                let limit = [0, 1, 2, 3, 4, 64, 128][i % 7];
                let budget = a.minimum_run_budget().max(128);
                let expected = a.run_slice_with_retirement_limit(budget, 8, limit);
                let actual = b.run_slice_with_retirement_limit(budget, 8, limit);
                assert_eq!(format!("{expected:?}"), format!("{actual:?}"));
                if !traced {
                    assert_eq!(checkpoint(&a), checkpoint(&b));
                }
                assert_eq!(a.trace_digest(), b.trace_digest());
                if !traced && i % 31 == 0 {
                    let bytes = checkpoint(&b);
                    let mut reader =
                        Reader::new(&bytes, 8 * 1024 * 1024, 16 * 1024 * 1024).unwrap();
                    b.restore_checkpoint_state(&mut reader).unwrap();
                    reader.finish().unwrap();
                    // Tier selection is ephemeral and intentionally absent from checkpoint bytes.
                    b.jit_enabled = true;
                }
            }
            if artifact_index == heap_fixture {
                assert!(
                    b.jit_statistics().hits > 0,
                    "heap loop must execute generated code"
                );
            }
            hits += b.jit_statistics().hits;
        }
    }
    assert!(hits > 0, "machine comparison must execute generated code");
}

#[test]
fn executable_mapping_and_module_reservations_are_bounded() {
    let count = AtomicUsize::new(0);
    for _ in 0..MAX_MODULES {
        assert!(reserve_count(&count));
    }
    assert!(!reserve_count(&count));
    assert_eq!(count.load(Ordering::Relaxed), MAX_MODULES);
}

#[path = "tests/heap.rs"]
mod heap;
