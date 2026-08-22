use std::sync::Arc;

use crate::{
    artifact::{
        Block, BlockId, ByteRange, Constant, DecodedCode, Export, Field, Function, FunctionId,
        Import, Instruction, ModuleId, NominalType, SwitchCase, TypeId, Utf16LiteralId, ValueType,
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

pub(super) fn capability_artifact(
    asynchronous: bool,
    required: bool,
    operation_count: u32,
    operation: u32,
) -> VerifiedArtifact {
    verified_mutated(|artifact| {
        artifact.capabilities.push(crate::artifact::Capability {
            namespace: 0,
            name: 1,
            abi_major: 1,
            minimum_abi_minor: 2,
            flags: if required { 1 } else { 2 },
            operation_count,
        });
        artifact.header.semantic_features = if asynchronous { 0b110 } else { 0b100 };
        artifact.manifest.required_capabilities = u32::from(required);
        artifact.manifest.optional_capabilities = u32::from(!required);
        artifact.manifest.maximum_host_requests = 1;
        artifact.manifest.required_stack_bytes = 32;
        artifact.modules[0].functions[0].flags = u32::from(asynchronous);
        let NominalType::Function { flags, .. } = &mut artifact.modules[0].types[0] else {
            unreachable!();
        };
        *flags = u16::from(asynchronous);
        let function = &mut artifact.modules[0].functions[0];
        function.first_block = BlockId(0);
        function.block_count = if asynchronous { 2 } else { 1 };
        let call = if asynchronous {
            Instruction::CapabilityCallAsync {
                dst: u16::MAX,
                capability: 0,
                operation,
                args: Box::new([]),
                resume_block: 1,
            }
        } else {
            Instruction::CapabilityCallSync {
                dst: u16::MAX,
                capability: 0,
                operation,
                args: Box::new([]),
            }
        };
        let mut first = vec![call];
        if !asynchronous {
            first.push(Instruction::Return { value: u16::MAX });
        }
        let first_cost = first
            .iter()
            .map(|instruction| instruction.fixed_cost().unwrap())
            .sum();
        artifact.modules[0].blocks[0].instruction_count = first.len() as u32;
        artifact.modules[0].blocks[0].declared_fixed_cost = first_cost;
        artifact.modules[0].code[0].instructions = first.into_boxed_slice();
        artifact.modules[0].code[0].fixed_cost = first_cost;
        if asynchronous {
            artifact.modules[0].blocks.push(Block {
                owner_function: FunctionId(0),
                code_record: BlockId(1),
                instruction_count: 1,
                declared_fixed_cost: 1,
                flags: 0,
            });
            artifact.modules[0].code.push(DecodedCode {
                bytes: ByteRange { start: 0, end: 0 },
                instructions: vec![Instruction::Return { value: u16::MAX }].into_boxed_slice(),
                fixed_cost: 1,
            });
        }
        artifact.manifest.maximum_block_cost = first_cost;
        artifact.manifest.minimum_slice_cost = artifact.manifest.maximum_block_cost;
    })
}

pub(super) fn scalar_capability_artifact(
    value_type: ValueType,
    constant: Constant,
) -> VerifiedArtifact {
    verified_mutated(|artifact| {
        artifact.header.semantic_features = 0b110;
        artifact.capabilities.push(crate::artifact::Capability {
            namespace: 0,
            name: 1,
            abi_major: 1,
            minimum_abi_minor: 0,
            flags: 1,
            operation_count: 1,
        });
        artifact.manifest.required_capabilities = 1;
        artifact.manifest.maximum_host_requests = 1;
        artifact.manifest.required_stack_bytes = 64;
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 1,
            result: value_type,
            parameters: Vec::new(),
        };
        artifact.modules[0].constants = vec![constant];
        let function = &mut artifact.modules[0].functions[0];
        function.flags = 1;
        function.register_count = 2;
        function.parameter_count = 0;
        function.registers = vec![value_type, value_type];
        function.first_block = BlockId(0);
        function.block_count = 2;
        let first = vec![
            Instruction::Const {
                dst: 0,
                constant: 0,
            },
            Instruction::CapabilityCallAsync {
                dst: 1,
                capability: 0,
                operation: 0,
                args: vec![0].into_boxed_slice(),
                resume_block: 1,
            },
        ];
        let first_cost = first
            .iter()
            .map(|instruction| instruction.fixed_cost().unwrap())
            .sum();
        artifact.modules[0].blocks = vec![
            Block {
                owner_function: FunctionId(0),
                code_record: BlockId(0),
                instruction_count: first.len() as u32,
                declared_fixed_cost: first_cost,
                flags: 0,
            },
            Block {
                owner_function: FunctionId(0),
                code_record: BlockId(1),
                instruction_count: 1,
                declared_fixed_cost: 1,
                flags: 0,
            },
        ];
        artifact.modules[0].code = vec![
            DecodedCode {
                bytes: ByteRange { start: 0, end: 0 },
                instructions: first.into_boxed_slice(),
                fixed_cost: first_cost,
            },
            DecodedCode {
                bytes: ByteRange { start: 0, end: 0 },
                instructions: vec![Instruction::Return { value: 1 }].into_boxed_slice(),
                fixed_cost: 1,
            },
        ];
        artifact.manifest.maximum_block_cost = first_cost;
        artifact.manifest.minimum_slice_cost = first_cost;
    })
}

pub(super) fn two_unit_capability_calls_artifact() -> VerifiedArtifact {
    verified_mutated(|artifact| {
        artifact.header.semantic_features = 0b110;
        artifact.capabilities.push(crate::artifact::Capability {
            namespace: 0,
            name: 1,
            abi_major: 1,
            minimum_abi_minor: 0,
            flags: 1,
            operation_count: 1,
        });
        artifact.manifest.required_capabilities = 1;
        artifact.manifest.maximum_host_requests = 2;
        artifact.manifest.required_stack_bytes = 32;
        let NominalType::Function { flags, .. } = &mut artifact.modules[0].types[0] else {
            unreachable!();
        };
        *flags = 1;
        let function = &mut artifact.modules[0].functions[0];
        function.flags = 1;
        function.first_block = BlockId(0);
        function.block_count = 3;
        let call = |resume_block| Instruction::CapabilityCallAsync {
            dst: u16::MAX,
            capability: 0,
            operation: 0,
            args: Box::new([]),
            resume_block,
        };
        let programs = [
            vec![call(1)],
            vec![call(2)],
            vec![Instruction::Return { value: u16::MAX }],
        ];
        artifact.modules[0].blocks = programs
            .iter()
            .enumerate()
            .map(|(index, instructions)| Block {
                owner_function: FunctionId(0),
                code_record: BlockId(index as u32),
                instruction_count: instructions.len() as u32,
                declared_fixed_cost: instructions[0].fixed_cost().unwrap(),
                flags: 0,
            })
            .collect();
        artifact.modules[0].code = programs
            .into_iter()
            .map(|instructions| DecodedCode {
                bytes: ByteRange { start: 0, end: 0 },
                fixed_cost: instructions[0].fixed_cost().unwrap(),
                instructions: instructions.into_boxed_slice(),
            })
            .collect();
        artifact.manifest.maximum_block_cost = artifact.modules[0]
            .code
            .iter()
            .map(|code| code.fixed_cost)
            .max()
            .unwrap();
        artifact.manifest.minimum_slice_cost = artifact.manifest.maximum_block_cost;
    })
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
            EntryArgument::unowned(RuntimeValue::Bool(key == 0)),
            EntryArgument::unowned(RuntimeValue::I32(key)),
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
                210, 97, 93, 138, 111, 54, 126, 53, 10, 37, 45, 198, 192, 27, 212, 174, 59, 165,
                154, 74, 150, 26, 207, 5, 120, 25, 252, 251, 187, 38, 243, 161,
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
                60, 68, 50, 190, 195, 34, 207, 80, 192, 210, 247, 156, 55, 119, 238, 51, 173, 210,
                119, 78, 5, 155, 156, 243, 49, 36, 233, 82, 96, 197, 162, 114,
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
                76, 169, 238, 254, 205, 63, 208, 233, 222, 142, 242, 6, 127, 193, 101, 114, 49, 19,
                172, 203, 186, 236, 88, 43, 75, 42, 148, 32, 187, 206, 13, 20,
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
                93, 211, 170, 141, 159, 215, 165, 184, 12, 97, 91, 119, 9, 235, 35, 229, 40, 117,
                32, 8, 215, 140, 224, 81, 73, 12, 173, 251, 156, 94, 20, 157,
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
                119, 50, 19, 43, 241, 140, 29, 222, 192, 224, 117, 237, 160, 122, 174, 22, 139,
                135, 46, 235, 204, 215, 249, 198, 74, 121, 177, 198, 31, 116, 67, 199,
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
                217, 218, 127, 86, 12, 61, 234, 142, 196, 9, 58, 58, 145, 71, 116, 101, 8, 126,
                144, 104, 213, 32, 185, 216, 158, 118, 35, 208, 92, 128, 13, 155,
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
                232, 248, 68, 1, 178, 67, 15, 167, 210, 241, 173, 87, 16, 75, 185, 128, 253, 212,
                217, 71, 18, 167, 204, 181, 126, 203, 39, 113, 39, 61, 136, 127,
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
                97, 66, 140, 5, 73, 113, 159, 198, 178, 206, 33, 114, 180, 166, 99, 191, 35, 175,
                246, 164, 251, 169, 175, 54, 117, 38, 209, 143, 47, 193, 94, 78,
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
            args: args.into_iter().map(EntryArgument::unowned).collect(),
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
            "surrogate_character",
            vec![primitive(1), primitive(6)],
            vec![RuntimeValue::I32(0xd800)],
            Instruction::Convert { dst: 1, src: 0 },
            Ok(RuntimeValue::Char(0xd800)),
        ),
        case(
            "negative_i32_truncates_to_char",
            vec![primitive(1), primitive(6)],
            vec![RuntimeValue::I32(-1)],
            Instruction::Convert { dst: 1, src: 0 },
            Ok(RuntimeValue::Char(0xffff)),
        ),
        case(
            "i32_low_sixteen_bits_wrap_to_char",
            vec![primitive(1), primitive(6)],
            vec![RuntimeValue::I32(65_536)],
            Instruction::Convert { dst: 1, src: 0 },
            Ok(RuntimeValue::Char(0x0000)),
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
            vec![RuntimeValue::Char(0xd800)],
            Instruction::Convert { dst: 1, src: 0 },
            Ok(RuntimeValue::I32(0xd800)),
        ),
        case(
            "char_order_is_unsigned_u16",
            vec![primitive(6), primitive(6), primitive(5)],
            vec![RuntimeValue::Char(0xdfff), RuntimeValue::Char(0xe000)],
            Instruction::Less {
                form: 6,
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
            Ok(RuntimeValue::Bool(true)),
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

pub(super) fn literal_string_artifact() -> VerifiedArtifact {
    literal_string_program(
        ValueType {
            kind: 7,
            flags: 0,
            nominal_type: TypeId(0x8000_0000),
        },
        vec![ValueType {
            kind: 7,
            flags: 0,
            nominal_type: TypeId(0x8000_0000),
        }],
        Vec::new(),
        &[0x48, 0x69],
        vec![
            Instruction::Const {
                dst: 0,
                constant: 0,
            },
            Instruction::Return { value: 0 },
        ],
    )
}

pub(super) fn literal_string_length_artifact() -> VerifiedArtifact {
    let string = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(0x8000_0000),
    };
    literal_string_program(
        primitive(1),
        vec![string, primitive(1)],
        Vec::new(),
        &[0x48, 0x69],
        vec![
            Instruction::Const {
                dst: 0,
                constant: 0,
            },
            Instruction::StringLength { dst: 1, string: 0 },
            Instruction::Return { value: 1 },
        ],
    )
}

pub(super) fn literal_string_get_artifact(index: i32) -> VerifiedArtifact {
    let string = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(0x8000_0000),
    };
    literal_string_program(
        primitive(6),
        vec![string, primitive(1), primitive(6)],
        vec![Constant::I32(index)],
        &[0x48, 0x69],
        vec![
            Instruction::Const {
                dst: 0,
                constant: 1,
            },
            Instruction::Const {
                dst: 1,
                constant: 0,
            },
            Instruction::StringGet {
                dst: 2,
                string: 0,
                index: 1,
            },
            Instruction::Return { value: 2 },
        ],
    )
}

pub(super) fn literal_string_equals_artifact() -> VerifiedArtifact {
    literal_string_binary_artifact(
        primitive(5),
        primitive(5),
        Instruction::StringEquals {
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
    )
}

pub(super) fn literal_string_compare_artifact() -> VerifiedArtifact {
    literal_string_binary_artifact(
        primitive(1),
        primitive(1),
        Instruction::StringCompare {
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
    )
}

fn literal_string_binary_artifact(
    result: ValueType,
    result_register: ValueType,
    operation: Instruction,
) -> VerifiedArtifact {
    let string = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(0x8000_0000),
    };
    literal_string_program(
        result,
        vec![string, string, result_register],
        Vec::new(),
        &[0x48, 0x69],
        vec![
            Instruction::Const {
                dst: 0,
                constant: 0,
            },
            Instruction::Const {
                dst: 1,
                constant: 0,
            },
            operation,
            Instruction::Return { value: 2 },
        ],
    )
}

pub(super) fn literal_string_hash_artifact() -> VerifiedArtifact {
    let string = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(0x8000_0000),
    };
    literal_string_program(
        primitive(1),
        vec![string, primitive(1)],
        Vec::new(),
        &[0x48, 0x69],
        vec![
            Instruction::Const {
                dst: 0,
                constant: 0,
            },
            Instruction::StringHash { dst: 1, string: 0 },
            Instruction::Return { value: 1 },
        ],
    )
}

pub(super) fn long_literal_string_hash_artifact(code_units: &[u16]) -> VerifiedArtifact {
    let string = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(0x8000_0000),
    };
    literal_string_program(
        primitive(1),
        vec![string, primitive(1)],
        Vec::new(),
        code_units,
        vec![
            Instruction::Const {
                dst: 0,
                constant: 0,
            },
            Instruction::StringHash { dst: 1, string: 0 },
            Instruction::Return { value: 1 },
        ],
    )
}

