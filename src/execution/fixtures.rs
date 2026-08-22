use std::sync::Arc;

use crate::{
    artifact::{
        Block, BlockId, ByteRange, Constant, DecodedCode, Function, FunctionId, Instruction,
        NominalType, SwitchCase, TypeId, ValueType,
    },
    verify_artifact, ArtifactLimits, VerifiedArtifact,
};

use super::{
    image::{AdmittedReference, ExecutionImage, ExecutionProfile},
    machine::Machine,
    value::{EntryArgument, ReferenceValue, RegisterValue, RuntimeValue},
    TypeKey,
};

pub(super) fn started(artifact: VerifiedArtifact, args: &[EntryArgument]) -> Machine {
    started_with_arguments_and_profile(artifact, profile(), args)
}

pub(super) fn started_with_profile(
    artifact: VerifiedArtifact,
    profile: ExecutionProfile,
) -> Machine {
    started_with_arguments_and_profile(artifact, profile, &[])
}

fn started_with_arguments_and_profile(
    artifact: VerifiedArtifact,
    profile: ExecutionProfile,
    args: &[EntryArgument],
) -> Machine {
    let image = ExecutionImage::admit(artifact, profile).unwrap();
    let mut machine = Machine::new(image).unwrap();
    machine.start(args).unwrap();
    machine
}

pub(super) fn started_zero_arg(artifact: VerifiedArtifact) -> Machine {
    started(artifact, &[])
}

pub(super) fn nested_call_artifact() -> VerifiedArtifact {
    verified_mutated(|artifact| {
        let i32_type = primitive(1);
        artifact.modules[0].types = vec![
            function_type(i32_type, Vec::new()),
            function_type(i32_type, vec![i32_type]),
            function_type(i32_type, vec![i32_type, i32_type]),
        ];
        artifact.modules[0].declared_types = 3;
        artifact.modules[0].constants = vec![Constant::I32(20), Constant::I32(22)];
        artifact.modules[0].functions = vec![
            function(0, 0, vec![i32_type], 0),
            function(1, 1, vec![i32_type, i32_type], 1),
            function(2, 2, vec![i32_type, i32_type], 2),
        ];
        artifact.modules[0].declared_functions = 3;
        let programs = vec![
            vec![
                Instruction::Const {
                    dst: 0,
                    constant: 0,
                },
                Instruction::CallDirect {
                    dst: 0,
                    function_ref: 1,
                    args: Box::new([0]),
                },
                Instruction::Return { value: 0 },
            ],
            vec![
                Instruction::Const {
                    dst: 1,
                    constant: 1,
                },
                Instruction::CallDirect {
                    dst: 0,
                    function_ref: 2,
                    args: Box::new([0, 1]),
                },
                Instruction::Return { value: 0 },
            ],
            vec![
                Instruction::Add {
                    form: 1,
                    dst: 0,
                    lhs: 0,
                    rhs: 1,
                },
                Instruction::Return { value: 0 },
            ],
        ];
        install_function_blocks(artifact, programs);
        configure_stack(artifact, 2, 3);
    })
}

pub(super) fn recursive_artifact(maximum_call_depth: u32) -> VerifiedArtifact {
    verified_mutated(|artifact| {
        let i32_type = primitive(1);
        artifact.modules[0].types[0] = function_type(primitive(0), Vec::new());
        artifact.modules[0].constants = vec![Constant::I32(7)];
        artifact.modules[0].functions[0] = function(0, 0, vec![i32_type], 0);
        install_function_blocks(
            artifact,
            vec![vec![
                Instruction::Const {
                    dst: 0,
                    constant: 0,
                },
                Instruction::CallDirect {
                    dst: u16::MAX,
                    function_ref: 0,
                    args: Box::new([]),
                },
                Instruction::Return { value: u16::MAX },
            ]],
        );
        configure_stack(artifact, 1, maximum_call_depth);
    })
}

