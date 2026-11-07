use super::*;
use crate::{
    artifact::Instruction,
    execution::{
        checkpoint::{Reader, Writer},
        fixtures,
        value::{EntryArgument, RuntimeValue},
    },
};

#[test]
fn simd_unpack_matches_scalar_for_all_operand_bytes_and_inactive_tail_lanes() {
    for value in 0..=255u32 {
        let words = [
            value | (255 - value) << 8 | value << 16 | (value ^ 0xa5) << 24,
            0,
            u32::MAX,
            0x0123_4567,
        ];
        let mut expected = [Operation::default(); 4];
        let mut actual = [Operation::default(); 4];
        unpack_scalar(&words, &mut expected);
        unpack_four(&words, &mut actual);
        for (expected, actual) in expected.iter().zip(actual) {
            assert_eq!(
                (expected.opcode, expected.dst, expected.lhs, expected.rhs),
                (actual.opcode, actual.dst, actual.lhs, actual.rhs)
            );
        }
    }
}

fn bytes(machine: &super::super::machine::Machine) -> Vec<u8> {
    let mut writer = Writer::new(8 * 1024 * 1024);
    machine.write_checkpoint_state(&mut writer).unwrap();
    writer.finish()
}

fn started(
    artifact: crate::VerifiedArtifact,
    args: &[EntryArgument],
) -> super::super::machine::Machine {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 16 * 1024;
    let image = super::super::image::ExecutionImage::admit(artifact, profile).unwrap();
    let mut machine = super::super::machine::Machine::new_untraced(image).unwrap();
    machine.use_reference_execution();
    machine.start(args).unwrap();
    machine
}

fn compare(
    artifact: crate::VerifiedArtifact,
    args: &[EntryArgument],
    mode: ExecutionMode,
    steps: usize,
) {
    let mut reference = started(artifact.clone(), args);
    let mut candidate = started(artifact, args);
    candidate.prepare_execution(mode).unwrap();
    for step in 0..steps {
        // Changing the retirement limit includes zero-work and arbitrary interruption.
        let retirement = [0, 1, 2, 1, 3][step % 5];
        let expected = reference.run_slice_with_retirement_limit(128, 8, retirement);
        let actual = candidate.run_slice_with_retirement_limit(128, 8, retirement);
        assert_eq!(
            format!("{expected:?}"),
            format!("{actual:?}"),
            "{mode:?}, step {step}"
        );
        assert_eq!(
            bytes(&reference),
            bytes(&candidate),
            "{mode:?}, step {step}"
        );
        // Rebuild derived candidate code after a real logical checkpoint restore.
        let encoded = bytes(&candidate);
        let mut reader = Reader::new(&encoded, 8 * 1024 * 1024, 16 * 1024 * 1024).unwrap();
        // Use a newly admitted image of the same artifact via the machine restore helper.
        candidate.restore_checkpoint_state(&mut reader).unwrap();
        reader.finish().unwrap();
        candidate.prepare_execution(mode).unwrap();
    }
}

#[test]
fn reference_and_candidates_preserve_scalar_results_and_every_instruction_boundary() {
    for mode in [
        ExecutionMode::Packed,
        ExecutionMode::Slots,
        ExecutionMode::PackedBatch,
        ExecutionMode::PackedSimd,
        ExecutionMode::Offsets,
    ] {
        for case in fixtures::scalar_cases() {
            compare(case.artifact, &case.args, mode, 6);
        }
        compare(fixtures::nested_call_artifact(), &[], mode, 20);
        compare(fixtures::nested_fault_artifact(), &[], mode, 12);
        compare(fixtures::task_spawn_join_artifact(), &[], mode, 20);
        compare(fixtures::channel_handoff_artifact(), &[], mode, 30);
        compare(
            fixtures::exception_artifact(true, true, false),
            &[],
            mode,
            100,
        );
        for workload in fixtures::performance_workloads() {
            compare(workload.artifact, &[], mode, 30);
        }
    }
}

use crate::execution::prepared::tests::{binary, values};