pub(super) fn literal_string_concat_artifact() -> VerifiedArtifact {
    literal_string_concat_units_artifact(&[0x48, 0x69])
}

pub(super) fn literal_string_concat_units_artifact(code_units: &[u16]) -> VerifiedArtifact {
    let string = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(0x8000_0000),
    };
    literal_string_program_blocks(
        string,
        vec![string, string, string],
        Vec::new(),
        code_units,
        vec![
            vec![
                Instruction::Const {
                    dst: 0,
                    constant: 0,
                },
                Instruction::Const {
                    dst: 1,
                    constant: 0,
                },
                Instruction::Jump { target: 1 },
            ],
            vec![
                Instruction::StringConcat {
                    dst: 2,
                    lhs: 0,
                    rhs: 1,
                },
                Instruction::Return { value: 2 },
            ],
        ],
    )
}

pub(super) fn literal_string_substring_artifact(start: i32, end: i32) -> VerifiedArtifact {
    let string = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(0x8000_0000),
    };
    let mut bounds = vec![start, end];
    bounds.sort_unstable_by_key(|value| value.to_le_bytes());
    bounds.dedup();
    let start_constant = bounds.iter().position(|value| *value == start).unwrap() as u32;
    let end_constant = bounds.iter().position(|value| *value == end).unwrap() as u32;
    let string_constant = bounds.len() as u32;
    literal_string_program_blocks(
        string,
        vec![string, primitive(1), primitive(1), string],
        bounds.into_iter().map(Constant::I32).collect(),
        &[0x48, 0x69],
        vec![
            vec![
                Instruction::Const {
                    dst: 0,
                    constant: string_constant,
                },
                Instruction::Const {
                    dst: 1,
                    constant: start_constant,
                },
                Instruction::Const {
                    dst: 2,
                    constant: end_constant,
                },
                Instruction::Jump { target: 1 },
            ],
            vec![
                Instruction::StringSubstring {
                    dst: 3,
                    string: 0,
                    start: 1,
                    end: 2,
                },
                Instruction::Return { value: 3 },
            ],
        ],
    )
}