pub(super) fn recursive_pre_call_state() -> Box<[RegisterValue]> {
    Box::new([RegisterValue::Initialized(RuntimeValue::I32(7))])
}

pub(super) fn two_block_artifact(first_cost: u32, second_cost: u32) -> VerifiedArtifact {
    let mut first = (0..first_cost - 1)
        .map(|_| Instruction::Nop)
        .collect::<Vec<_>>();
    first.push(Instruction::Jump { target: 1 });
    let mut second = (0..second_cost - 1)
        .map(|_| Instruction::Nop)
        .collect::<Vec<_>>();
    second.push(Instruction::Return { value: u16::MAX });
    verified_blocks(
        primitive(0),
        0,
        Vec::new(),
        Vec::new(),
        vec![(0, first), (0, second)],
        1,
    )
}

pub(super) fn empty_loop_artifact(cost: u32) -> VerifiedArtifact {
    let mut instructions = (0..cost - 1).map(|_| Instruction::Nop).collect::<Vec<_>>();
    instructions.push(Instruction::Jump { target: 0 });
    verified_blocks(
        primitive(0),
        0,
        Vec::new(),
        Vec::new(),
        vec![(1, instructions)],
        1,
    )
}

pub(super) fn allocation_workloads() -> Vec<VerifiedArtifact> {
    vec![empty_loop_artifact(3), arithmetic_loop_artifact()]
}

pub(super) struct PerformanceWorkload {
    pub name: &'static str,
    pub artifact: VerifiedArtifact,
}

pub(super) fn performance_workloads() -> Vec<PerformanceWorkload> {
    vec![
        PerformanceWorkload {
            name: "hot_integer",
            artifact: arithmetic_loop_artifact(),
        },
        PerformanceWorkload {
            name: "mixed_branch_switch",
            artifact: mixed_control_loop_artifact(),
        },
        PerformanceWorkload {
            name: "nested_direct_calls",
            artifact: nested_call_loop_artifact(),
        },
        PerformanceWorkload {
            name: "empty_quota_loop",
            artifact: empty_loop_artifact(3),
        },
    ]
}

fn arithmetic_loop_artifact() -> VerifiedArtifact {
    verified_blocks(
        primitive(0),
        0,
        vec![primitive(1)],
        vec![Constant::I32(1)],
        vec![(
            1,
            vec![
                Instruction::Const {
                    dst: 0,
                    constant: 0,
                },
                Instruction::Add {
                    form: 1,
                    dst: 0,
                    lhs: 0,
                    rhs: 0,
                },
                Instruction::Jump { target: 0 },
            ],
        )],
        1,
    )
}

fn mixed_control_loop_artifact() -> VerifiedArtifact {
    verified_blocks(
        primitive(0),
        0,
        vec![primitive(5), primitive(1)],
        vec![Constant::I32(1), Constant::Bool(false)],
        vec![
            (
                1,
                vec![
                    Instruction::Const {
                        dst: 0,
                        constant: 1,
                    },
                    Instruction::Const {
                        dst: 1,
                        constant: 0,
                    },
                    Instruction::Branch {
                        condition: 0,
                        true_block: 1,
                        false_block: 2,
                    },
                ],
            ),
            (0, vec![Instruction::Jump { target: 3 }]),
            (
                0,
                vec![Instruction::SwitchI32 {
                    key: 1,
                    default_block: 3,
                    cases: Box::new([SwitchCase {
                        value: 1,
                        target: 3,
                    }]),
                }],
            ),
            (0, vec![Instruction::Jump { target: 0 }]),
        ],
        1,
    )
}

