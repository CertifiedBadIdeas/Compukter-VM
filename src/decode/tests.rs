use crate::{diagnostic::Code, limits::ArtifactLimits};
use std::sync::Arc;

use crate::test_support as support;

fn decoded_fixture(name: &str) -> crate::artifact::DecodedArtifact {
    assert!(matches!(name, "language-runtime.cpkt" | "debug.cpkt"));
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap();
    super::records::decode_artifact(Arc::from(bytes), &ArtifactLimits::default()).unwrap()
}

fn error_code(bytes: Vec<u8>) -> Code {
    super::container::decode_container(&bytes, &ArtifactLimits::default())
        .unwrap_err()
        .first()
        .unwrap()
        .code
}

#[test]
fn container_accepts_canonical_vector_a() {
    let bytes = support::minimal_vector();
    let container = super::container::decode_container(&bytes, &ArtifactLimits::default()).unwrap();

    assert_eq!(container.header.section_count, 14);
    assert_eq!(container.header.payload_end, 1112);
    assert_eq!(container.directory.len(), 14);
    assert_eq!(container.bytes.len(), 1144);
    assert_eq!(
        container.header.entry_arguments,
        crate::artifact::EntryArguments::None
    );
}

#[test]
fn container_rejects_format_v1_after_the_entry_contract_bump() {
    let mut bytes = support::minimal_vector();
    support::write_u16(&mut bytes, 4, 1);
    support::rehash(&mut bytes);
    assert_eq!(error_code(bytes), Code::UnsupportedVersion);
}

#[test]
fn container_decodes_the_string_array_entry_tag() {
    let mut bytes = support::minimal_vector();
    support::write_u16(&mut bytes, 4, 2);
    bytes[48] = 1;
    support::rehash(&mut bytes);
    let container = super::container::decode_container(&bytes, &ArtifactLimits::default()).unwrap();
    assert_eq!(
        container.header.entry_arguments,
        crate::artifact::EntryArguments::StringArray
    );
}

#[test]
fn container_rejects_bad_magic_before_directory_decode() {
    let mut bytes = support::minimal_vector();
    bytes[0] = b'X';
    support::rehash(&mut bytes);
    assert_eq!(error_code(bytes), Code::BadMagic);
}

#[test]
fn container_rejects_bad_digest() {
    let mut bytes = support::minimal_vector();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert_eq!(error_code(bytes), Code::BadDigest);
}

#[test]
fn container_rejects_directory_overlap() {
    assert_eq!(
        error_code(support::minimal_vector_with_overlapping_sections()),
        Code::BadDirectory
    );
}

#[test]
fn container_rejects_non_zero_alignment_gap() {
    assert_eq!(
        error_code(support::minimal_vector_with_non_zero_gap()),
        Code::BadDirectory
    );
}

#[test]
fn container_rejects_unknown_feature_bit() {
    let mut bytes = support::minimal_vector();
    support::write_u32(&mut bytes, 20, 1 << 4);
    support::rehash(&mut bytes);
    assert_eq!(error_code(bytes), Code::UnsupportedVersion);
}

#[test]
fn container_rejects_unsupported_major() {
    let mut bytes = support::minimal_vector();
    support::write_u16(&mut bytes, 4, 3);
    support::rehash(&mut bytes);
    assert_eq!(error_code(bytes), Code::UnsupportedVersion);
}

#[test]
fn container_rejects_duplicate_directory_key() {
    let mut bytes = support::minimal_vector();
    support::write_u16(&mut bytes, 64 + 32, 0x0001);
    support::rehash(&mut bytes);
    assert_eq!(error_code(bytes), Code::BadDirectory);
}

#[test]
fn container_rejects_wrong_section_scope() {
    let mut bytes = support::minimal_vector();
    support::write_u32(&mut bytes, 64 + 4, 1);
    support::rehash(&mut bytes);
    assert_eq!(error_code(bytes), Code::BadSection);
}

#[test]
fn container_rejects_wrong_first_payload_offset() {
    let mut bytes = support::minimal_vector();
    support::write_u64(&mut bytes, 64 + 8, 488);
    support::rehash(&mut bytes);
    assert_eq!(error_code(bytes), Code::BadDirectory);
}

#[test]
fn container_rejects_truncated_trailer() {
    let mut bytes = support::minimal_vector();
    bytes.pop();
    assert_eq!(error_code(bytes), Code::BadLength);
}

