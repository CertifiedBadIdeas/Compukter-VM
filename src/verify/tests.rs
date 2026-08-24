use std::sync::Arc;

use crate::{diagnostic::Code, limits::ArtifactLimits};

use crate::test_support as support;

fn decoded(bytes: Vec<u8>) -> crate::artifact::DecodedArtifact {
    crate::decode::records::decode_artifact(Arc::from(bytes), &ArtifactLimits::default()).unwrap()
}

#[test]
fn module_accepts_vector_a_identity() {
    super::modules::verify_modules(
        &decoded(support::minimal_vector()),
        &ArtifactLimits::default(),
    )
    .unwrap();
}

#[test]
fn module_rejects_wrong_semantic_hash() {
    let mut bytes = support::minimal_vector();
    let function_type =
        support::indexed_record_offset(&bytes, crate::artifact::format::TYPES, 1, 0);
    support::write_u32(&mut bytes, function_type + 4, 0);
    support::rehash(&mut bytes);
    let error =
        super::modules::verify_modules(&decoded(bytes), &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadModule);
}

#[test]
fn module_accepts_acyclic_two_module_bundle() {
    let artifact = decoded(support::two_module_vector());
    assert_eq!(artifact.modules.len(), 2);
    assert_eq!(artifact.modules[0].imports.len(), 1);
    assert_eq!(artifact.modules[1].exports.len(), 1);
    super::modules::verify_modules(&artifact, &ArtifactLimits::default()).unwrap();
}

#[test]
fn module_rejects_import_hash_mismatch() {
    let mut artifact = decoded(support::two_module_vector());
    artifact.modules[0].imports[0].target_hash = [0; 32];
    let error = super::modules::verify_modules(&artifact, &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadSymbol);
}

#[test]
fn module_rejects_missing_import_target_export() {
    let mut artifact = decoded(support::two_module_vector());
    artifact.modules[1].exports.clear();
    let error = super::modules::verify_modules(&artifact, &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadSymbol);
}

#[test]
fn module_rejects_import_cycle() {
    let mut artifact = decoded(support::two_module_vector());
    let application_hash = artifact.modules[0].semantic_hash;
    artifact.modules[1].imports.push(crate::artifact::Import {
        kind: 1,
        target_module: crate::artifact::ModuleId(0),
        target_name: 1,
        expected_signature: crate::artifact::TypeId(0),
        target_hash: application_hash,
    });
    let error = super::modules::verify_modules(&artifact, &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadModule);
}

#[test]
fn module_rejects_signature_mismatch() {
    let mut artifact = decoded(support::two_module_vector());
    artifact.modules[1].exports[0].signature = crate::artifact::TypeId(u32::MAX);
    let error = super::modules::verify_modules(&artifact, &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadSymbol);
}

#[test]
fn module_rejects_ambiguous_export_resolution() {
    let mut artifact = decoded(support::two_module_vector());
    artifact.modules[1].exports.push(crate::artifact::Export {
        kind: 1,
        visibility: 1,
        name: 1,
        local_symbol: 0,
        signature: crate::artifact::TypeId(0),
    });
    let error = super::modules::verify_modules(&artifact, &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadSymbol);
}

fn class(name: u32, flags: u8, super_type: u32) -> crate::artifact::NominalType {
    crate::artifact::NominalType::Class {
        flags,
        generic_arity: 0,
        name,
        super_type: crate::artifact::TypeId(super_type),
        interfaces: Vec::new(),
        field_start: 0,
        field_count: 0,
        method_start: 0,
        method_count: 0,
    }
}