fn nested_call_loop_artifact() -> VerifiedArtifact {
    verified_mutated(|artifact| {
        let i32_type = primitive(1);
        artifact.modules[0].types = vec![
            function_type(primitive(0), Vec::new()),
            function_type(i32_type, vec![i32_type]),
            function_type(i32_type, vec![i32_type, i32_type]),
        ];
        artifact.modules[0].declared_types = 3;
        artifact.modules[0].constants = vec![Constant::I32(20), Constant::I32(22)];
        artifact.modules[0].functions = vec![
            function(0, 0, vec![i32_type], 0),
            function(1, 1, vec![i32_type, i32_type], 1),
            function(2, 2, vec![i32_type, i32_type], 2),
        ];
        artifact.modules[0].declared_functions = 3;
        install_function_blocks(
            artifact,
            vec![
                vec![
                    Instruction::Const {
                        dst: 0,
                        constant: 0,
                    },
                    Instruction::CallDirect {
                        dst: 0,
                        function_ref: 1,
                        args: Box::new([0]),
                    },
                    Instruction::Jump { target: 0 },
                ],
                vec![
                    Instruction::Const {
                        dst: 1,
                        constant: 1,
                    },
                    Instruction::CallDirect {
                        dst: 0,
                        function_ref: 2,
                        args: Box::new([0, 1]),
                    },
                    Instruction::Return { value: 0 },
                ],
                vec![
                    Instruction::Add {
                        form: 1,
                        dst: 0,
                        lhs: 0,
                        rhs: 1,
                    },
                    Instruction::Return { value: 0 },
                ],
            ],
        );
        artifact.modules[0].blocks[0].flags = 1;
        configure_stack(artifact, 2, 3);
    })
}