fn strings_error(bytes: Vec<u8>) -> Code {
    let limits = ArtifactLimits::default();
    let container = super::container::decode_container(&bytes, &limits).unwrap();
    let entry = container
        .directory
        .iter()
        .find(|entry| entry.kind == crate::artifact::format::STRINGS)
        .unwrap();
    super::indexed::decode_string_table(&container, entry, &limits)
        .unwrap_err()
        .code
}

#[test]
fn indexed_accepts_canonical_string_table() {
    let bytes = support::minimal_vector();
    let limits = ArtifactLimits::default();
    let container = super::container::decode_container(&bytes, &limits).unwrap();
    let entry = container
        .directory
        .iter()
        .find(|entry| entry.kind == crate::artifact::format::STRINGS)
        .unwrap();

    let strings = super::indexed::decode_string_table(&container, entry, &limits).unwrap();
    assert_eq!(strings, ["app", "entry"]);
}

#[test]
fn indexed_rejects_non_zero_first_offset() {
    let mut bytes = support::minimal_vector();
    let strings = support::section_offset(&bytes, crate::artifact::format::STRINGS, 1);
    support::write_u32(&mut bytes, strings + 16, 1);
    support::rehash(&mut bytes);
    assert_eq!(strings_error(bytes), Code::BadRecord);
}

#[test]
fn indexed_rejects_decreasing_offsets() {
    let mut bytes = support::minimal_vector();
    let strings = support::section_offset(&bytes, crate::artifact::format::STRINGS, 1);
    support::write_u32(&mut bytes, strings + 20, 6);
    support::write_u32(&mut bytes, strings + 24, 5);
    support::rehash(&mut bytes);
    assert_eq!(strings_error(bytes), Code::BadRecord);
}

#[test]
fn indexed_rejects_last_offset_mismatch() {
    let mut bytes = support::minimal_vector();
    let strings = support::section_offset(&bytes, crate::artifact::format::STRINGS, 1);
    support::write_u32(&mut bytes, strings + 24, 7);
    support::rehash(&mut bytes);
    assert_eq!(strings_error(bytes), Code::BadRecord);
}

#[test]
fn indexed_rejects_non_zero_envelope_padding() {
    let mut bytes = support::minimal_vector();
    let strings = support::section_offset(&bytes, crate::artifact::format::STRINGS, 1);
    bytes[strings + 28] = 1;
    support::rehash(&mut bytes);
    assert_eq!(strings_error(bytes), Code::BadRecord);
}

#[test]
fn indexed_rejects_directory_count_disagreement() {
    let mut bytes = support::minimal_vector();
    let strings = support::directory_entry_offset(&bytes, crate::artifact::format::STRINGS, 1);
    support::write_u32(&mut bytes, strings + 24, 1);
    support::rehash(&mut bytes);
    assert_eq!(strings_error(bytes), Code::BadRecord);
}

#[test]
fn indexed_rejects_invalid_utf8() {
    let bytes = support::minimal_vector_with_string_records(&[b"app", &[0xff]]);
    assert_eq!(strings_error(bytes), Code::InvalidUtf8);
}

#[test]
fn indexed_rejects_unsorted_strings() {
    let bytes = support::minimal_vector_with_string_records(&[b"z", b"a"]);
    assert_eq!(strings_error(bytes), Code::BadRecord);
}

#[test]
fn indexed_rejects_duplicate_strings() {
    let bytes = support::minimal_vector_with_string_records(&[b"same", b"same"]);
    assert_eq!(strings_error(bytes), Code::BadRecord);
}

#[test]
fn indexed_rejects_empty_string_after_index_zero() {
    let bytes = support::minimal_vector_with_string_records(&[b"app", b""]);
    assert_eq!(strings_error(bytes), Code::BadRecord);
}

#[test]
fn indexed_checks_record_limit_before_offset_allocation() {
    let bytes = support::minimal_vector();
    let limits = ArtifactLimits {
        records_per_section: 1,
        ..ArtifactLimits::default()
    };
    let error = super::container::decode_container(&bytes, &limits).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::LimitExceeded);
}