pub(super) fn repeated_concat_artifact(content_equality: bool) -> VerifiedArtifact {
    let string = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(0x8000_0000),
    };
    let comparison = if content_equality {
        Instruction::StringEquals {
            dst: 4,
            lhs: 2,
            rhs: 3,
        }
    } else {
        Instruction::RefNotEqual {
            dst: 4,
            lhs: 2,
            rhs: 3,
        }
    };
    literal_string_program_blocks(
        primitive(5),
        vec![string, string, string, string, primitive(5)],
        Vec::new(),
        &[0x48, 0x69],
        vec![
            vec![
                Instruction::Const {
                    dst: 0,
                    constant: 0,
                },
                Instruction::Const {
                    dst: 1,
                    constant: 0,
                },
                Instruction::Jump { target: 1 },
            ],
            vec![
                Instruction::StringConcat {
                    dst: 2,
                    lhs: 0,
                    rhs: 1,
                },
                Instruction::Jump { target: 2 },
            ],
            vec![
                Instruction::StringConcat {
                    dst: 3,
                    lhs: 0,
                    rhs: 1,
                },
                Instruction::Jump { target: 3 },
            ],
            vec![comparison, Instruction::Return { value: 4 }],
        ],
    )
}

pub(super) fn unsigned_dynamic_string_compare_artifact() -> VerifiedArtifact {
    let string = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(0x8000_0000),
    };
    literal_string_program_blocks(
        primitive(1),
        vec![
            string,
            primitive(1),
            primitive(1),
            string,
            string,
            primitive(1),
        ],
        vec![Constant::I32(0), Constant::I32(1), Constant::I32(2)],
        &[0xdfff, 0xe000],
        vec![
            vec![
                Instruction::Const {
                    dst: 0,
                    constant: 3,
                },
                Instruction::Const {
                    dst: 1,
                    constant: 0,
                },
                Instruction::Const {
                    dst: 2,
                    constant: 1,
                },
                Instruction::Jump { target: 1 },
            ],
            vec![
                Instruction::StringSubstring {
                    dst: 3,
                    string: 0,
                    start: 1,
                    end: 2,
                },
                Instruction::Jump { target: 2 },
            ],
            vec![
                Instruction::Const {
                    dst: 1,
                    constant: 1,
                },
                Instruction::Const {
                    dst: 2,
                    constant: 2,
                },
                Instruction::Jump { target: 3 },
            ],
            vec![
                Instruction::StringSubstring {
                    dst: 4,
                    string: 0,
                    start: 1,
                    end: 2,
                },
                Instruction::Jump { target: 4 },
            ],
            vec![
                Instruction::StringCompare {
                    dst: 5,
                    lhs: 3,
                    rhs: 4,
                },
                Instruction::Return { value: 5 },
            ],
        ],
    )
}

fn literal_string_program(
    result: ValueType,
    registers: Vec<ValueType>,
    extra_constants: Vec<Constant>,
    literal_code_units: &[u16],
    instructions: Vec<Instruction>,
) -> VerifiedArtifact {
    literal_string_program_blocks(
        result,
        registers,
        extra_constants,
        literal_code_units,
        vec![instructions],
    )
}

fn literal_string_program_blocks(
    result: ValueType,
    registers: Vec<ValueType>,
    extra_constants: Vec<Constant>,
    literal_code_units: &[u16],
    programs: Vec<Vec<Instruction>>,
) -> VerifiedArtifact {
    literal_string_program_blocks_configured(
        result,
        registers,
        extra_constants,
        literal_code_units,
        programs,
        |_| {},
    )
}