pub(super) fn trap_after_write_artifact(cost: u32) -> VerifiedArtifact {
    let instructions = vec![
        Instruction::Const {
            dst: 0,
            constant: 1,
        },
        Instruction::Const {
            dst: 1,
            constant: 0,
        },
        Instruction::Div {
            form: 1,
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        Instruction::Return { value: u16::MAX },
    ];
    assert_eq!(
        cost,
        instructions
            .iter()
            .map(|instruction| instruction.fixed_cost().unwrap())
            .sum()
    );
    verified_blocks(
        primitive(0),
        0,
        vec![primitive(1), primitive(1), primitive(1)],
        vec![Constant::I32(0), Constant::I32(1)],
        vec![(0, instructions)],
        1,
    )
}

pub(super) fn pre_trap_registers() -> Box<[RegisterValue]> {
    Box::new([
        RegisterValue::Initialized(RuntimeValue::I32(1)),
        RegisterValue::Initialized(RuntimeValue::I32(0)),
        RegisterValue::Uninitialized,
    ])
}

pub(super) fn branch_switch_artifact(key: i32) -> (VerifiedArtifact, Box<[EntryArgument]>) {
    let artifact = verified_blocks(
        primitive(1),
        2,
        vec![primitive(5), primitive(1), primitive(1)],
        vec![Constant::I32(10), Constant::I32(20), Constant::I32(30)],
        vec![
            (
                0,
                vec![Instruction::Branch {
                    condition: 0,
                    true_block: 1,
                    false_block: 2,
                }],
            ),
            (
                0,
                vec![
                    Instruction::Const {
                        dst: 2,
                        constant: 0,
                    },
                    Instruction::Return { value: 2 },
                ],
            ),
            (
                0,
                vec![Instruction::SwitchI32 {
                    key: 1,
                    default_block: 4,
                    cases: Box::new([SwitchCase {
                        value: 1,
                        target: 3,
                    }]),
                }],
            ),
            (
                0,
                vec![
                    Instruction::Const {
                        dst: 2,
                        constant: 1,
                    },
                    Instruction::Return { value: 2 },
                ],
            ),
            (
                0,
                vec![
                    Instruction::Const {
                        dst: 2,
                        constant: 2,
                    },
                    Instruction::Return { value: 2 },
                ],
            ),
        ],
        1,
    );
    (
        artifact,
        Box::new([
            EntryArgument(RuntimeValue::Bool(key == 0)),
            EntryArgument(RuntimeValue::I32(key)),
        ]),
    )
}

pub(super) struct ScalarCase {
    pub name: &'static str,
    pub artifact: VerifiedArtifact,
    pub args: Box<[EntryArgument]>,
    pub expected: Result<RuntimeValue, super::error::GuestTrap>,
    pub expected_fixed_cost: u64,
}

pub(super) struct TraceCase {
    pub name: &'static str,
    pub artifact: VerifiedArtifact,
    pub args: Box<[EntryArgument]>,
    pub budget: u32,
    pub outcome: super::error::Outcome,
    pub digest: [u8; 32],
    pub fixed_cost: u64,
}

pub(super) fn trace_cases() -> Vec<TraceCase> {
    let scalar = scalar_cases().remove(0);
    let (branch, branch_args) = branch_switch_artifact(0);
    let (switch, switch_args) = branch_switch_artifact(1);
    vec![
        TraceCase {
            name: "straight_line",
            artifact: scalar.artifact,
            args: scalar.args,
            budget: 2,
            outcome: super::error::Outcome::Halted(Some(RuntimeValue::I32(7))),
            digest: [
                166, 84, 34, 161, 100, 88, 173, 25, 105, 181, 10, 183, 116, 180, 193, 205, 85, 40,
                175, 96, 101, 147, 151, 218, 69, 87, 171, 59, 106, 147, 61, 44,
            ],
            fixed_cost: 2,
        },
        TraceCase {
            name: "branch",
            artifact: branch,
            args: branch_args,
            budget: 64,
            outcome: super::error::Outcome::Halted(Some(RuntimeValue::I32(10))),
            digest: [
                172, 177, 184, 35, 78, 58, 68, 53, 75, 168, 170, 148, 208, 224, 98, 38, 46, 123,
                193, 19, 116, 124, 75, 219, 171, 58, 227, 215, 133, 55, 36, 147,
            ],
            fixed_cost: 3,
        },
        TraceCase {
            name: "switch",
            artifact: switch,
            args: switch_args,
            budget: 64,
            outcome: super::error::Outcome::Halted(Some(RuntimeValue::I32(20))),
            digest: [
                158, 20, 181, 111, 155, 172, 189, 72, 253, 154, 121, 182, 128, 246, 244, 47, 106,
                78, 245, 87, 198, 49, 17, 199, 168, 102, 154, 217, 245, 118, 44, 1,
            ],
            fixed_cost: 5,
        },
        TraceCase {
            name: "nested_call",
            artifact: nested_call_artifact(),
            args: Box::new([]),
            budget: 128,
            outcome: super::error::Outcome::Halted(Some(RuntimeValue::I32(42))),
            digest: [
                202, 247, 15, 74, 82, 69, 0, 127, 155, 56, 229, 155, 56, 220, 134, 34, 208, 84, 34,
                165, 24, 106, 30, 112, 49, 52, 209, 183, 224, 14, 221, 147,
            ],
            fixed_cost: 17,
        },
        TraceCase {
            name: "trap",
            artifact: trap_after_write_artifact(7),
            args: Box::new([]),
            budget: 7,
            outcome: super::error::Outcome::Crashed(super::error::GuestTrap::DivisionByZero),
            digest: [
                105, 237, 23, 177, 137, 66, 195, 156, 18, 75, 55, 255, 194, 9, 48, 7, 74, 26, 210,
                125, 152, 194, 149, 199, 44, 88, 177, 128, 54, 199, 205, 48,
            ],
            fixed_cost: 7,
        },
        TraceCase {
            name: "exact_fit",
            artifact: two_block_artifact(3, 5),
            args: Box::new([]),
            budget: 8,
            outcome: super::error::Outcome::Halted(None),
            digest: [
                26, 4, 113, 135, 27, 23, 254, 52, 42, 10, 81, 236, 202, 247, 54, 211, 75, 251, 117,
                40, 139, 21, 195, 158, 137, 160, 93, 117, 6, 22, 172, 78,
            ],
            fixed_cost: 8,
        },
        TraceCase {
            name: "discarded_remainder",
            artifact: two_block_artifact(3, 5),
            args: Box::new([]),
            budget: 7,
            outcome: super::error::Outcome::SliceExhausted,
            digest: [
                76, 219, 106, 116, 17, 37, 160, 129, 145, 182, 101, 200, 118, 224, 228, 252, 24,
                221, 129, 236, 252, 89, 83, 215, 187, 104, 118, 53, 120, 241, 157, 206,
            ],
            fixed_cost: 3,
        },
        TraceCase {
            name: "infinite_loop",
            artifact: empty_loop_artifact(3),
            args: Box::new([]),
            budget: 10,
            outcome: super::error::Outcome::SliceExhausted,
            digest: [
                160, 22, 191, 190, 189, 165, 225, 226, 226, 121, 222, 145, 44, 1, 76, 170, 57, 118,
                61, 83, 10, 64, 160, 248, 9, 184, 71, 57, 71, 225, 71, 173,
            ],
            fixed_cost: 9,
        },
    ]
}

pub(super) fn scalar_cases() -> Vec<ScalarCase> {
    let case = |name, registers: Vec<ValueType>, args: Vec<RuntimeValue>, instruction, expected| {
        let instructions = vec![
            instruction,
            Instruction::Return {
                value: (registers.len() - 1) as u16,
            },
        ];
        let cost = instructions
            .iter()
            .map(|instruction| instruction.fixed_cost().unwrap() as u64)
            .sum();
        ScalarCase {
            name,
            artifact: verified_program(
                registers.last().copied().unwrap(),
                args.len(),
                registers,
                Vec::new(),
                instructions,
            ),
            args: args.into_iter().map(EntryArgument).collect(),
            expected,
            expected_fixed_cost: cost,
        }
    };
    vec![
        case(
            "move_alias",
            vec![primitive(1)],
            vec![RuntimeValue::I32(7)],
            Instruction::Move { dst: 0, src: 0 },
            Ok(RuntimeValue::I32(7)),
        ),
        case(
            "wrapping_add_i32",
            vec![primitive(1), primitive(1), primitive(1)],
            vec![RuntimeValue::I32(i32::MAX), RuntimeValue::I32(1)],
            Instruction::Add {
                form: 1,
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Ok(RuntimeValue::I32(i32::MIN)),
        ),
        case(
            "division_by_zero",
            vec![primitive(2), primitive(2), primitive(2)],
            vec![RuntimeValue::I64(1), RuntimeValue::I64(0)],
            Instruction::Div {
                form: 2,
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Err(super::error::GuestTrap::DivisionByZero),
        ),
        case(
            "canonical_float_remainder_nan",
            vec![primitive(4), primitive(4), primitive(4)],
            vec![
                RuntimeValue::F64(f64::INFINITY.to_bits()),
                RuntimeValue::F64(1.0_f64.to_bits()),
            ],
            Instruction::Rem {
                form: 4,
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Ok(RuntimeValue::F64(super::numeric::CANONICAL_F64_NAN)),
        ),
        case(
            "invalid_character",
            vec![primitive(1), primitive(6)],
            vec![RuntimeValue::I32(0xd800)],
            Instruction::Convert { dst: 1, src: 0 },
            Err(super::error::GuestTrap::InvalidCharacter),
        ),
        case(
            "i64_to_f32_rounding",
            vec![primitive(2), primitive(3)],
            vec![RuntimeValue::I64(16_777_217)],
            Instruction::Convert { dst: 1, src: 0 },
            Ok(RuntimeValue::F32(16_777_216.0_f32.to_bits())),
        ),
        case(
            "f64_to_i32_saturation",
            vec![primitive(4), primitive(1)],
            vec![RuntimeValue::F64(f64::INFINITY.to_bits())],
            Instruction::Convert { dst: 1, src: 0 },
            Ok(RuntimeValue::I32(i32::MAX)),
        ),
        case(
            "char_to_i32",
            vec![primitive(6), primitive(1)],
            vec![RuntimeValue::Char('🦀')],
            Instruction::Convert { dst: 1, src: 0 },
            Ok(RuntimeValue::I32(0x1f980)),
        ),
        case(
            "nan_primitive_equality",
            vec![primitive(3), primitive(3), primitive(5)],
            vec![
                RuntimeValue::F32(f32::NAN.to_bits()),
                RuntimeValue::F32(f32::NAN.to_bits()),
            ],
            Instruction::Equal {
                form: 3,
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Ok(RuntimeValue::Bool(false)),
        ),
    ]
}

pub(super) fn scalar_artifact() -> VerifiedArtifact {
    verified_with_stack(crate::test_support::minimal_vector(), 32)
}

pub(super) fn artifact_with_new_object() -> VerifiedArtifact {
    verified_with_stack(crate::test_support::language_runtime_vector(), 160)
}

pub(super) fn typed_entry_artifact() -> VerifiedArtifact {
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: primitive(0),
            parameters: vec![primitive(1)],
        };
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 1;
        function.parameter_count = 1;
        function.registers = vec![primitive(1)];
        artifact.manifest.required_stack_bytes = 48;
    })
}

pub(super) fn reference_entry_case() -> (
    ExecutionImage,
    RuntimeValue,
    RuntimeValue,
    RuntimeValue,
    RuntimeValue,
) {
    let artifact = verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: primitive(0),
            parameters: vec![ValueType {
                kind: 7,
                flags: 0,
                nominal_type: TypeId(1),
            }],
        };
        artifact.modules[0].types.push(NominalType::Class {
            flags: 0,
            generic_arity: 0,
            name: 0,
            super_type: TypeId(u32::MAX),
            interfaces: Vec::new(),
            field_start: 0,
            field_count: 0,
            method_start: 0,
            method_count: 0,
        });
        artifact.modules[0].declared_types = 2;
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 1;
        function.parameter_count = 1;
        function.registers = vec![ValueType {
            kind: 7,
            flags: 0,
            nominal_type: TypeId(1),
        }];
        artifact.manifest.required_stack_bytes = 48;
    });
    let hash = artifact.content_hash();
    let ty = TypeKey { module: 0, ty: 1 };
    let mut profile = profile();
    profile.host_references = Box::new([
        AdmittedReference {
            ty,
            handle: 1,
            generation: 1,
            live: true,
        },
        AdmittedReference {
            ty,
            handle: 2,
            generation: 1,
            live: false,
        },
    ]);
    let image = ExecutionImage::admit(artifact, profile).unwrap();
    let reference = |image, handle, generation| {
        RuntimeValue::Reference(ReferenceValue {
            image,
            ty,
            handle,
            generation,
        })
    };
    (
        image,
        reference(hash, 1, 1),
        reference([0xff; 32], 1, 1),
        reference(hash, 2, 1),
        reference(hash, 1, 2),
    )
}