#[test]
fn indexed_checks_total_string_byte_limit() {
    let bytes = support::minimal_vector();
    let limits = ArtifactLimits {
        strings_bytes: 7,
        ..ArtifactLimits::default()
    };
    let container = super::container::decode_container(&bytes, &limits).unwrap();
    let entry = container
        .directory
        .iter()
        .find(|entry| entry.kind == crate::artifact::format::STRINGS)
        .unwrap();
    let error = super::indexed::decode_string_table(&container, entry, &limits).unwrap_err();
    assert_eq!(error.code, Code::LimitExceeded);
}

#[test]
fn indexed_rejects_record_id_outside_table() {
    let bytes = support::minimal_vector();
    let limits = ArtifactLimits::default();
    let container = super::container::decode_container(&bytes, &limits).unwrap();
    let entry = container
        .directory
        .iter()
        .find(|entry| entry.kind == crate::artifact::format::STRINGS)
        .unwrap();
    let section = super::indexed::IndexedSection::decode(&container, entry, &limits).unwrap();
    assert_eq!(section.record(2).unwrap_err().code, Code::BadRecord);
}

fn utf16_literal_result(
    records: &[&[u8]],
) -> Result<crate::artifact::DecodedArtifact, crate::diagnostic::DiagnosticSet> {
    super::records::decode_artifact(
        Arc::from(support::minimal_vector_with_utf16_literal_records(records)),
        &ArtifactLimits::default(),
    )
}

#[test]
fn utf16_literal_pool_preserves_every_code_unit_pattern() {
    let empty = [];
    let nul = [0x00, 0x00];
    let high_surrogate = [0x00, 0xd8];
    let low_surrogate = [0x00, 0xdc];
    let surrogate_pair = [0x3d, 0xd8, 0x80, 0xde];
    let artifact = utf16_literal_result(&[
        &empty,
        &nul,
        &high_surrogate,
        &low_surrogate,
        &surrogate_pair,
    ])
    .unwrap();
    let actual = artifact.modules[0]
        .utf16_literals
        .iter()
        .map(|range| range.slice(&artifact.bytes))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        [
            &empty[..],
            &nul,
            &high_surrogate,
            &low_surrogate,
            &surrogate_pair
        ]
    );
}

#[test]
fn utf16_literal_pool_rejects_odd_byte_length() {
    let error = utf16_literal_result(&[&[0x41]]).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadRecord);
}

#[test]
fn utf16_literal_pool_rejects_duplicate_records() {
    let error = utf16_literal_result(&[&[0x41, 0], &[0x41, 0]]).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadRecord);
}

#[test]
fn utf16_literal_pool_rejects_non_increasing_raw_bytes() {
    let error = utf16_literal_result(&[&[0x00, 0xdc], &[0x00, 0xd8]]).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadRecord);
}

fn verify_error_code(bytes: Vec<u8>) -> Code {
    crate::verify_artifact(Arc::from(bytes), ArtifactLimits::default())
        .unwrap_err()
        .first()
        .unwrap()
        .code
}

#[test]
fn utf16_literal_section_is_required() {
    let mut bytes = support::minimal_vector();
    let entry = support::directory_entry_offset(&bytes, crate::artifact::format::UTF16_LITERALS, 1);
    support::write_u16(&mut bytes, entry, 0x8000);
    support::write_u16(&mut bytes, entry + 2, 0);
    support::rehash(&mut bytes);
    assert_eq!(verify_error_code(bytes), Code::BadSection);
}

#[test]
fn utf16_literal_section_rejects_global_scope() {
    let mut bytes = support::minimal_vector();
    let entry = support::directory_entry_offset(&bytes, crate::artifact::format::UTF16_LITERALS, 1);
    support::write_u32(&mut bytes, entry + 4, 0);
    support::rehash(&mut bytes);
    assert_eq!(verify_error_code(bytes), Code::BadSection);
}

#[test]
fn utf16_literal_section_requires_critical_semantic_flags() {
    let mut bytes = support::minimal_vector();
    let entry = support::directory_entry_offset(&bytes, crate::artifact::format::UTF16_LITERALS, 1);
    support::write_u16(&mut bytes, entry + 2, 0);
    support::rehash(&mut bytes);
    assert_eq!(verify_error_code(bytes), Code::BadSection);
}