#[test]
fn module_rejects_abstract_final_class() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].types[0] = class(0, 3, u32::MAX);
    let error = super::modules::verify_modules(&artifact, &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

#[test]
fn module_rejects_inheritance_cycle() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].types = vec![class(0, 0, 1), class(1, 0, 0)];
    let error = super::modules::verify_modules(&artifact, &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

#[test]
fn module_rejects_non_interface_implementation_edge() {
    let mut artifact = decoded(support::minimal_vector());
    let mut root = class(0, 0, u32::MAX);
    if let crate::artifact::NominalType::Class { interfaces, .. } = &mut root {
        interfaces.push(crate::artifact::TypeId(1));
    }
    artifact.modules[0].types = vec![root, class(1, 0, u32::MAX)];
    let error = super::modules::verify_modules(&artifact, &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

#[test]
fn module_rejects_field_owned_by_function_type() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].fields.push(crate::artifact::Field {
        owner: crate::artifact::TypeId(0),
        name: 1,
        value_type: crate::artifact::ValueType {
            kind: 1,
            flags: 0,
            nominal_type: crate::artifact::TypeId(u32::MAX),
        },
        flags: 0,
    });
    let error = super::modules::verify_modules(&artifact, &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

fn verify_cfg(artifact: &crate::artifact::DecodedArtifact) -> Result<(), crate::DiagnosticSet> {
    let limits = ArtifactLimits::default();
    let exceptions = super::exceptions::verify_exceptions(artifact, &limits)?;
    super::functions::verify_functions(artifact, &exceptions, &limits)
}

fn unit_return() -> crate::artifact::DecodedCode {
    crate::artifact::DecodedCode {
        bytes: crate::artifact::ByteRange { start: 0, end: 0 },
        instructions: vec![crate::artifact::Instruction::Return { value: u16::MAX }]
            .into_boxed_slice(),
        fixed_cost: 1,
    }
}

#[test]
fn cfg_accepts_vector_a() {
    verify_cfg(&decoded(support::minimal_vector())).unwrap();
}

#[test]
fn cfg_rejects_bad_entry_function() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.header.entry_function = 1;
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadControlFlow);
}

#[test]
fn cfg_rejects_uninitialized_register_read() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.manifest.maximum_block_cost = 2;
    artifact.manifest.minimum_slice_cost = 2;
    let function = &mut artifact.modules[0].functions[0];
    function.register_count = 2;
    function.registers = vec![
        crate::artifact::ValueType {
            kind: 1,
            flags: 0,
            nominal_type: crate::artifact::TypeId(u32::MAX),
        },
        crate::artifact::ValueType {
            kind: 1,
            flags: 0,
            nominal_type: crate::artifact::TypeId(u32::MAX),
        },
    ];
    artifact.modules[0].blocks[0].instruction_count = 2;
    artifact.modules[0].blocks[0].declared_fixed_cost = 2;
    artifact.modules[0].code[0].instructions = vec![
        crate::artifact::Instruction::Move { dst: 0, src: 1 },
        crate::artifact::Instruction::Return { value: u16::MAX },
    ]
    .into_boxed_slice();
    artifact.modules[0].code[0].fixed_cost = 2;
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::UninitializedRegister);
}

#[test]
fn cfg_rejects_wrong_destination_type() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.manifest.maximum_block_cost = 2;
    artifact.manifest.minimum_slice_cost = 2;
    let bool_type = crate::artifact::ValueType {
        kind: 5,
        flags: 0,
        nominal_type: crate::artifact::TypeId(u32::MAX),
    };
    let i32_type = crate::artifact::ValueType {
        kind: 1,
        flags: 0,
        nominal_type: crate::artifact::TypeId(u32::MAX),
    };
    artifact.modules[0].types[0] = crate::artifact::NominalType::Function {
        name: 1,
        flags: 0,
        result: crate::artifact::ValueType {
            kind: 0,
            flags: 0,
            nominal_type: crate::artifact::TypeId(u32::MAX),
        },
        parameters: vec![bool_type, i32_type],
    };
    let function = &mut artifact.modules[0].functions[0];
    function.register_count = 2;
    function.parameter_count = 2;
    function.registers = vec![bool_type, i32_type];
    artifact.modules[0].blocks[0].instruction_count = 2;
    artifact.modules[0].blocks[0].declared_fixed_cost = 2;
    artifact.modules[0].code[0].instructions = vec![
        crate::artifact::Instruction::Move { dst: 0, src: 1 },
        crate::artifact::Instruction::Return { value: u16::MAX },
    ]
    .into_boxed_slice();
    artifact.modules[0].code[0].fixed_cost = 2;
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

#[test]
fn cfg_rejects_unreachable_block() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].functions[0].block_count = 2;
    artifact.modules[0].blocks.push(crate::artifact::Block {
        owner_function: crate::artifact::FunctionId(0),
        code_record: crate::artifact::BlockId(1),
        instruction_count: 1,
        declared_fixed_cost: 1,
        flags: 0,
    });
    artifact.modules[0].code.push(unit_return());
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadControlFlow);
}

#[test]
fn cfg_rejects_backedge_to_non_safepoint() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].code[0].instructions =
        vec![crate::artifact::Instruction::Jump { target: 0 }].into_boxed_slice();
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadControlFlow);
}

#[test]
fn cfg_rejects_incorrect_block_cost() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].blocks[0].declared_fixed_cost = 2;
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadCost);
}

#[test]
fn cfg_rejects_slice_cost_below_maximum_block_cost() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.manifest.minimum_slice_cost = 0;
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadCost);
}

fn primitive(kind: u8) -> crate::artifact::ValueType {
    crate::artifact::ValueType {
        kind,
        flags: 0,
        nominal_type: crate::artifact::TypeId(u32::MAX),
    }
}

