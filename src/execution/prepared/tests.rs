use super::*;
use crate::artifact::Instruction;
use crate::execution::{
    checkpoint::{Reader, Writer},
    fixtures,
    image::ExecutionImage,
    machine::Machine,
    value::EntryArgument,
};

fn state(machine: &Machine) -> Vec<u8> {
    let mut writer = Writer::new(8 * 1024 * 1024);
    machine.write_checkpoint_state(&mut writer).unwrap();
    writer.finish()
}

fn compare(artifact: crate::VerifiedArtifact, args: &[EntryArgument], steps: usize) {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 16 * 1024;
    let image = ExecutionImage::admit(artifact, profile).unwrap();
    let mut reference = Machine::new_untraced(image.clone()).unwrap();
    let mut prepared = Machine::new_untraced(image).unwrap();
    reference.start(args).unwrap();
    prepared.start(args).unwrap();
    for step in 0..steps {
        reference.use_reference_execution();
        let limit = [0, 1, 2, 1, 3][step % 5];
        let budget = reference.minimum_run_budget().max(128);
        let expected = reference.run_slice_with_retirement_limit(budget, 8, limit);
        let actual = prepared.run_slice_with_retirement_limit(budget, 8, limit);
        assert_eq!(
            format!("{expected:?}"),
            format!("{actual:?}"),
            "step {step}"
        );
        assert_eq!(state(&reference), state(&prepared), "step {step}");
        let encoded = state(&prepared);
        let mut reader = Reader::new(&encoded, 8 * 1024 * 1024, 16 * 1024 * 1024).unwrap();
        prepared.restore_checkpoint_state(&mut reader).unwrap();
        reader.finish().unwrap();
    }
}

#[test]
fn default_executor_matches_reference_at_instruction_and_checkpoint_boundaries() {
    for case in fixtures::scalar_cases() {
        compare(case.artifact, &case.args, 20);
    }
    for workload in fixtures::performance_workloads() {
        compare(workload.artifact, &[], 50);
    }
    for artifact in [
        fixtures::nested_call_artifact(),
        fixtures::nested_fault_artifact(),
        fixtures::task_spawn_join_artifact(),
        fixtures::channel_handoff_artifact(),
        fixtures::exception_artifact(true, true, false),
    ] {
        compare(artifact, &[], 100);
    }
}

#[test]
fn prepared_scalar_frame_bounds_remain_checked() {
    let mut arena = FrameArena::new(64).unwrap();
    let frame = FrameReservation {
        base: 16,
        byte_len: 16,
    };
    for offset in [16, u32::MAX] {
        assert!(arena.read_prepared::<4>(frame, offset).is_err());
        assert!(arena.write_prepared(frame, offset, [0; 4]).is_err());
    }
    assert!(arena
        .read_prepared::<8>(
            FrameReservation {
                base: 60,
                byte_len: 16
            },
            0
        )
        .is_err());
}

#[test]
fn prepared_heap_operations_match_reference_results_traps_and_checkpoints() {
    for (artifact, _) in fixtures::primitive_array_roundtrip_cases() {
        compare(artifact, &[], 60);
    }
    for set in [false, true] {
        compare(
            fixtures::static_roundtrip_artifact(),
            &[
                EntryArgument::unowned(RuntimeValue::Bool(set)),
                EntryArgument::unowned(RuntimeValue::I32(42)),
            ],
            100,
        );
    }
    for index in [-1, 0, 1, i32::MAX] {
        let args = [EntryArgument::unowned(RuntimeValue::I32(index))];
        compare(fixtures::array_bounds_artifact(), &args, 60);
        compare(fixtures::failed_array_store_artifact(), &args, 60);
    }
    for artifact in [
        fixtures::field_roundtrip_artifact(),
        fixtures::reference_field_roundtrip_artifact(),
        fixtures::reference_array_roundtrip_artifact(),
        fixtures::nonnull_zero_field_artifact(),
        fixtures::lazy_type_initializer_artifact(),
        fixtures::actively_used_failing_type_initializer_artifact(),
    ] {
        compare(artifact, &[], 100);
    }
}