pub(super) fn profile() -> ExecutionProfile {
    ExecutionProfile {
        heap_bytes: u32::MAX,
        frame_storage_bytes: 1024 * 1024,
        maximum_call_depth: 64,
        maximum_coroutines: 64,
        maximum_host_requests: 64,
        maximum_events: 64,
        maximum_slice_budget: u32::MAX,
        compiler_abi: [0; 32],
        standard_library_abi: [0; 32],
        capability_mask: u32::MAX,
        host_references: Box::new([]),
    }
}

pub(super) fn profiles_below_each_manifest_limit() -> Vec<ExecutionProfile> {
    let mut profiles = Vec::new();
    let mut compiler = profile();
    compiler.compiler_abi = [1; 32];
    profiles.push(compiler);
    let mut standard_library = profile();
    standard_library.standard_library_abi = [1; 32];
    profiles.push(standard_library);
    let mut frame_storage = profile();
    frame_storage.frame_storage_bytes = 31;
    profiles.push(frame_storage);
    let mut call_depth = profile();
    call_depth.maximum_call_depth = 0;
    profiles.push(call_depth);
    let mut coroutines = profile();
    coroutines.maximum_coroutines = 0;
    profiles.push(coroutines);
    let mut slice = profile();
    slice.maximum_slice_budget = 0;
    profiles.push(slice);
    profiles
}