fn literal_string_program_blocks_configured(
    result: ValueType,
    registers: Vec<ValueType>,
    extra_constants: Vec<Constant>,
    literal_code_units: &[u16],
    programs: Vec<Vec<Instruction>>,
    configure: impl FnOnce(&mut crate::artifact::DecodedArtifact),
) -> VerifiedArtifact {
    let mut decoded = crate::decode::records::decode_artifact(
        Arc::from(crate::test_support::two_module_vector()),
        &ArtifactLimits::default(),
    )
    .unwrap();
    let mut bytes = decoded.bytes.to_vec();
    let library_name_start = bytes.len();
    bytes.extend_from_slice(b"aaa");
    let library_name_end = bytes.len();
    let function_name_start = bytes.len();
    bytes.extend_from_slice(b"bbb");
    let function_name_end = bytes.len();
    let name_start = bytes.len();
    bytes.extend_from_slice(b"kotlin.String");
    let name_end = bytes.len();
    let literal_start = bytes.len();
    for code_unit in literal_code_units {
        bytes.extend_from_slice(&code_unit.to_le_bytes());
    }
    let literal_end = bytes.len();
    decoded.bytes = Arc::from(bytes);

    let library = &mut decoded.modules[1];
    library.strings[0] = ByteRange {
        start: library_name_start,
        end: library_name_end,
    };
    library.strings[1] = ByteRange {
        start: function_name_start,
        end: function_name_end,
    };
    let string_name = library.strings.len() as u32;
    library.strings.push(ByteRange {
        start: name_start,
        end: name_end,
    });
    library.types.push(NominalType::Class {
        flags: 2,
        generic_arity: 0,
        name: string_name,
        super_type: TypeId(u32::MAX),
        interfaces: Vec::new(),
        field_start: 0,
        field_count: 0,
        method_start: 0,
        method_count: 0,
    });
    library.exports.insert(
        0,
        Export {
            kind: 0,
            visibility: 1,
            name: string_name,
            local_symbol: 1,
            signature: TypeId(1),
        },
    );
    library.declared_types = 2;
    library.declared_exports = 2;

    let application = &mut decoded.modules[0];
    application.imports.clear();
    application.imports.push(Import {
        kind: 0,
        target_module: ModuleId(1),
        target_name: string_name,
        expected_signature: TypeId(0x8000_0000),
        target_hash: [0; 32],
    });
    application.declared_imports = 1;
    application.utf16_literals.push(ByteRange {
        start: literal_start,
        end: literal_end,
    });
    application.constants = extra_constants;
    application
        .constants
        .push(Constant::String(Utf16LiteralId(0)));
    let register_count = registers.len();
    application.types[0] = NominalType::Function {
        name: 1,
        flags: 0,
        result,
        parameters: Vec::new(),
    };
    let function = &mut application.functions[0];
    function.register_count = register_count as u16;
    function.parameter_count = 0;
    function.registers = registers;
    function.first_block = BlockId(0);
    function.block_count = programs.len() as u32;
    application.blocks.clear();
    application.code.clear();
    let mut maximum_block_cost = 0;
    for (block_id, instructions) in programs.into_iter().enumerate() {
        let fixed_cost = instructions
            .iter()
            .map(|instruction| instruction.fixed_cost().unwrap())
            .sum();
        maximum_block_cost = maximum_block_cost.max(fixed_cost);
        application.blocks.push(Block {
            owner_function: FunctionId(0),
            code_record: BlockId(block_id as u32),
            instruction_count: instructions.len() as u32,
            declared_fixed_cost: fixed_cost,
            flags: 1,
        });
        application.code.push(DecodedCode {
            bytes: ByteRange { start: 0, end: 0 },
            instructions: instructions.into_boxed_slice(),
            fixed_cost,
        });
    }
    decoded.manifest.maximum_block_cost = maximum_block_cost;
    decoded.manifest.minimum_slice_cost = maximum_block_cost;
    decoded.manifest.required_stack_bytes =
        super::image::frame_charge(register_count as u64).unwrap() as u32;
    configure(&mut decoded);

    let bytes = crate::test_encode::encode_artifact_rehashed(decoded).unwrap();
    verify_artifact(Arc::from(bytes), ArtifactLimits::default()).unwrap()
}

pub(super) fn string_capability_artifact(
    code_units: &[u16],
    dynamic: bool,
    duplicate_argument: bool,
) -> VerifiedArtifact {
    let string = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(0x8000_0000),
    };
    let (registers, programs, argument_register) = if dynamic {
        (
            vec![string, string, string],
            vec![
                vec![
                    Instruction::Const {
                        dst: 0,
                        constant: 0,
                    },
                    Instruction::Const {
                        dst: 1,
                        constant: 0,
                    },
                    Instruction::Jump { target: 1 },
                ],
                vec![
                    Instruction::StringConcat {
                        dst: 2,
                        lhs: 0,
                        rhs: 1,
                    },
                    Instruction::Jump { target: 2 },
                ],
                vec![Instruction::CapabilityCallAsync {
                    dst: u16::MAX,
                    capability: 0,
                    operation: 0,
                    args: if duplicate_argument {
                        vec![2, 2]
                    } else {
                        vec![2]
                    }
                    .into_boxed_slice(),
                    resume_block: 3,
                }],
                vec![Instruction::Return { value: u16::MAX }],
            ],
            2,
        )
    } else {
        (
            vec![string],
            vec![
                vec![
                    Instruction::Const {
                        dst: 0,
                        constant: 0,
                    },
                    Instruction::CapabilityCallAsync {
                        dst: u16::MAX,
                        capability: 0,
                        operation: 0,
                        args: if duplicate_argument {
                            vec![0, 0]
                        } else {
                            vec![0]
                        }
                        .into_boxed_slice(),
                        resume_block: 1,
                    },
                ],
                vec![Instruction::Return { value: u16::MAX }],
            ],
            0,
        )
    };
    let _ = argument_register;
    literal_string_program_blocks_configured(
        primitive(0),
        registers,
        Vec::new(),
        code_units,
        programs,
        |artifact| {
            artifact.header.semantic_features = 0b1110;
            artifact.capabilities.push(crate::artifact::Capability {
                namespace: 0,
                name: 1,
                abi_major: 1,
                minimum_abi_minor: 0,
                flags: 1,
                operation_count: 1,
            });
            artifact.manifest.required_capabilities = 1;
            artifact.manifest.maximum_host_requests = 1;
            let NominalType::Function { flags, .. } = &mut artifact.modules[0].types[0] else {
                unreachable!();
            };
            *flags = 1;
            artifact.modules[0].functions[0].flags = 1;
        },
    )
}