fn reference(type_id: u32, nullable: bool) -> crate::artifact::ValueType {
    crate::artifact::ValueType {
        kind: 7,
        flags: u8::from(nullable),
        nominal_type: crate::artifact::TypeId(type_id),
    }
}

fn configure_entry(
    artifact: &mut crate::artifact::DecodedArtifact,
    registers: Vec<crate::artifact::ValueType>,
    parameter_count: u16,
    instructions: Vec<crate::artifact::Instruction>,
) {
    let parameters = registers[..parameter_count as usize].to_vec();
    artifact.modules[0].types[0] = crate::artifact::NominalType::Function {
        name: 1,
        flags: 0,
        result: primitive(0),
        parameters,
    };
    let function = &mut artifact.modules[0].functions[0];
    function.register_count = registers.len() as u16;
    function.parameter_count = parameter_count;
    function.registers = registers;
    let fixed_cost = instructions
        .iter()
        .map(|instruction| instruction.fixed_cost().unwrap())
        .sum();
    artifact.manifest.maximum_block_cost = fixed_cost;
    artifact.manifest.minimum_slice_cost = fixed_cost;
    artifact.modules[0].blocks[0].instruction_count = instructions.len() as u32;
    artifact.modules[0].blocks[0].declared_fixed_cost = fixed_cost;
    artifact.modules[0].code[0].instructions = instructions.into_boxed_slice();
    artifact.modules[0].code[0].fixed_cost = fixed_cost;
}

fn allocation_artifact(
    instructions: Vec<crate::artifact::Instruction>,
) -> crate::artifact::DecodedArtifact {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].types.push(class(0, 0, u32::MAX));
    configure_entry(&mut artifact, vec![reference(1, false)], 0, instructions);
    artifact
}

fn string_artifact(
    registers: Vec<crate::artifact::ValueType>,
    parameter_count: u16,
    instructions: Vec<crate::artifact::Instruction>,
) -> crate::artifact::DecodedArtifact {
    let mut artifact = decoded(support::minimal_vector());
    let mut bytes = artifact.bytes.to_vec();
    let name_start = bytes.len();
    bytes.extend_from_slice(b"kotlin.String");
    let name_end = bytes.len();
    artifact.bytes = Arc::from(bytes);

    let module = &mut artifact.modules[0];
    module.flags = 2;
    let name = module.strings.len() as u32;
    module.strings.push(crate::artifact::ByteRange {
        start: name_start,
        end: name_end,
    });
    module.types.push(class(name, 2, u32::MAX));
    module.exports.push(crate::artifact::Export {
        kind: 0,
        visibility: 1,
        name,
        local_symbol: 1,
        signature: crate::artifact::TypeId(1),
    });
    configure_entry(&mut artifact, registers, parameter_count, instructions);
    artifact
}