#[test]
fn all_numeric_opcodes_match_reference_for_overflow_zero_nan_infinity_and_signed_zero() {
    for form in [1, 2, 3, 4, 8, 9] {
        let kind = if form >= 8 { form - 7 } else { form };
        for group in (0..5).chain(7..13) {
            let kinds = [kind, kind, if group >= 7 { 5 } else { kind }];
            let artifact = fixtures::unsigned_operation_artifact(&kinds, 2, binary(group, form));
            for mode in [
                ExecutionMode::Packed,
                ExecutionMode::Slots,
                ExecutionMode::PackedBatch,
                ExecutionMode::PackedSimd,
                ExecutionMode::Offsets,
            ] {
                for lhs in values(kind) {
                    for rhs in values(kind) {
                        let args = [EntryArgument::unowned(lhs), EntryArgument::unowned(rhs)];
                        let mut reference = started(artifact.clone(), &args);
                        let mut candidate = started(artifact.clone(), &args);
                        let stats = candidate.prepare_execution(mode).unwrap();
                        let uses_reference = form >= 8 && (group == 3 || group == 4);
                        assert_eq!(stats.eligible, usize::from(!uses_reference));
                        assert_eq!(
                            reference.run_slice(128, 8).unwrap(),
                            candidate.run_slice(128, 8).unwrap(),
                            "form={form} group={group} lhs={lhs:?} rhs={rhs:?} mode={mode:?}"
                        );
                        assert_eq!(
                            reference.consumed_fixed_cost(),
                            candidate.consumed_fixed_cost()
                        );
                        assert_eq!(
                            reference.executed_instructions(),
                            candidate.executed_instructions()
                        );
                        assert_eq!(
                            reference.retired_instructions(),
                            candidate.retired_instructions()
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn unary_numeric_operations_preserve_raw_moves_and_canonical_negation() {
    for kind in 1..=4 {
        for negation in [false, true] {
            let instruction = if negation {
                Instruction::Neg {
                    form: kind,
                    dst: 1,
                    src: 0,
                }
            } else {
                Instruction::Move { dst: 1, src: 0 }
            };
            let artifact = fixtures::unsigned_operation_artifact(&[kind, kind], 1, instruction);
            for value in values(kind) {
                for mode in [
                    ExecutionMode::Packed,
                    ExecutionMode::Slots,
                    ExecutionMode::PackedBatch,
                    ExecutionMode::PackedSimd,
                    ExecutionMode::Offsets,
                ] {
                    let args = [EntryArgument::unowned(value)];
                    let mut reference = started(artifact.clone(), &args);
                    let mut candidate = started(artifact.clone(), &args);
                    assert_eq!(candidate.prepare_execution(mode).unwrap().eligible, 1);
                    assert_eq!(
                        reference.run_slice(128, 8).unwrap(),
                        candidate.run_slice(128, 8).unwrap()
                    );
                }
            }
        }
    }
}

#[test]
fn candidate_execution_preserves_reference_trace_digests() {
    for case in fixtures::trace_cases() {
        for mode in [
            ExecutionMode::Packed,
            ExecutionMode::Slots,
            ExecutionMode::PackedBatch,
            ExecutionMode::PackedSimd,
            ExecutionMode::Offsets,
        ] {
            let mut reference = fixtures::started(case.artifact.clone(), &case.args);
            let mut candidate = fixtures::started(case.artifact.clone(), &case.args);
            candidate.prepare_execution(mode).unwrap();
            assert_eq!(
                reference.run_slice(case.budget, 8).unwrap(),
                candidate.run_slice(case.budget, 8).unwrap(),
                "{}",
                case.name
            );
            assert_eq!(
                reference.trace_digest(),
                candidate.trace_digest(),
                "{}",
                case.name
            );
        }
    }
}

#[test]
fn wide_registers_use_reference_exit_in_packed_and_direct_offsets_in_prepared() {
    let artifact = fixtures::unsigned_operation_artifact(
        &vec![1; 258],
        2,
        Instruction::Add {
            form: 1,
            dst: 257,
            lhs: 0,
            rhs: 1,
        },
    );
    for mode in [
        ExecutionMode::Packed,
        ExecutionMode::Slots,
        ExecutionMode::PackedBatch,
        ExecutionMode::PackedSimd,
        ExecutionMode::Offsets,
    ] {
        let mut machine = fixtures::started_zero_arg(fixtures::nested_call_artifact());
        let image =
            super::super::image::ExecutionImage::admit(artifact.clone(), fixtures::profile())
                .unwrap();
        let prepared = image.prepare_execution(mode).unwrap();
        assert_eq!(
            prepared.stats.eligible,
            usize::from(mode == ExecutionMode::Offsets)
        );
        let args = [
            EntryArgument::unowned(RuntimeValue::I32(20)),
            EntryArgument::unowned(RuntimeValue::I32(22)),
        ];
        compare(artifact.clone(), &args, mode, 5);
        // Keep an independent machine alive while inspecting aligned code allocation.
        machine.prepare_execution(mode).unwrap();
        let address = match &prepared.code {
            Code::Packed(code) => code.lines.as_ptr() as usize,
            Code::Offsets(code) => code.lines.as_ptr() as usize,
        };
        assert_eq!(address % 64, 0);
    }
}

#[test]
fn prepared_access_rejects_cross_frame_and_overflowing_offsets() {
    let mut arena = FrameArena::new(64).unwrap();
    let frame = FrameReservation {
        base: 0,
        byte_len: 8,
    };
    arena.write_prepared(frame, 0, 42i64.to_le_bytes()).unwrap();
    assert_eq!(
        arena.read_prepared::<8>(frame, 0).unwrap(),
        42i64.to_le_bytes()
    );
    assert!(arena.read_prepared::<4>(frame, 8).is_err());
    assert!(arena.write_prepared(frame, 8, [0u8; 4]).is_err());
    assert!(arena.read_prepared::<8>(frame, u32::MAX).is_err());
    assert!(arena
        .read_prepared::<8>(
            FrameReservation {
                base: u32::MAX,
                byte_len: 8
            },
            0
        )
        .is_err());
}

#[test]
fn physical_slot_encoding_falls_back_when_small_register_ids_have_large_offsets() {
    let artifact = fixtures::unsigned_operation_artifact(
        &vec![4; 130],
        2,
        Instruction::Add {
            form: 4,
            dst: 129,
            lhs: 0,
            rhs: 1,
        },
    );
    let args = [
        EntryArgument::unowned(RuntimeValue::F64(20f64.to_bits())),
        EntryArgument::unowned(RuntimeValue::F64(22f64.to_bits())),
    ];
    for mode in [
        ExecutionMode::Packed,
        ExecutionMode::Slots,
        ExecutionMode::Offsets,
    ] {
        let mut machine = started(artifact.clone(), &args);
        let stats = machine.prepare_execution(mode).unwrap();
        assert_eq!(stats.eligible, usize::from(mode != ExecutionMode::Slots));
        assert_eq!(
            machine.run_slice(128, 8).unwrap(),
            crate::execution::error::Outcome::Halted(Some(RuntimeValue::F64(42f64.to_bits())))
        );
        compare(artifact.clone(), &args, mode, 5);
    }
}