pub(super) fn string_response_capability_artifact() -> VerifiedArtifact {
    let string = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(0x8000_0000),
    };
    literal_string_program_blocks_configured(
        primitive(1),
        vec![string, primitive(1)],
        Vec::new(),
        &[],
        vec![
            vec![Instruction::CapabilityCallAsync {
                dst: 0,
                capability: 0,
                operation: 0,
                args: Box::new([]),
                resume_block: 1,
            }],
            vec![
                Instruction::StringHash { dst: 1, string: 0 },
                Instruction::Return { value: 1 },
            ],
        ],
        |artifact| {
            artifact.header.semantic_features = 0b1110;
            artifact.capabilities.push(crate::artifact::Capability {
                namespace: 0,
                name: 1,
                abi_major: 1,
                minimum_abi_minor: 0,
                flags: 1,
                operation_count: 1,
            });
            artifact.manifest.required_capabilities = 1;
            artifact.manifest.maximum_host_requests = 1;
            let NominalType::Function { flags, .. } = &mut artifact.modules[0].types[0] else {
                unreachable!();
            };
            *flags = 1;
            artifact.modules[0].functions[0].flags = 1;
        },
    )
}

pub(super) fn string_response_gc_retry_artifact() -> VerifiedArtifact {
    let string = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(0x8000_0000),
    };
    literal_string_program_blocks_configured(
        primitive(1),
        vec![string, string, string, string, primitive(1)],
        Vec::new(),
        &[0x0100; 8],
        vec![
            vec![
                Instruction::Const {
                    dst: 0,
                    constant: 0,
                },
                Instruction::Const {
                    dst: 1,
                    constant: 0,
                },
                Instruction::Jump { target: 1 },
            ],
            vec![
                Instruction::StringConcat {
                    dst: 2,
                    lhs: 0,
                    rhs: 1,
                },
                Instruction::Jump { target: 2 },
            ],
            vec![
                Instruction::Const {
                    dst: 2,
                    constant: 0,
                },
                Instruction::CapabilityCallAsync {
                    dst: 3,
                    capability: 0,
                    operation: 0,
                    args: Box::new([]),
                    resume_block: 3,
                },
            ],
            vec![
                Instruction::StringHash { dst: 4, string: 3 },
                Instruction::Return { value: 4 },
            ],
        ],
        |artifact| {
            artifact.header.semantic_features = 0b1110;
            artifact.capabilities.push(crate::artifact::Capability {
                namespace: 0,
                name: 1,
                abi_major: 1,
                minimum_abi_minor: 0,
                flags: 1,
                operation_count: 1,
            });
            artifact.manifest.required_capabilities = 1;
            artifact.manifest.maximum_host_requests = 1;
            let NominalType::Function { flags, .. } = &mut artifact.modules[0].types[0] else {
                unreachable!();
            };
            *flags = 1;
            artifact.modules[0].functions[0].flags = 1;
        },
    )
}

pub(super) fn portable_layout_artifact() -> VerifiedArtifact {
    verified_mutated(|artifact| {
        let base_reference = ValueType {
            kind: 7,
            flags: 0,
            nominal_type: TypeId(1),
        };
        artifact.modules[0].types.extend([
            NominalType::Class {
                flags: 0,
                generic_arity: 0,
                name: 0,
                super_type: TypeId(u32::MAX),
                interfaces: Vec::new(),
                field_start: 0,
                field_count: 2,
                method_start: 0,
                method_count: 0,
            },
            NominalType::Class {
                flags: 0,
                generic_arity: 0,
                name: 0,
                super_type: TypeId(1),
                interfaces: Vec::new(),
                field_start: 2,
                field_count: 3,
                method_start: 0,
                method_count: 0,
            },
            NominalType::Array {
                name: 0,
                element: primitive(6),
            },
        ]);
        artifact.modules[0].declared_types = 4;
        artifact.modules[0].fields = vec![
            Field {
                owner: TypeId(1),
                name: 0,
                value_type: primitive(2),
                flags: 0,
            },
            Field {
                owner: TypeId(1),
                name: 0,
                value_type: primitive(5),
                flags: 0,
            },
            Field {
                owner: TypeId(2),
                name: 0,
                value_type: primitive(6),
                flags: 0,
            },
            Field {
                owner: TypeId(2),
                name: 0,
                value_type: base_reference,
                flags: 0,
            },
            Field {
                owner: TypeId(2),
                name: 0,
                value_type: primitive(1),
                flags: 2,
            },
        ];
        artifact.modules[0].utf16_literals = vec![ByteRange { start: 0, end: 4 }];
        configure_stack(artifact, 0, 1);
    })
}

pub(super) fn object_allocation_artifact(field_count: u32) -> VerifiedArtifact {
    verified_mutated(|artifact| {
        let reference = ValueType {
            kind: 7,
            flags: 0,
            nominal_type: TypeId(1),
        };
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: reference,
            parameters: Vec::new(),
        };
        artifact.modules[0].types.push(NominalType::Class {
            flags: 0,
            generic_arity: 0,
            name: 0,
            super_type: TypeId(u32::MAX),
            interfaces: Vec::new(),
            field_start: 0,
            field_count,
            method_start: 0,
            method_count: 0,
        });
        artifact.modules[0].declared_types = 2;
        artifact.modules[0].fields = (0..field_count)
            .map(|_| Field {
                owner: TypeId(1),
                name: 0,
                value_type: reference,
                flags: 0,
            })
            .collect();
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 1;
        function.registers = vec![reference];
        let instructions = vec![
            Instruction::NewObject {
                dst: 0,
                type_ref: 1,
            },
            Instruction::Return { value: 0 },
        ];
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
        configure_stack(artifact, 1, 1);
    })
}

pub(super) fn gc_retry_artifact() -> VerifiedArtifact {
    verified_mutated(|artifact| {
        let reference = ValueType {
            kind: 7,
            flags: 1,
            nominal_type: TypeId(1),
        };
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: reference,
            parameters: Vec::new(),
        };
        artifact.modules[0]
            .types
            .push(plain_class(TypeId(u32::MAX), 0, 0));
        artifact.modules[0].declared_types = 2;
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 1;
        function.registers = vec![reference];
        install_entry_blocks(
            artifact,
            vec![
                vec![
                    Instruction::NewObject {
                        dst: 0,
                        type_ref: 1,
                    },
                    Instruction::Jump { target: 1 },
                ],
                vec![
                    Instruction::Null { dst: 0 },
                    Instruction::Jump { target: 2 },
                ],
                vec![
                    Instruction::NewObject {
                        dst: 0,
                        type_ref: 1,
                    },
                    Instruction::Jump { target: 3 },
                ],
                vec![Instruction::Return { value: 0 }],
            ],
        );
        configure_stack(artifact, 1, 1);
    })
}