#[test]
fn utf16_literal_section_rejects_duplicate_directory_key() {
    let mut bytes = support::minimal_vector();
    let exceptions =
        support::directory_entry_offset(&bytes, crate::artifact::format::EXCEPTIONS, 1);
    support::write_u16(
        &mut bytes,
        exceptions,
        crate::artifact::format::UTF16_LITERALS,
    );
    support::rehash(&mut bytes);
    assert_eq!(verify_error_code(bytes), Code::BadDirectory);
}

#[test]
fn string_constants_address_the_utf16_literal_pool() {
    let literals = [&[][..], &[0x00, 0x00][..], &[0x41, 0x00][..]];
    let artifact = super::records::decode_artifact(
        Arc::from(support::minimal_vector_with_utf16_string_constant(
            &literals, 2,
        )),
        &ArtifactLimits::default(),
    )
    .unwrap();
    assert_eq!(artifact.modules[0].constants.len(), 1);

    let error = super::records::decode_artifact(
        Arc::from(support::minimal_vector_with_utf16_string_constant(
            &literals, 3,
        )),
        &ArtifactLimits::default(),
    )
    .unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadRecord);
}

#[test]
fn test_encoder_preserves_utf16_literal_pool_bytes() {
    let records = [&[][..], &[0x00, 0xd8][..], &[0x3d, 0xd8, 0x80, 0xde][..]];
    let original = support::minimal_vector_with_utf16_literal_records(&records);
    let decoded =
        super::records::decode_artifact(Arc::from(original.clone()), &ArtifactLimits::default())
            .unwrap();
    let encoded = crate::test_encode::encode_artifact(&decoded).unwrap();
    assert_eq!(encoded, original);
}

#[test]
fn records_decode_spec_vector_a() {
    let bytes = support::minimal_vector();
    assert_eq!(bytes.len(), 1144);
    assert_eq!(
        support::artifact_hash(&bytes),
        support::hex32("ffdf3638f06189ffee32fe1c0df945a4f47e4a4efe7064fd37099139e3b809ac")
    );
    let artifact =
        super::records::decode_artifact(Arc::from(bytes), &ArtifactLimits::default()).unwrap();

    assert_eq!(artifact.modules.len(), 1);
    assert_eq!(artifact.header.entry_module, 0);
    assert_eq!(artifact.header.entry_function, 0);
    let module = &artifact.modules[0];
    assert_eq!(module.name_string, 0);
    assert_eq!(module.flags, 1);
    assert_eq!(module.declared_imports, 0);
    assert_eq!(module.declared_exports, 0);
    assert_eq!(module.declared_types, 1);
    assert_eq!(module.declared_functions, 1);
    let strings: Vec<_> = module
        .strings
        .iter()
        .map(|range| std::str::from_utf8(range.slice(&artifact.bytes)).unwrap())
        .collect();
    assert_eq!(strings, ["app", "entry"]);
    assert_eq!(
        artifact.content_hash,
        support::artifact_hash(&artifact.bytes)
    );
    assert_eq!(artifact.manifest.maximum_coroutines, 1);
    assert_eq!(artifact.manifest.maximum_call_depth, 1);
    assert_eq!(artifact.manifest.maximum_block_cost, 1);
    assert_eq!(artifact.manifest.minimum_slice_cost, 1);
    assert_eq!(artifact.capabilities.len(), 0);
    assert_eq!(artifact.modules[0].types.len(), 1);
    assert!(matches!(
        artifact.modules[0].types[0],
        crate::artifact::NominalType::Function { ref parameters, .. } if parameters.is_empty()
    ));
    assert!(artifact.modules[0].constants.is_empty());
    assert!(artifact.modules[0].imports.is_empty());
    assert!(artifact.modules[0].exports.is_empty());
    assert!(artifact.modules[0].fields.is_empty());
    assert_eq!(artifact.modules[0].functions.len(), 1);
    assert_eq!(artifact.modules[0].functions[0].block_count, 1);
    assert_eq!(artifact.modules[0].blocks.len(), 1);
    assert_eq!(artifact.modules[0].blocks[0].declared_fixed_cost, 1);
    assert_eq!(artifact.modules[0].code[0].instructions.len(), 1);
    assert_eq!(artifact.modules[0].code[0].fixed_cost, 1);
    assert_eq!(
        artifact.modules[0].code[0].bytes.slice(&artifact.bytes),
        [0xe3, 0, 6, 0, 0xff, 0xff]
    );
    assert!(artifact.modules[0].exceptions.is_empty());
    assert!(artifact.modules[0].debug.is_empty());
    assert_eq!(
        artifact.modules[0].semantic_hash,
        support::hex32("f1379df5fe4e751a1df57cf6be2d1575956f8c3e3ebaabe795820b44de2185ee")
    );
}