#[test]
fn prepared_inline_values_preserve_components_references_calls_and_tasks() {
    for artifact in [
        fixtures::inline_value_artifact(0),
        fixtures::inline_value_artifact(1),
        fixtures::inline_reference_root_artifact(),
        fixtures::inline_task_argument_artifact(),
    ] {
        compare(artifact, &[], 100);
    }
}

pub(in crate::execution) fn binary(group: u32, form: u8) -> Instruction {
    match group {
        0 => Instruction::Add {
            form,
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        1 => Instruction::Sub {
            form,
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        2 => Instruction::Mul {
            form,
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        3 => Instruction::Div {
            form,
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        4 => Instruction::Rem {
            form,
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        7 => Instruction::Equal {
            form,
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        8 => Instruction::NotEqual {
            form,
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        9 => Instruction::Less {
            form,
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        10 => Instruction::LessEqual {
            form,
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        11 => Instruction::Greater {
            form,
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        12 => Instruction::GreaterEqual {
            form,
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        _ => unreachable!(),
    }
}

pub(in crate::execution) fn values(kind: u8) -> Vec<RuntimeValue> {
    match kind {
        1 => [0, 1, -1, i32::MIN, i32::MAX]
            .into_iter()
            .map(RuntimeValue::I32)
            .collect(),
        2 => [0, 1, -1, i64::MIN, i64::MAX]
            .into_iter()
            .map(RuntimeValue::I64)
            .collect(),
        3 => [
            0,
            0x8000_0000,
            0x3fc0_0000,
            0x7f80_0000,
            0xff80_0000,
            0x7fc0_1234,
        ]
        .into_iter()
        .map(RuntimeValue::F32)
        .collect(),
        4 => [
            0,
            0x8000_0000_0000_0000,
            0x3ff8_0000_0000_0000,
            0x7ff0_0000_0000_0000,
            0xfff0_0000_0000_0000,
            0x7ff8_0000_0000_1234,
        ]
        .into_iter()
        .map(RuntimeValue::F64)
        .collect(),
        _ => unreachable!(),
    }
}

#[test]
fn default_numeric_operations_match_reference_on_integer_and_float_edges() {
    for form in [1, 2, 3, 4, 8, 9] {
        let kind = if form >= 8 { form - 7 } else { form };
        for group in (0..5).chain(7..13) {
            let artifact = fixtures::unsigned_operation_artifact(
                &[kind, kind, if group >= 7 { 5 } else { kind }],
                2,
                binary(group, form),
            );
            let mut profile = fixtures::profile();
            profile.heap_bytes = 16 * 1024;
            let image = ExecutionImage::admit(artifact, profile).unwrap();
            for lhs in values(kind) {
                for rhs in values(kind) {
                    let args = [EntryArgument::unowned(lhs), EntryArgument::unowned(rhs)];
                    let mut reference = Machine::new_untraced(image.clone()).unwrap();
                    let mut prepared = Machine::new_untraced(image.clone()).unwrap();
                    reference.use_reference_execution();
                    reference.start(&args).unwrap();
                    prepared.start(&args).unwrap();
                    assert_eq!(
                        reference.run_slice(128, 8).unwrap(),
                        prepared.run_slice(128, 8).unwrap(),
                        "form={form} group={group} lhs={lhs:?} rhs={rhs:?}"
                    );
                    assert_eq!(
                        reference.consumed_fixed_cost(),
                        prepared.consumed_fixed_cost()
                    );
                    assert_eq!(
                        reference.executed_instructions(),
                        prepared.executed_instructions()
                    );
                    assert_eq!(
                        reference.retired_instructions(),
                        prepared.retired_instructions()
                    );
                }
            }
        }
    }
}