#[test]
fn cfg_accepts_typed_string_length() {
    let artifact = string_artifact(
        vec![reference(1, false), primitive(1)],
        1,
        vec![
            crate::artifact::Instruction::StringLength { dst: 1, string: 0 },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );

    verify_cfg(&artifact).unwrap();
}

#[test]
fn cfg_rejects_string_materialization_from_non_char_array() {
    let mut artifact = string_artifact(
        vec![
            reference(2, false),
            primitive(1),
            primitive(1),
            reference(1, false),
        ],
        3,
        vec![
            crate::artifact::Instruction::StringFromCharArray {
                dst: 3,
                array: 0,
                start: 1,
                end: 2,
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    artifact.modules[0]
        .types
        .push(crate::artifact::NominalType::Array {
            name: 0,
            element: primitive(1),
        });

    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

#[test]
fn cfg_requires_exact_standard_string_type_for_string_constants() {
    let mut accepted = string_artifact(
        vec![reference(1, false)],
        0,
        vec![
            crate::artifact::Instruction::Const {
                dst: 0,
                constant: 0,
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    accepted.modules[0]
        .utf16_literals
        .push(crate::artifact::ByteRange { start: 0, end: 0 });
    accepted.modules[0]
        .constants
        .push(crate::artifact::Constant::String(
            crate::artifact::Utf16LiteralId(0),
        ));
    verify_cfg(&accepted).unwrap();

    let mut rejected = allocation_artifact(vec![
        crate::artifact::Instruction::Const {
            dst: 0,
            constant: 0,
        },
        crate::artifact::Instruction::Return { value: u16::MAX },
    ]);
    rejected.modules[0]
        .utf16_literals
        .push(crate::artifact::ByteRange { start: 0, end: 0 });
    rejected.modules[0]
        .constants
        .push(crate::artifact::Constant::String(
            crate::artifact::Utf16LiteralId(0),
        ));
    assert_eq!(
        Code::BadType,
        verify_cfg(&rejected).unwrap_err().first().unwrap().code
    );
}

#[test]
fn cfg_accepts_every_typed_string_instruction() {
    let string = reference(1, false);
    let cases = [
        (
            vec![string, primitive(1), primitive(6)],
            2,
            crate::artifact::Instruction::StringGet {
                dst: 2,
                string: 0,
                index: 1,
            },
        ),
        (
            vec![string, string, primitive(5)],
            2,
            crate::artifact::Instruction::StringEquals {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
        ),
        (
            vec![string, string, primitive(1)],
            2,
            crate::artifact::Instruction::StringCompare {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
        ),
        (
            vec![string, primitive(1)],
            1,
            crate::artifact::Instruction::StringHash { dst: 1, string: 0 },
        ),
        (
            vec![string, string, string],
            2,
            crate::artifact::Instruction::StringConcat {
                dst: 2,
                lhs: 0,
                rhs: 1,
            },
        ),
        (
            vec![string, primitive(1), primitive(1), string],
            3,
            crate::artifact::Instruction::StringSubstring {
                dst: 3,
                string: 0,
                start: 1,
                end: 2,
            },
        ),
    ];

    for (registers, parameter_count, instruction) in cases {
        let artifact = string_artifact(
            registers,
            parameter_count,
            vec![
                instruction,
                crate::artifact::Instruction::Return { value: u16::MAX },
            ],
        );
        verify_cfg(&artifact).unwrap();
    }
}

#[test]
fn cfg_rejects_string_allocation_after_another_instruction() {
    for instruction in [
        crate::artifact::Instruction::StringConcat {
            dst: 2,
            lhs: 0,
            rhs: 1,
        },
        crate::artifact::Instruction::StringSubstring {
            dst: 3,
            string: 0,
            start: 1,
            end: 2,
        },
    ] {
        let is_concat = matches!(
            &instruction,
            crate::artifact::Instruction::StringConcat { .. }
        );
        let registers = match &instruction {
            crate::artifact::Instruction::StringConcat { .. } => {
                vec![
                    reference(1, false),
                    reference(1, false),
                    reference(1, false),
                ]
            }
            _ => vec![
                reference(1, false),
                primitive(1),
                primitive(1),
                reference(1, false),
            ],
        };
        let artifact = string_artifact(
            registers,
            if is_concat { 2 } else { 3 },
            vec![
                crate::artifact::Instruction::Nop,
                instruction,
                crate::artifact::Instruction::Return { value: u16::MAX },
            ],
        );

        let error = verify_cfg(&artifact).unwrap_err();
        assert_eq!(error.first().unwrap().code, Code::BadInstruction);
    }
}

#[test]
fn cfg_rejects_allocation_after_another_instruction() {
    let artifact = allocation_artifact(vec![
        crate::artifact::Instruction::Nop,
        crate::artifact::Instruction::NewObject {
            dst: 0,
            type_ref: 1,
        },
        crate::artifact::Instruction::Return { value: u16::MAX },
    ]);

    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadInstruction);
}

#[test]
fn cfg_rejects_two_allocations_in_one_block() {
    let artifact = allocation_artifact(vec![
        crate::artifact::Instruction::NewObject {
            dst: 0,
            type_ref: 1,
        },
        crate::artifact::Instruction::NewObject {
            dst: 0,
            type_ref: 1,
        },
        crate::artifact::Instruction::Return { value: u16::MAX },
    ]);

    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadInstruction);
}

#[test]
fn cfg_accepts_dedicated_object_and_array_allocation_blocks() {
    let object = allocation_artifact(vec![
        crate::artifact::Instruction::NewObject {
            dst: 0,
            type_ref: 1,
        },
        crate::artifact::Instruction::Return { value: u16::MAX },
    ]);
    verify_cfg(&object).unwrap();

    let mut array = decoded(support::minimal_vector());
    array.modules[0]
        .types
        .push(crate::artifact::NominalType::Array {
            name: 0,
            element: primitive(1),
        });
    configure_entry(
        &mut array,
        vec![reference(1, false), primitive(1)],
        2,
        vec![
            crate::artifact::Instruction::NewArray {
                dst: 0,
                type_ref: 1,
                length: 1,
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    verify_cfg(&array).unwrap();
}

#[test]
fn cfg_accepts_i32_char_conversions() {
    let mut artifact = decoded(support::minimal_vector());
    configure_entry(
        &mut artifact,
        vec![primitive(1), primitive(6), primitive(6), primitive(1)],
        2,
        vec![
            crate::artifact::Instruction::Convert { dst: 2, src: 0 },
            crate::artifact::Instruction::Convert { dst: 3, src: 1 },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    verify_cfg(&artifact).unwrap();
}

#[test]
fn cfg_rejects_other_char_conversion_pairs() {
    for numeric_kind in [2, 3, 4] {
        let mut artifact = decoded(support::minimal_vector());
        configure_entry(
            &mut artifact,
            vec![primitive(6), primitive(numeric_kind)],
            1,
            vec![
                crate::artifact::Instruction::Convert { dst: 1, src: 0 },
                crate::artifact::Instruction::Return { value: u16::MAX },
            ],
        );
        let error = verify_cfg(&artifact).unwrap_err();
        assert_eq!(error.first().unwrap().code, Code::BadType);
    }
}

#[test]
fn cfg_accepts_imported_direct_call() {
    let mut artifact = decoded(support::two_module_vector());
    configure_entry(
        &mut artifact,
        Vec::new(),
        0,
        vec![
            crate::artifact::Instruction::CallDirect {
                dst: u16::MAX,
                function_ref: 0x8000_0000,
                args: Vec::new().into_boxed_slice(),
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    verify_cfg(&artifact).unwrap();
}

#[test]
fn cfg_rejects_wrong_call_argument_count() {
    let mut artifact = decoded(support::two_module_vector());
    configure_entry(
        &mut artifact,
        vec![primitive(1)],
        1,
        vec![
            crate::artifact::Instruction::CallDirect {
                dst: u16::MAX,
                function_ref: 0x8000_0000,
                args: vec![0].into_boxed_slice(),
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

#[test]
fn cfg_rejects_direct_call_to_abstract_function() {
    let mut artifact = decoded(support::two_module_vector());
    artifact.modules[1].functions[0].flags |= 1 << 3;
    artifact.modules[1].functions[0].block_count = 0;
    configure_entry(
        &mut artifact,
        Vec::new(),
        0,
        vec![
            crate::artifact::Instruction::CallDirect {
                dst: u16::MAX,
                function_ref: 0x8000_0000,
                args: Vec::new().into_boxed_slice(),
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

#[test]
fn cfg_rejects_forged_import_kind_reference() {
    let mut artifact = decoded(support::two_module_vector());
    configure_entry(
        &mut artifact,
        vec![reference(0, false)],
        0,
        vec![
            crate::artifact::Instruction::NewObject {
                dst: 0,
                type_ref: 0x8000_0000,
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

fn field_artifact(nullable_receiver: bool, mutable: bool) -> crate::artifact::DecodedArtifact {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].types.push(class(0, 0, u32::MAX));
    artifact.modules[0].fields.push(crate::artifact::Field {
        owner: crate::artifact::TypeId(1),
        name: 1,
        value_type: primitive(1),
        flags: u32::from(mutable),
    });
    configure_entry(
        &mut artifact,
        vec![reference(1, nullable_receiver), primitive(1)],
        2,
        vec![
            crate::artifact::Instruction::FieldSet {
                receiver: 0,
                field_ref: 0,
                value: 1,
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    artifact
}

#[test]
fn cfg_accepts_mutable_field_write() {
    verify_cfg(&field_artifact(false, true)).unwrap();
}

#[test]
fn cfg_accepts_base_field_through_subclass_receiver() {
    let mut artifact = field_artifact(false, true);
    artifact.modules[0].types.push(class(0, 0, 1));
    configure_entry(
        &mut artifact,
        vec![reference(2, false), primitive(1)],
        2,
        vec![
            crate::artifact::Instruction::FieldSet {
                receiver: 0,
                field_ref: 0,
                value: 1,
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    verify_cfg(&artifact).unwrap();
}

fn verify_exceptions(
    artifact: &crate::artifact::DecodedArtifact,
) -> Result<(), crate::DiagnosticSet> {
    super::exceptions::verify_exceptions(artifact, &ArtifactLimits::default()).map(|_| ())
}

fn add_exception_register(
    artifact: &mut crate::artifact::DecodedArtifact,
    value_type: crate::artifact::ValueType,
) {
    artifact.modules[0].functions[0].register_count = 1;
    artifact.modules[0].functions[0].registers = vec![value_type];
    artifact.modules[0].functions[0].first_exception = 0;
    artifact.modules[0].functions[0].exception_count = 1;
}

#[test]
fn exception_rejects_empty_protected_range() {
    let mut artifact = decoded(support::minimal_vector());
    add_exception_register(&mut artifact, reference(0, false));
    artifact.modules[0]
        .exceptions
        .push(crate::artifact::ExceptionEntry {
            owner_function: crate::artifact::FunctionId(0),
            first_protected_block: crate::artifact::BlockId(0),
            protected_block_count: 0,
            catch_type: crate::artifact::TypeId(u32::MAX),
            handler_block: crate::artifact::BlockId(0),
            exception_register: 0,
        });
    let error = verify_exceptions(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadException);
}

#[test]
fn exception_rejects_non_reference_catch_type() {
    let mut artifact = decoded(support::minimal_vector());
    add_exception_register(&mut artifact, reference(0, false));
    artifact.modules[0]
        .exceptions
        .push(crate::artifact::ExceptionEntry {
            owner_function: crate::artifact::FunctionId(0),
            first_protected_block: crate::artifact::BlockId(0),
            protected_block_count: 1,
            catch_type: crate::artifact::TypeId(0),
            handler_block: crate::artifact::BlockId(0),
            exception_register: 0,
        });
    let error = verify_exceptions(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadException);
}

#[test]
fn exception_rejects_incompatible_exception_register() {
    let mut artifact = decoded(support::minimal_vector());
    add_exception_register(&mut artifact, primitive(1));
    artifact.modules[0]
        .exceptions
        .push(crate::artifact::ExceptionEntry {
            owner_function: crate::artifact::FunctionId(0),
            first_protected_block: crate::artifact::BlockId(0),
            protected_block_count: 1,
            catch_type: crate::artifact::TypeId(u32::MAX),
            handler_block: crate::artifact::BlockId(0),
            exception_register: 0,
        });
    let error = verify_exceptions(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadException);
}

#[test]
fn exception_rejects_crossing_ranges() {
    let mut artifact = decoded(support::minimal_vector());
    add_exception_register(&mut artifact, reference(0, false));
    artifact.modules[0].functions[0].block_count = 4;
    for block_id in 1..4 {
        artifact.modules[0].blocks.push(crate::artifact::Block {
            owner_function: crate::artifact::FunctionId(0),
            code_record: crate::artifact::BlockId(block_id),
            instruction_count: 1,
            declared_fixed_cost: 1,
            flags: 0,
        });
        artifact.modules[0].code.push(unit_return());
    }
    artifact.modules[0].exceptions = vec![
        crate::artifact::ExceptionEntry {
            owner_function: crate::artifact::FunctionId(0),
            first_protected_block: crate::artifact::BlockId(0),
            protected_block_count: 3,
            catch_type: crate::artifact::TypeId(u32::MAX),
            handler_block: crate::artifact::BlockId(3),
            exception_register: 0,
        },
        crate::artifact::ExceptionEntry {
            owner_function: crate::artifact::FunctionId(0),
            first_protected_block: crate::artifact::BlockId(2),
            protected_block_count: 2,
            catch_type: crate::artifact::TypeId(u32::MAX),
            handler_block: crate::artifact::BlockId(3),
            exception_register: 0,
        },
    ];
    artifact.modules[0].functions[0].exception_count = 2;
    let error = verify_exceptions(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadException);
}

#[test]
fn exception_rejects_suspend_in_non_suspending_function() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].blocks[0].flags = 1;
    artifact.modules[0].blocks[0].declared_fixed_cost = 2;
    artifact.modules[0].code[0].fixed_cost = 2;
    artifact.manifest.maximum_block_cost = 2;
    artifact.manifest.minimum_slice_cost = 2;
    artifact.modules[0].code[0].instructions =
        vec![crate::artifact::Instruction::Yield { resume_block: 0 }].into_boxed_slice();
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadControlFlow);
}

#[test]
fn exception_rejects_capability_id_out_of_range() {
    let mut artifact = decoded(support::minimal_vector());
    configure_entry(
        &mut artifact,
        Vec::new(),
        0,
        vec![
            crate::artifact::Instruction::CapabilityCallSync {
                dst: u16::MAX,
                capability: 0,
                operation: 0,
                args: Vec::new().into_boxed_slice(),
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadCapability);
}

#[test]
fn exception_rejects_capability_operation_out_of_range() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.capabilities.push(crate::artifact::Capability {
        namespace: 0,
        name: 1,
        abi_major: 1,
        minimum_abi_minor: 0,
        flags: 1,
        operation_count: 1,
    });
    configure_entry(
        &mut artifact,
        Vec::new(),
        0,
        vec![
            crate::artifact::Instruction::CapabilityCallSync {
                dst: u16::MAX,
                capability: 0,
                operation: 1,
                args: Vec::new().into_boxed_slice(),
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadCapability);
}

#[test]
fn exception_rejects_non_suspending_call_suspend_target() {
    let mut artifact = decoded(support::two_module_vector());
    configure_entry(
        &mut artifact,
        Vec::new(),
        0,
        vec![crate::artifact::Instruction::CallSuspend {
            dst: u16::MAX,
            function_ref: 0x8000_0000,
            args: Vec::new().into_boxed_slice(),
            resume_block: 0,
        }],
    );
    artifact.modules[0].functions[0].flags |= 1;
    if let crate::artifact::NominalType::Function { flags, .. } = &mut artifact.modules[0].types[0]
    {
        *flags |= 1;
    }
    artifact.modules[0].blocks[0].flags = 1;
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

#[test]
fn exception_rejects_missing_semantic_feature_bit() {
    let artifact = decoded(support::two_module_vector());
    let mut artifact = artifact;
    artifact.header.semantic_features = 0;
    let error = super::exceptions::verify_semantic_features(&artifact, &ArtifactLimits::default())
        .unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadModule);
}

#[test]
fn exception_rejects_unused_semantic_feature_bit() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.header.semantic_features = 1;
    let error = super::exceptions::verify_semantic_features(&artifact, &ArtifactLimits::default())
        .unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadModule);
}

fn exception_handler_artifact(reads_uninitialized_local: bool) -> crate::artifact::DecodedArtifact {
    let mut artifact = decoded(support::minimal_vector());
    artifact.header.semantic_features = 1;
    artifact.modules[0].types.push(class(0, 0, u32::MAX));
    artifact.modules[0].functions[0].register_count = 2;
    artifact.modules[0].functions[0].registers = vec![reference(1, false), primitive(1)];
    artifact.modules[0].functions[0].block_count = 2;
    artifact.modules[0].functions[0].first_exception = 0;
    artifact.modules[0].functions[0].exception_count = 1;
    artifact.modules[0].blocks = vec![
        crate::artifact::Block {
            owner_function: crate::artifact::FunctionId(0),
            code_record: crate::artifact::BlockId(0),
            instruction_count: 2,
            declared_fixed_cost: 6,
            flags: 0,
        },
        crate::artifact::Block {
            owner_function: crate::artifact::FunctionId(0),
            code_record: crate::artifact::BlockId(1),
            instruction_count: if reads_uninitialized_local { 2 } else { 1 },
            declared_fixed_cost: if reads_uninitialized_local { 2 } else { 1 },
            flags: 0,
        },
    ];
    let handler = if reads_uninitialized_local {
        vec![
            crate::artifact::Instruction::Move { dst: 1, src: 1 },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ]
    } else {
        vec![crate::artifact::Instruction::Return { value: u16::MAX }]
    };
    artifact.modules[0].code = vec![
        crate::artifact::DecodedCode {
            bytes: crate::artifact::ByteRange { start: 0, end: 0 },
            instructions: vec![
                crate::artifact::Instruction::NewObject {
                    dst: 0,
                    type_ref: 1,
                },
                crate::artifact::Instruction::Throw { exception: 0 },
            ]
            .into_boxed_slice(),
            fixed_cost: 6,
        },
        crate::artifact::DecodedCode {
            bytes: crate::artifact::ByteRange { start: 0, end: 0 },
            fixed_cost: if reads_uninitialized_local { 2 } else { 1 },
            instructions: handler.into_boxed_slice(),
        },
    ];
    artifact.manifest.maximum_block_cost = 6;
    artifact.manifest.minimum_slice_cost = 6;
    artifact.modules[0].exceptions = vec![crate::artifact::ExceptionEntry {
        owner_function: crate::artifact::FunctionId(0),
        first_protected_block: crate::artifact::BlockId(0),
        protected_block_count: 1,
        catch_type: crate::artifact::TypeId(1),
        handler_block: crate::artifact::BlockId(1),
        exception_register: 0,
    }];
    artifact
}

#[test]
fn exception_accepts_handler_initialized_from_throwing_paths() {
    verify_cfg(&exception_handler_artifact(false)).unwrap();
}

#[test]
fn exception_rejects_handler_read_not_initialized_on_throwing_paths() {
    let error = verify_cfg(&exception_handler_artifact(true)).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::UninitializedRegister);
}

#[test]
fn cfg_rejects_immutable_field_write() {
    let error = verify_cfg(&field_artifact(false, false)).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

#[test]
fn cfg_rejects_nullable_field_receiver() {
    let error = verify_cfg(&field_artifact(true, true)).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

#[test]
fn cfg_rejects_target_outside_function() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].code[0].instructions =
        vec![crate::artifact::Instruction::Jump { target: 1 }].into_boxed_slice();
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadControlFlow);
}

#[test]
fn cfg_rejects_missing_terminator() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].code[0].instructions =
        vec![crate::artifact::Instruction::Nop].into_boxed_slice();
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadControlFlow);
}

#[test]
fn cfg_rejects_noncontiguous_function_blocks() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].blocks[0].owner_function = crate::artifact::FunctionId(1);
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadControlFlow);
}

#[test]
fn cfg_rejects_value_initialized_on_only_one_join_path() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0]
        .constants
        .push(crate::artifact::Constant::I32(7));
    artifact.modules[0].types[0] = crate::artifact::NominalType::Function {
        name: 1,
        flags: 0,
        result: primitive(0),
        parameters: vec![primitive(5)],
    };
    let function = &mut artifact.modules[0].functions[0];
    function.register_count = 3;
    function.parameter_count = 1;
    function.registers = vec![primitive(5), primitive(1), primitive(1)];
    function.block_count = 3;
    artifact.manifest.maximum_block_cost = 2;
    artifact.manifest.minimum_slice_cost = 2;
    artifact.modules[0].blocks = vec![
        crate::artifact::Block {
            owner_function: crate::artifact::FunctionId(0),
            code_record: crate::artifact::BlockId(0),
            instruction_count: 1,
            declared_fixed_cost: 1,
            flags: 0,
        },
        crate::artifact::Block {
            owner_function: crate::artifact::FunctionId(0),
            code_record: crate::artifact::BlockId(1),
            instruction_count: 2,
            declared_fixed_cost: 2,
            flags: 0,
        },
        crate::artifact::Block {
            owner_function: crate::artifact::FunctionId(0),
            code_record: crate::artifact::BlockId(2),
            instruction_count: 2,
            declared_fixed_cost: 2,
            flags: 0,
        },
    ];
    artifact.modules[0].code = vec![
        crate::artifact::DecodedCode {
            bytes: crate::artifact::ByteRange { start: 0, end: 0 },
            instructions: vec![crate::artifact::Instruction::Branch {
                condition: 0,
                true_block: 1,
                false_block: 2,
            }]
            .into_boxed_slice(),
            fixed_cost: 1,
        },
        crate::artifact::DecodedCode {
            bytes: crate::artifact::ByteRange { start: 0, end: 0 },
            instructions: vec![
                crate::artifact::Instruction::Const {
                    dst: 1,
                    constant: 0,
                },
                crate::artifact::Instruction::Jump { target: 2 },
            ]
            .into_boxed_slice(),
            fixed_cost: 2,
        },
        crate::artifact::DecodedCode {
            bytes: crate::artifact::ByteRange { start: 0, end: 0 },
            instructions: vec![
                crate::artifact::Instruction::Move { dst: 2, src: 1 },
                crate::artifact::Instruction::Return { value: u16::MAX },
            ]
            .into_boxed_slice(),
            fixed_cost: 2,
        },
    ];
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::UninitializedRegister);
}

#[test]
fn cfg_rejects_wrong_call_argument_type() {
    let mut artifact = decoded(support::two_module_vector());
    artifact.modules[1].types[0] = crate::artifact::NominalType::Function {
        name: 1,
        flags: 0,
        result: primitive(0),
        parameters: vec![primitive(1)],
    };
    artifact.modules[1].functions[0].register_count = 1;
    artifact.modules[1].functions[0].parameter_count = 1;
    artifact.modules[1].functions[0].registers = vec![primitive(1)];
    configure_entry(
        &mut artifact,
        vec![primitive(5)],
        1,
        vec![
            crate::artifact::Instruction::CallDirect {
                dst: u16::MAX,
                function_ref: 0x8000_0000,
                args: vec![0].into_boxed_slice(),
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

#[test]
fn cfg_rejects_invalid_field_receiver_kind() {
    let mut artifact = field_artifact(false, true);
    configure_entry(
        &mut artifact,
        vec![primitive(1), primitive(1)],
        2,
        vec![
            crate::artifact::Instruction::FieldSet {
                receiver: 0,
                field_ref: 0,
                value: 1,
            },
            crate::artifact::Instruction::Return { value: u16::MAX },
        ],
    );
    let error = verify_cfg(&artifact).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadType);
}

#[test]
fn cfg_accepts_while_true_at_loop_header_safepoint() {
    let mut artifact = decoded(support::minimal_vector());
    artifact.modules[0].blocks[0].flags = 1;
    artifact.modules[0].code[0].instructions =
        vec![crate::artifact::Instruction::Jump { target: 0 }].into_boxed_slice();
    verify_cfg(&artifact).unwrap();
}