pub(super) fn gc_failed_retry_artifact() -> VerifiedArtifact {
    verified_mutated(|artifact| {
        let reference = ValueType {
            kind: 7,
            flags: 0,
            nominal_type: TypeId(1),
        };
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: reference,
            parameters: Vec::new(),
        };
        artifact.modules[0]
            .types
            .push(plain_class(TypeId(u32::MAX), 0, 0));
        artifact.modules[0].declared_types = 2;
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 2;
        function.registers = vec![reference, reference];
        install_entry_blocks(
            artifact,
            vec![
                vec![
                    Instruction::NewObject {
                        dst: 0,
                        type_ref: 1,
                    },
                    Instruction::Jump { target: 1 },
                ],
                vec![
                    Instruction::NewObject {
                        dst: 1,
                        type_ref: 1,
                    },
                    Instruction::Jump { target: 2 },
                ],
                vec![Instruction::Return { value: 1 }],
            ],
        );
        configure_stack(artifact, 2, 1);
    })
}

pub(super) fn gc_graph_artifact() -> VerifiedArtifact {
    verified_mutated(|artifact| {
        let reference = ValueType {
            kind: 7,
            flags: 1,
            nominal_type: TypeId(1),
        };
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: primitive(0),
            parameters: Vec::new(),
        };
        artifact.modules[0]
            .types
            .push(plain_class(TypeId(u32::MAX), 0, 3));
        artifact.modules[0].declared_types = 2;
        artifact.modules[0].fields = vec![
            Field {
                owner: TypeId(1),
                name: 0,
                value_type: reference,
                flags: 0,
            },
            Field {
                owner: TypeId(1),
                name: 0,
                value_type: reference,
                flags: 0,
            },
            Field {
                owner: TypeId(1),
                name: 0,
                value_type: reference,
                flags: 2,
            },
        ];
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 1;
        function.registers = vec![reference];
        install_entry_blocks(
            artifact,
            vec![vec![Instruction::Return { value: u16::MAX }]],
        );
        configure_stack(artifact, 1, 2);
    })
}

pub(super) fn array_allocation_artifact(length: i32) -> VerifiedArtifact {
    let reference = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(1),
    };
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: reference,
            parameters: Vec::new(),
        };
        artifact.modules[0].types.push(NominalType::Array {
            name: 0,
            element: primitive(5),
        });
        artifact.modules[0].declared_types = 2;
        artifact.modules[0].constants = vec![Constant::I32(length)];
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 2;
        function.registers = vec![primitive(1), reference];
        function.block_count = 2;
        let programs = [
            vec![
                Instruction::Const {
                    dst: 0,
                    constant: 0,
                },
                Instruction::Jump { target: 1 },
            ],
            vec![
                Instruction::NewArray {
                    dst: 1,
                    type_ref: 1,
                    length: 0,
                },
                Instruction::Return { value: 1 },
            ],
        ];
        let mut maximum = 0;
        artifact.modules[0].blocks.clear();
        artifact.modules[0].code.clear();
        for (block_id, instructions) in programs.into_iter().enumerate() {
            let fixed_cost = instructions
                .iter()
                .map(|instruction| instruction.fixed_cost().unwrap())
                .sum();
            maximum = maximum.max(fixed_cost);
            artifact.modules[0].blocks.push(Block {
                owner_function: FunctionId(0),
                code_record: BlockId(block_id as u32),
                instruction_count: instructions.len() as u32,
                declared_fixed_cost: fixed_cost,
                flags: 0,
            });
            artifact.modules[0].code.push(DecodedCode {
                bytes: ByteRange { start: 0, end: 0 },
                instructions: instructions.into_boxed_slice(),
                fixed_cost,
            });
        }
        artifact.manifest.maximum_block_cost = maximum;
        artifact.manifest.minimum_slice_cost = maximum;
        configure_stack(artifact, 2, 1);
    })
}

pub(super) fn static_roundtrip_artifact() -> VerifiedArtifact {
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: primitive(1),
            parameters: vec![primitive(5), primitive(1)],
        };
        artifact.modules[0].types.push(NominalType::Class {
            flags: 0,
            generic_arity: 0,
            name: 0,
            super_type: TypeId(u32::MAX),
            interfaces: Vec::new(),
            field_start: 0,
            field_count: 1,
            method_start: 0,
            method_count: 0,
        });
        artifact.modules[0].declared_types = 2;
        artifact.modules[0].fields = vec![Field {
            owner: TypeId(1),
            name: 0,
            value_type: primitive(1),
            flags: 3,
        }];
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 3;
        function.parameter_count = 2;
        function.registers = vec![primitive(5), primitive(1), primitive(1)];
        install_entry_blocks(
            artifact,
            vec![
                vec![Instruction::Branch {
                    condition: 0,
                    true_block: 1,
                    false_block: 2,
                }],
                vec![
                    Instruction::StaticSet {
                        field_ref: 0,
                        value: 1,
                    },
                    Instruction::Jump { target: 2 },
                ],
                vec![
                    Instruction::StaticGet {
                        dst: 2,
                        field_ref: 0,
                    },
                    Instruction::Return { value: 2 },
                ],
            ],
        );
        configure_stack(artifact, 3, 1);
    })
}

pub(super) fn field_roundtrip_artifact() -> VerifiedArtifact {
    verified_mutated(|artifact| {
        let subclass = ValueType {
            kind: 7,
            flags: 0,
            nominal_type: TypeId(3),
        };
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: primitive(1),
            parameters: Vec::new(),
        };
        artifact.modules[0].types.extend([
            NominalType::Interface {
                flags: 0,
                generic_arity: 0,
                name: 0,
                super_type: TypeId(u32::MAX),
                interfaces: Vec::new(),
                method_start: 0,
                method_count: 0,
            },
            NominalType::Class {
                flags: 0,
                generic_arity: 0,
                name: 0,
                super_type: TypeId(u32::MAX),
                interfaces: vec![TypeId(1)],
                field_start: 0,
                field_count: 1,
                method_start: 0,
                method_count: 0,
            },
            NominalType::Class {
                flags: 0,
                generic_arity: 0,
                name: 0,
                super_type: TypeId(2),
                interfaces: Vec::new(),
                field_start: 1,
                field_count: 0,
                method_start: 0,
                method_count: 0,
            },
        ]);
        artifact.modules[0].declared_types = 4;
        artifact.modules[0].fields = vec![Field {
            owner: TypeId(2),
            name: 0,
            value_type: primitive(1),
            flags: 1,
        }];
        artifact.modules[0].constants = vec![Constant::I32(42)];
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 4;
        function.registers = vec![subclass, primitive(1), primitive(1), primitive(5)];
        install_entry_blocks(
            artifact,
            vec![
                vec![
                    Instruction::NewObject {
                        dst: 0,
                        type_ref: 3,
                    },
                    Instruction::Jump { target: 1 },
                ],
                vec![
                    Instruction::Const {
                        dst: 1,
                        constant: 0,
                    },
                    Instruction::FieldSet {
                        receiver: 0,
                        field_ref: 0,
                        value: 1,
                    },
                    Instruction::FieldGet {
                        dst: 2,
                        receiver: 0,
                        field_ref: 0,
                    },
                    Instruction::IsType {
                        dst: 3,
                        value: 0,
                        type_ref: 1,
                    },
                    Instruction::Return { value: 2 },
                ],
            ],
        );
        configure_stack(artifact, 4, 1);
    })
}