#[test]
fn committed_artifacts_reencode_byte_for_byte() {
    let fixtures: [(&str, &[u8]); 4] = [
        (
            "vector-a.cpkt",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/vector-a.cpkt"
            )),
        ),
        (
            "two-module.cpkt",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/two-module.cpkt"
            )),
        ),
        (
            "language-runtime.cpkt",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/language-runtime.cpkt"
            )),
        ),
        (
            "debug.cpkt",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/debug.cpkt"
            )),
        ),
    ];

    for (name, original) in fixtures {
        let decoded =
            super::records::decode_artifact(Arc::from(original), &ArtifactLimits::default())
                .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let encoded = crate::test_encode::encode_artifact(&decoded)
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(encoded, original, "{name}");
    }
}

#[test]
fn records_reject_module_count_disagreement() {
    let mut bytes = support::minimal_vector();
    let module = support::indexed_record_offset(&bytes, crate::artifact::format::MODULES, 0, 0);
    support::write_u32(&mut bytes, module + 48, 0);
    support::rehash(&mut bytes);
    let error =
        super::records::decode_artifact(Arc::from(bytes), &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadRecord);
}

#[test]
fn records_reject_out_of_range_local_ids() {
    let mut bytes = support::minimal_vector();
    let module = support::indexed_record_offset(&bytes, crate::artifact::format::MODULES, 0, 0);
    support::write_u32(&mut bytes, module, 99);
    support::rehash(&mut bytes);
    let error =
        super::records::decode_artifact(Arc::from(bytes), &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadRecord);
}

#[test]
fn records_reject_function_parameter_count_above_register_count() {
    let mut bytes = support::minimal_vector();
    let function = support::indexed_record_offset(&bytes, crate::artifact::format::FUNCTIONS, 1, 0);
    support::write_u16(&mut bytes, function + 18, 1);
    support::rehash(&mut bytes);
    let error =
        super::records::decode_artifact(Arc::from(bytes), &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadRecord);
}

#[test]
fn records_reject_block_code_id_disagreement() {
    let mut bytes = support::minimal_vector();
    let block = support::indexed_record_offset(&bytes, crate::artifact::format::BLOCKS, 1, 0);
    support::write_u32(&mut bytes, block + 4, 1);
    support::rehash(&mut bytes);
    let error =
        super::records::decode_artifact(Arc::from(bytes), &ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadRecord);
}

#[test]
fn records_enforce_artifact_wide_table_limits() {
    let limits = [
        ArtifactLimits {
            strings_bytes: 7,
            ..ArtifactLimits::default()
        },
        ArtifactLimits {
            code_bytes: 5,
            ..ArtifactLimits::default()
        },
        ArtifactLimits {
            functions: 0,
            ..ArtifactLimits::default()
        },
        ArtifactLimits {
            blocks: 0,
            ..ArtifactLimits::default()
        },
    ];
    for limits in limits {
        let error = super::records::decode_artifact(Arc::from(support::minimal_vector()), &limits)
            .unwrap_err();
        assert_eq!(error.first().unwrap().code, Code::LimitExceeded);
    }
}

fn instruction_error(bytes: &[u8], count: u32, limits: &ArtifactLimits) -> Code {
    super::code::decode_code_record(bytes, count, limits)
        .unwrap_err()
        .first()
        .unwrap()
        .code
}

#[test]
fn instruction_decodes_unit_return_and_arithmetic_block() {
    let unit_return = [0xe3, 0, 6, 0, 0xff, 0xff];
    let instructions =
        super::code::decode_code_record(&unit_return, 1, &ArtifactLimits::default()).unwrap();
    assert_eq!(instructions.len(), 1);
    assert_eq!(instructions[0].fixed_cost().unwrap(), 1);

    let arithmetic = [0x10, 1, 10, 0, 0, 0, 1, 0, 2, 0, 0xe3, 0, 6, 0, 0, 0];
    let instructions =
        super::code::decode_code_record(&arithmetic, 2, &ArtifactLimits::default()).unwrap();
    assert_eq!(instructions.len(), 2);
    assert_eq!(
        instructions
            .iter()
            .map(|value| value.fixed_cost().unwrap())
            .sum::<u32>(),
        2
    );
}