fn verified_with_stack(mut bytes: Vec<u8>, required_stack_bytes: u32) -> VerifiedArtifact {
    let manifest = section_offset(&bytes, crate::artifact::format::MANIFEST, 0);
    bytes[manifest + 4..manifest + 8].copy_from_slice(&required_stack_bytes.to_le_bytes());
    crate::test_support::rehash(&mut bytes);
    verify_artifact(Arc::from(bytes), ArtifactLimits::default()).unwrap()
}

fn verified_mutated(
    change: impl FnOnce(&mut crate::artifact::DecodedArtifact),
) -> VerifiedArtifact {
    let mut decoded = crate::decode::records::decode_artifact(
        Arc::from(crate::test_support::minimal_vector()),
        &ArtifactLimits::default(),
    )
    .unwrap();
    change(&mut decoded);
    let bytes = crate::test_encode::encode_artifact_rehashed(decoded).unwrap();
    verify_artifact(Arc::from(bytes), ArtifactLimits::default()).unwrap()
}

fn verified_program(
    result: ValueType,
    parameter_count: usize,
    registers: Vec<ValueType>,
    constants: Vec<Constant>,
    instructions: Vec<Instruction>,
) -> VerifiedArtifact {
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result,
            parameters: registers[..parameter_count].to_vec(),
        };
        artifact.modules[0].constants = constants;
        let register_count = registers.len() as u16;
        artifact.modules[0].functions[0].register_count = register_count;
        artifact.modules[0].functions[0].parameter_count = parameter_count as u16;
        artifact.modules[0].functions[0].registers = registers;
        let fixed_cost = instructions
            .iter()
            .map(|instruction| instruction.fixed_cost().unwrap())
            .sum();
        artifact.modules[0].blocks[0].instruction_count = instructions.len() as u32;
        artifact.modules[0].blocks[0].declared_fixed_cost = fixed_cost;
        artifact.modules[0].code[0].instructions = instructions.into_boxed_slice();
        artifact.modules[0].code[0].fixed_cost = fixed_cost;
        artifact.manifest.maximum_block_cost = fixed_cost;
        artifact.manifest.minimum_slice_cost = fixed_cost;
        artifact.manifest.required_stack_bytes =
            super::image::frame_charge(register_count.into()).unwrap() as u32;
    })
}