pub(super) fn primitive_array_roundtrip_cases() -> Vec<(VerifiedArtifact, RuntimeValue)> {
    vec![
        primitive_array_roundtrip_artifact(1, Constant::I32(-7), RuntimeValue::I32(-7)),
        primitive_array_roundtrip_artifact(
            2,
            Constant::I64(i64::MIN + 9),
            RuntimeValue::I64(i64::MIN + 9),
        ),
        primitive_array_roundtrip_artifact(
            3,
            Constant::F32(0x7fc0_1234),
            RuntimeValue::F32(0x7fc0_1234),
        ),
        primitive_array_roundtrip_artifact(
            4,
            Constant::F64(0x7ff8_0000_0000_1234),
            RuntimeValue::F64(0x7ff8_0000_0000_1234),
        ),
        primitive_array_roundtrip_artifact(5, Constant::Bool(true), RuntimeValue::Bool(true)),
        primitive_array_roundtrip_artifact(6, Constant::Char(0xd800), RuntimeValue::Char(0xd800)),
    ]
}

fn primitive_array_roundtrip_artifact(
    kind: u8,
    constant: Constant,
    expected: RuntimeValue,
) -> (VerifiedArtifact, RuntimeValue) {
    let element = primitive(kind);
    let array = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(1),
    };
    let artifact = verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: element,
            parameters: Vec::new(),
        };
        artifact.modules[0]
            .types
            .push(NominalType::Array { name: 0, element });
        artifact.modules[0].declared_types = 2;
        artifact.modules[0].constants = vec![Constant::I32(0), Constant::I32(1), constant];
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 6;
        function.registers = vec![
            primitive(1),
            primitive(1),
            element,
            array,
            element,
            primitive(1),
        ];
        install_entry_blocks(
            artifact,
            vec![
                vec![
                    Instruction::Const {
                        dst: 0,
                        constant: 1,
                    },
                    Instruction::Const {
                        dst: 1,
                        constant: 0,
                    },
                    Instruction::Const {
                        dst: 2,
                        constant: 2,
                    },
                    Instruction::Jump { target: 1 },
                ],
                vec![
                    Instruction::NewArray {
                        dst: 3,
                        type_ref: 1,
                        length: 0,
                    },
                    Instruction::ArrayStore {
                        array: 3,
                        index: 1,
                        value: 2,
                    },
                    Instruction::ArrayLoad {
                        dst: 4,
                        array: 3,
                        index: 1,
                    },
                    Instruction::ArrayLength { dst: 5, array: 3 },
                    Instruction::Return { value: 4 },
                ],
            ],
        );
        configure_stack(artifact, 6, 1);
    });
    (artifact, expected)
}

pub(super) fn reference_array_roundtrip_artifact() -> VerifiedArtifact {
    let base = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(1),
    };
    let subclass = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(2),
    };
    let array = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(3),
    };
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: base,
            parameters: Vec::new(),
        };
        artifact.modules[0].types.extend([
            plain_class(TypeId(u32::MAX), 0, 0),
            plain_class(TypeId(1), 0, 0),
            NominalType::Array {
                name: 0,
                element: base,
            },
        ]);
        artifact.modules[0].declared_types = 4;
        artifact.modules[0].constants = vec![Constant::I32(0), Constant::I32(1)];
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 5;
        function.registers = vec![primitive(1), primitive(1), subclass, array, base];
        install_entry_blocks(
            artifact,
            vec![
                vec![
                    Instruction::NewObject {
                        dst: 2,
                        type_ref: 2,
                    },
                    Instruction::Jump { target: 1 },
                ],
                vec![
                    Instruction::Const {
                        dst: 0,
                        constant: 1,
                    },
                    Instruction::Const {
                        dst: 1,
                        constant: 0,
                    },
                    Instruction::Jump { target: 2 },
                ],
                vec![
                    Instruction::NewArray {
                        dst: 3,
                        type_ref: 3,
                        length: 0,
                    },
                    Instruction::ArrayStore {
                        array: 3,
                        index: 1,
                        value: 2,
                    },
                    Instruction::ArrayLoad {
                        dst: 4,
                        array: 3,
                        index: 1,
                    },
                    Instruction::Return { value: 4 },
                ],
            ],
        );
        configure_stack(artifact, 5, 1);
    })
}

pub(super) fn array_bounds_artifact() -> VerifiedArtifact {
    let array = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(1),
    };
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: primitive(1),
            parameters: vec![primitive(1)],
        };
        artifact.modules[0].types.push(NominalType::Array {
            name: 0,
            element: primitive(1),
        });
        artifact.modules[0].declared_types = 2;
        artifact.modules[0].constants = vec![Constant::I32(1), Constant::I32(99)];
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 4;
        function.parameter_count = 1;
        function.registers = vec![primitive(1), primitive(1), primitive(1), array];
        install_entry_blocks(
            artifact,
            vec![
                vec![
                    Instruction::Const {
                        dst: 1,
                        constant: 0,
                    },
                    Instruction::Const {
                        dst: 2,
                        constant: 1,
                    },
                    Instruction::Jump { target: 1 },
                ],
                vec![
                    Instruction::NewArray {
                        dst: 3,
                        type_ref: 1,
                        length: 1,
                    },
                    Instruction::ArrayLoad {
                        dst: 2,
                        array: 3,
                        index: 0,
                    },
                    Instruction::Return { value: 2 },
                ],
            ],
        );
        configure_stack(artifact, 4, 1);
    })
}

pub(super) fn nonnull_zero_field_artifact() -> VerifiedArtifact {
    let reference = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(1),
    };
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: reference,
            parameters: Vec::new(),
        };
        artifact.modules[0]
            .types
            .push(plain_class(TypeId(u32::MAX), 0, 1));
        artifact.modules[0].declared_types = 2;
        artifact.modules[0].fields = vec![Field {
            owner: TypeId(1),
            name: 0,
            value_type: reference,
            flags: 1,
        }];
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 2;
        function.registers = vec![reference, reference];
        install_entry_blocks(
            artifact,
            vec![vec![
                Instruction::NewObject {
                    dst: 0,
                    type_ref: 1,
                },
                Instruction::FieldGet {
                    dst: 1,
                    receiver: 0,
                    field_ref: 0,
                },
                Instruction::Return { value: 1 },
            ]],
        );
        configure_stack(artifact, 2, 1);
    })
}