#[test]
fn instruction_rejects_unknown_opcode_form_and_bad_lengths() {
    let limits = ArtifactLimits::default();
    assert_eq!(
        instruction_error(&[0x05, 0, 4, 0], 1, &limits),
        Code::BadInstruction
    );
    assert_eq!(
        instruction_error(&[0x00, 1, 4, 0], 1, &limits),
        Code::BadInstruction
    );
    assert_eq!(
        instruction_error(&[0x00, 0, 3, 0], 1, &limits),
        Code::BadInstruction
    );
    assert_eq!(
        instruction_error(&[0x01, 0, 8, 0, 0, 0], 1, &limits),
        Code::BadInstruction
    );
    assert_eq!(
        instruction_error(&[0x01, 0, 10, 0, 0, 0, 1, 0, 0, 0], 1, &limits),
        Code::BadInstruction
    );
}

#[test]
fn instruction_rejects_noncanonical_ids_and_excessive_lists() {
    let limits = ArtifactLimits::default();
    assert_eq!(
        instruction_error(&[0x02, 0, 8, 0, 0, 0, 0x80, 0], 1, &limits),
        Code::NonCanonicalUleb128
    );

    let strict = ArtifactLimits {
        registers_per_function: 1,
        ..ArtifactLimits::default()
    };
    let call = [0x40, 0, 12, 0, 0xff, 0xff, 0, 2, 0, 0, 1, 0];
    assert_eq!(instruction_error(&call, 1, &strict), Code::LimitExceeded);

    let switch = [0xe2, 0, 10, 0, 0, 0, 0, 2, 0, 0];
    assert_eq!(instruction_error(&switch, 1, &strict), Code::LimitExceeded);
}

#[test]
fn instruction_allows_absent_register_only_for_optional_operands() {
    let limits = ArtifactLimits::default();
    let invalid_move = [0x01, 0, 8, 0, 0xff, 0xff, 0, 0];
    assert_eq!(
        instruction_error(&invalid_move, 1, &limits),
        Code::BadInstruction
    );
    let invalid_spawn = [0x50, 0, 8, 0, 0xff, 0xff, 0, 0];
    assert_eq!(
        instruction_error(&invalid_spawn, 1, &limits),
        Code::BadInstruction
    );
    super::code::decode_code_record(&[0xe3, 0, 6, 0, 0xff, 0xff], 1, &limits).unwrap();
}

#[test]
fn instruction_requires_one_final_terminator_and_exact_count() {
    let limits = ArtifactLimits::default();
    assert_eq!(
        instruction_error(&[0, 0, 4, 0], 1, &limits),
        Code::BadInstruction
    );
    assert_eq!(
        instruction_error(&[0xe3, 0, 6, 0, 0xff, 0xff, 0, 0, 4, 0], 2, &limits),
        Code::BadInstruction
    );
    assert_eq!(
        instruction_error(&[0xe3, 0, 6, 0, 0xff, 0xff], 2, &limits),
        Code::BadInstruction
    );
}