fn verified_blocks(
    result: ValueType,
    parameter_count: usize,
    registers: Vec<ValueType>,
    constants: Vec<Constant>,
    blocks: Vec<(u32, Vec<Instruction>)>,
    maximum_call_depth: u32,
) -> VerifiedArtifact {
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result,
            parameters: registers[..parameter_count].to_vec(),
        };
        artifact.modules[0].constants = constants;
        let register_count = registers.len() as u16;
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = register_count;
        function.parameter_count = parameter_count as u16;
        function.registers = registers;
        function.first_block = BlockId(0);
        function.block_count = blocks.len() as u32;
        let mut block_records = Vec::new();
        let mut code_records = Vec::new();
        let mut maximum_block_cost = 0;
        for (block_id, (flags, instructions)) in blocks.into_iter().enumerate() {
            let fixed_cost = instructions
                .iter()
                .map(|instruction| instruction.fixed_cost().unwrap())
                .sum();
            maximum_block_cost = maximum_block_cost.max(fixed_cost);
            block_records.push(Block {
                owner_function: FunctionId(0),
                code_record: BlockId(block_id as u32),
                instruction_count: instructions.len() as u32,
                declared_fixed_cost: fixed_cost,
                flags,
            });
            code_records.push(DecodedCode {
                bytes: ByteRange { start: 0, end: 0 },
                instructions: instructions.into_boxed_slice(),
                fixed_cost,
            });
        }
        artifact.modules[0].blocks = block_records;
        artifact.modules[0].code = code_records;
        artifact.manifest.maximum_block_cost = maximum_block_cost;
        artifact.manifest.minimum_slice_cost = maximum_block_cost;
        artifact.manifest.maximum_call_depth = maximum_call_depth;
        artifact.manifest.required_stack_bytes =
            (super::image::frame_charge(register_count.into()).unwrap()
                * u64::from(maximum_call_depth)) as u32;
    })
}