pub(super) fn nullable_cast_artifact(destination_nullable: bool) -> VerifiedArtifact {
    let source = ValueType {
        kind: 7,
        flags: 1,
        nominal_type: TypeId(1),
    };
    let destination = ValueType {
        kind: 7,
        flags: u8::from(destination_nullable),
        nominal_type: TypeId(2),
    };
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: destination,
            parameters: vec![source],
        };
        artifact.modules[0].types.extend([
            plain_class(TypeId(u32::MAX), 0, 0),
            plain_class(TypeId(1), 0, 0),
        ]);
        artifact.modules[0].declared_types = 3;
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 2;
        function.parameter_count = 1;
        function.registers = vec![source, destination];
        install_entry_blocks(
            artifact,
            vec![vec![
                Instruction::CheckedCast {
                    dst: 1,
                    value: 0,
                    type_ref: 2,
                },
                Instruction::Return { value: 1 },
            ]],
        );
        configure_stack(artifact, 2, 1);
    })
}

pub(super) fn incompatible_cast_artifact() -> VerifiedArtifact {
    let source = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(1),
    };
    let target = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(2),
    };
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: target,
            parameters: Vec::new(),
        };
        artifact.modules[0].types.extend([
            plain_class(TypeId(u32::MAX), 0, 0),
            plain_class(TypeId(u32::MAX), 0, 0),
        ]);
        artifact.modules[0].declared_types = 3;
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 2;
        function.registers = vec![source, target];
        install_entry_blocks(
            artifact,
            vec![vec![
                Instruction::NewObject {
                    dst: 0,
                    type_ref: 1,
                },
                Instruction::CheckedCast {
                    dst: 1,
                    value: 0,
                    type_ref: 2,
                },
                Instruction::Return { value: 1 },
            ]],
        );
        configure_stack(artifact, 2, 1);
    })
}

pub(super) fn reference_field_roundtrip_artifact() -> VerifiedArtifact {
    let reference = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(1),
    };
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: reference,
            parameters: Vec::new(),
        };
        artifact.modules[0]
            .types
            .push(plain_class(TypeId(u32::MAX), 0, 1));
        artifact.modules[0].declared_types = 2;
        artifact.modules[0].fields = vec![Field {
            owner: TypeId(1),
            name: 0,
            value_type: reference,
            flags: 1,
        }];
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 3;
        function.registers = vec![reference, reference, reference];
        install_entry_blocks(
            artifact,
            vec![
                vec![
                    Instruction::NewObject {
                        dst: 0,
                        type_ref: 1,
                    },
                    Instruction::Jump { target: 1 },
                ],
                vec![
                    Instruction::NewObject {
                        dst: 1,
                        type_ref: 1,
                    },
                    Instruction::Jump { target: 2 },
                ],
                vec![
                    Instruction::FieldSet {
                        receiver: 0,
                        field_ref: 0,
                        value: 1,
                    },
                    Instruction::FieldGet {
                        dst: 2,
                        receiver: 0,
                        field_ref: 0,
                    },
                    Instruction::Return { value: 2 },
                ],
            ],
        );
        configure_stack(artifact, 3, 1);
    })
}

pub(super) fn failed_array_store_artifact() -> VerifiedArtifact {
    let array = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: TypeId(1),
    };
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: primitive(0),
            parameters: vec![primitive(1)],
        };
        artifact.modules[0].types.push(NominalType::Array {
            name: 0,
            element: primitive(1),
        });
        artifact.modules[0].declared_types = 2;
        artifact.modules[0].constants = vec![Constant::I32(1), Constant::I32(7)];
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 4;
        function.parameter_count = 1;
        function.registers = vec![primitive(1), primitive(1), array, primitive(1)];
        install_entry_blocks(
            artifact,
            vec![
                vec![
                    Instruction::Const {
                        dst: 1,
                        constant: 0,
                    },
                    Instruction::Const {
                        dst: 3,
                        constant: 1,
                    },
                    Instruction::Jump { target: 1 },
                ],
                vec![
                    Instruction::NewArray {
                        dst: 2,
                        type_ref: 1,
                        length: 1,
                    },
                    Instruction::ArrayStore {
                        array: 2,
                        index: 0,
                        value: 3,
                    },
                    Instruction::Return { value: u16::MAX },
                ],
            ],
        );
        configure_stack(artifact, 4, 1);
    })
}

pub(super) fn null_is_type_artifact() -> VerifiedArtifact {
    let nullable = ValueType {
        kind: 7,
        flags: 1,
        nominal_type: TypeId(1),
    };
    verified_mutated(|artifact| {
        artifact.modules[0].types[0] = NominalType::Function {
            name: 1,
            flags: 0,
            result: primitive(5),
            parameters: Vec::new(),
        };
        artifact.modules[0]
            .types
            .push(plain_class(TypeId(u32::MAX), 0, 0));
        artifact.modules[0].declared_types = 2;
        let function = &mut artifact.modules[0].functions[0];
        function.register_count = 2;
        function.registers = vec![nullable, primitive(5)];
        install_entry_blocks(
            artifact,
            vec![vec![
                Instruction::Null { dst: 0 },
                Instruction::IsType {
                    dst: 1,
                    value: 0,
                    type_ref: 1,
                },
                Instruction::Return { value: 1 },
            ]],
        );
        configure_stack(artifact, 2, 1);
    })
}

fn plain_class(super_type: TypeId, field_start: u32, field_count: u32) -> NominalType {
    NominalType::Class {
        flags: 0,
        generic_arity: 0,
        name: 0,
        super_type,
        interfaces: Vec::new(),
        field_start,
        field_count,
        method_start: 0,
        method_count: 0,
    }
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
    EntryArgument,
    EntryArgument,
    EntryArgument,
    EntryArgument,
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
    let reference = |owner, handle, generation| {
        EntryArgument::owned(
            owner,
            RuntimeValue::Reference(ReferenceValue::host(handle, generation).unwrap()),
        )
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
        heap_bytes: 1024 * 1024,
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

fn install_entry_blocks(
    artifact: &mut crate::artifact::DecodedArtifact,
    programs: Vec<Vec<Instruction>>,
) {
    let function = &mut artifact.modules[0].functions[0];
    function.first_block = BlockId(0);
    function.block_count = programs.len() as u32;
    let mut blocks = Vec::with_capacity(programs.len());
    let mut code = Vec::with_capacity(programs.len());
    let mut maximum_block_cost = 0;
    for (block_id, instructions) in programs.into_iter().enumerate() {
        let fixed_cost = instructions
            .iter()
            .map(|instruction| instruction.fixed_cost().unwrap())
            .sum();
        maximum_block_cost = maximum_block_cost.max(fixed_cost);
        blocks.push(Block {
            owner_function: FunctionId(0),
            code_record: BlockId(block_id as u32),
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