fn for_each_v1_opcode_case(mut check: impl FnMut(u8, u8, &[u8], bool)) {
    let r2 = &[0, 0, 1, 0][..];
    let r3 = &[0, 0, 1, 0, 2, 0][..];
    let r4 = &[0, 0, 1, 0, 2, 0, 3, 0][..];
    let cases: &[(u8, u8, &[u8], bool)] = &[
        (0x00, 0, &[], false),
        (0x01, 0, r2, false),
        (0x02, 0, &[0, 0, 0], false),
        (0x03, 0, &[0, 0], false),
        (0x04, 0, r2, false),
        (0x10, 1, r3, false),
        (0x11, 2, r3, false),
        (0x12, 3, r3, false),
        (0x13, 4, r3, false),
        (0x14, 1, r3, false),
        (0x15, 2, r2, false),
        (0x16, 1, r3, false),
        (0x17, 2, r3, false),
        (0x18, 1, r3, false),
        (0x19, 2, r3, false),
        (0x1a, 1, r3, false),
        (0x1b, 2, r3, false),
        (0x20, 5, r3, false),
        (0x21, 6, r3, false),
        (0x22, 6, r3, false),
        (0x23, 1, r3, false),
        (0x24, 2, r3, false),
        (0x25, 3, r3, false),
        (0x26, 7, r3, false),
        (0x27, 7, r3, false),
        (0x30, 0, &[0, 0, 0], false),
        (0x31, 0, &[0, 0, 0, 1, 0], false),
        (0x32, 0, r2, false),
        (0x33, 0, r3, false),
        (0x34, 0, r3, false),
        (0x35, 0, &[0, 0, 1, 0, 0], false),
        (0x36, 0, &[0, 0, 0, 1, 0], false),
        (0x37, 0, &[0, 0, 0], false),
        (0x38, 0, &[0, 1, 0], false),
        (0x39, 0, &[0, 0, 1, 0, 0], false),
        (0x3a, 0, &[0, 0, 1, 0, 0], false),
        (0x40, 0, &[0xff, 0xff, 0, 0], false),
        (0x41, 0, &[0xff, 0xff, 0, 0], false),
        (0x42, 0, &[0xff, 0xff, 0, 0], false),
        (0x50, 0, &[0, 0, 0, 0], false),
        (0x51, 0, &[0xff, 0xff, 0, 0, 0], false),
        (0x60, 0, r2, false),
        (0x61, 0, r3, false),
        (0x62, 0, r3, false),
        (0x63, 0, r3, false),
        (0x64, 0, r2, false),
        (0x65, 0, r3, false),
        (0x66, 0, r4, false),
        (0xe0, 0, &[0], true),
        (0xe1, 0, &[0, 0, 0, 0], true),
        (0xe2, 0, &[0, 0, 0, 0], true),
        (0xe3, 0, &[0xff, 0xff], true),
        (0xe4, 0, &[0, 0], true),
        (0xe5, 0, &[0xff, 0xff, 0, 0, 0], true),
        (0xe6, 0, &[0], true),
        (0xe7, 0, &[0, 0, 0], true),
        (0xe8, 0, &[0xff, 0xff, 0, 0, 0], true),
        (0xe9, 0, &[0xff, 0xff, 0, 0, 0, 0], true),
        (0xff, 0, &[], true),
    ];

    for &(opcode, form, operands, terminator) in cases {
        check(opcode, form, operands, terminator);
    }
}

#[test]
fn instruction_decodes_every_v1_opcode() {
    for_each_v1_opcode_case(|opcode, form, operands, terminator| {
        let mut bytes = vec![opcode, form, (operands.len() + 4) as u8, 0];
        bytes.extend_from_slice(operands);
        let count = if terminator {
            1
        } else {
            bytes.extend_from_slice(&[0xe3, 0, 6, 0, 0xff, 0xff]);
            2
        };
        super::code::decode_code_record(&bytes, count, &ArtifactLimits::default())
            .unwrap_or_else(|error| panic!("opcode {opcode:#04x}: {error:?}"));
    });
}

#[test]
fn instruction_reencodes_every_v1_opcode() {
    for_each_v1_opcode_case(|opcode, form, operands, terminator| {
        let mut original = vec![opcode, form, (operands.len() + 4) as u8, 0];
        original.extend_from_slice(operands);
        let count = if terminator {
            1
        } else {
            original.extend_from_slice(&[0xe3, 0, 6, 0, 0xff, 0xff]);
            2
        };
        let decoded =
            super::code::decode_code_record(&original, count, &ArtifactLimits::default()).unwrap();
        let encoded = crate::test_encode::encode_instruction_record(&decoded)
            .unwrap_or_else(|error| panic!("opcode {opcode:#04x}: {error:?}"));
        assert_eq!(encoded, original, "opcode {opcode:#04x}");
    });
}