fn function_type(result: ValueType, parameters: Vec<ValueType>) -> NominalType {
    NominalType::Function {
        name: 1,
        flags: 0,
        result,
        parameters,
    }
}

fn function(
    signature: u32,
    parameter_count: u16,
    registers: Vec<ValueType>,
    first_block: u32,
) -> Function {
    Function {
        owner: TypeId(u32::MAX),
        name: 1,
        signature: TypeId(signature),
        flags: 0,
        register_count: registers.len() as u16,
        parameter_count,
        first_block: BlockId(first_block),
        block_count: 1,
        first_exception: 0,
        exception_count: 0,
        registers,
    }
}

fn install_function_blocks(
    artifact: &mut crate::artifact::DecodedArtifact,
    programs: Vec<Vec<Instruction>>,
) {
    let mut blocks = Vec::with_capacity(programs.len());
    let mut code = Vec::with_capacity(programs.len());
    let mut maximum_block_cost = 0;
    for (function_id, instructions) in programs.into_iter().enumerate() {
        let fixed_cost = instructions
            .iter()
            .map(|instruction| instruction.fixed_cost().unwrap())
            .sum();
        maximum_block_cost = maximum_block_cost.max(fixed_cost);
        blocks.push(Block {
            owner_function: FunctionId(function_id as u32),
            code_record: BlockId(function_id as u32),
            instruction_count: instructions.len() as u32,
            declared_fixed_cost: fixed_cost,
            flags: 0,
        });
        code.push(DecodedCode {
            bytes: ByteRange { start: 0, end: 0 },
            instructions: instructions.into_boxed_slice(),
            fixed_cost,
        });
    }
    artifact.modules[0].blocks = blocks;
    artifact.modules[0].code = code;
    artifact.manifest.maximum_block_cost = maximum_block_cost;
    artifact.manifest.minimum_slice_cost = maximum_block_cost;
}

fn configure_stack(
    artifact: &mut crate::artifact::DecodedArtifact,
    registers_per_frame: u64,
    maximum_call_depth: u32,
) {
    artifact.manifest.maximum_call_depth = maximum_call_depth;
    artifact.manifest.required_stack_bytes = (super::image::frame_charge(registers_per_frame)
        .unwrap()
        * u64::from(maximum_call_depth)) as u32;
}

fn primitive(kind: u8) -> ValueType {
    ValueType {
        kind,
        flags: 0,
        nominal_type: TypeId(u32::MAX),
    }
}

fn section_offset(bytes: &[u8], kind: u16, scope: u32) -> usize {
    let count = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
    for index in 0..count {
        let entry = crate::artifact::format::HEADER_SIZE
            + index * crate::artifact::format::DIRECTORY_ENTRY_SIZE;
        let entry_kind = u16::from_le_bytes(bytes[entry..entry + 2].try_into().unwrap());
        let entry_scope = u32::from_le_bytes(bytes[entry + 4..entry + 8].try_into().unwrap());
        if entry_kind == kind && entry_scope == scope {
            return u64::from_le_bytes(bytes[entry + 8..entry + 16].try_into().unwrap()) as usize;
        }
    }
    panic!("fixture section must exist")
}

#[allow(dead_code)]
pub(super) fn reference_type(module: u32, ty: u32) -> TypeKey {
    TypeKey { module, ty }
}