#[test]
fn instruction_rejects_unsorted_switch_cases() {
    let bytes = [0xe2, 0, 18, 0, 0, 0, 0, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(
        instruction_error(&bytes, 1, &ArtifactLimits::default()),
        Code::BadInstruction
    );
}

#[test]
fn instruction_computes_variable_fixed_costs() {
    let call = [
        0x40, 0, 12, 0, 0xff, 0xff, 0, 2, 0, 0, 1, 0, 0xe3, 0, 6, 0, 0xff, 0xff,
    ];
    let decoded = super::code::decode_code_record(&call, 2, &ArtifactLimits::default()).unwrap();
    assert_eq!(decoded[0].fixed_cost().unwrap(), 6);

    let switch = [0xe2, 0, 18, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 1, 0, 0, 1];
    let decoded = super::code::decode_code_record(&switch, 1, &ArtifactLimits::default()).unwrap();
    assert_eq!(decoded[0].fixed_cost().unwrap(), 3);

    let asynchronous = [0xe9, 0, 14, 0, 0xff, 0xff, 0, 0, 2, 0, 0, 1, 0, 0];
    let decoded =
        super::code::decode_code_record(&asynchronous, 1, &ArtifactLimits::default()).unwrap();
    assert_eq!(decoded[0].fixed_cost().unwrap(), 8);
}

#[test]
fn records_decode_language_runtime_golden_features() {
    let artifact = decoded_fixture("language-runtime.cpkt");
    let module = &artifact.modules[0];
    assert!(module
        .types
        .iter()
        .any(|value| matches!(value, crate::artifact::NominalType::Class { .. })));
    assert!(module
        .types
        .iter()
        .any(|value| matches!(value, crate::artifact::NominalType::Array { .. })));
    assert!(module.functions[0]
        .registers
        .iter()
        .any(|value| value.kind == 7 && value.flags == 1));
    let mut instructions = module.code.iter().flat_map(|code| code.instructions.iter());
    assert!(instructions
        .clone()
        .any(|value| matches!(value, crate::artifact::Instruction::NewObject { .. })));
    assert!(instructions
        .clone()
        .any(|value| matches!(value, crate::artifact::Instruction::NewArray { .. })));
    assert!(instructions.clone().any(|value| matches!(
        value,
        crate::artifact::Instruction::ArrayLoad { .. }
            | crate::artifact::Instruction::ArrayStore { .. }
    )));
    assert!(instructions.any(|value| matches!(value, crate::artifact::Instruction::Branch { .. })));
    assert!(module.blocks.iter().any(|block| block.flags & 1 != 0));
    assert_eq!(module.exceptions.len(), 1);
}

#[test]
fn records_decode_debug_golden_inline_ancestry() {
    let artifact = decoded_fixture("debug.cpkt");
    let debug = &artifact.modules[0].debug;
    assert_eq!(debug.len(), 2);
    assert_eq!(debug[0].inline_parent, u32::MAX);
    assert_eq!(debug[1].inline_parent, 0);
    assert!(debug[0].end_utf16 > debug[0].start_utf16);
    assert!(debug[1].end_utf16 > debug[1].start_utf16);
}

#[test]
fn instruction_decodes_host_runtime_golden() {
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/host-runtime.code"),
    )
    .unwrap();
    let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    assert_eq!(count, 5);
    let records_start = (16 + 4 * (count + 1) + 7) & !7;
    let expected_counts = [2, 1, 1, 2, 1];
    let expected_costs = [7, 3, 4, 6, 6];
    let mut decoded = Vec::new();
    for id in 0..count {
        let start =
            u32::from_le_bytes(bytes[16 + id * 4..20 + id * 4].try_into().unwrap()) as usize;
        let end = u32::from_le_bytes(bytes[20 + id * 4..24 + id * 4].try_into().unwrap()) as usize;
        let instructions = super::code::decode_code_record(
            &bytes[records_start + start..records_start + end],
            expected_counts[id],
            &ArtifactLimits::default(),
        )
        .unwrap();
        assert_eq!(
            instructions
                .iter()
                .map(|instruction| instruction.fixed_cost().unwrap())
                .sum::<u32>(),
            expected_costs[id]
        );
        decoded.push(instructions);
    }
    assert!(matches!(
        decoded[0][0],
        crate::artifact::Instruction::CoroutineSpawn { .. }
    ));
    assert!(matches!(
        decoded[1][0],
        crate::artifact::Instruction::Sleep { .. }
    ));
    assert!(matches!(
        decoded[2][0],
        crate::artifact::Instruction::CoroutineJoin { .. }
    ));
    assert!(matches!(
        decoded[3][0],
        crate::artifact::Instruction::CapabilityCallSync { .. }
    ));
    assert!(matches!(
        decoded[4][0],
        crate::artifact::Instruction::CapabilityCallAsync { .. }
    ));
}
