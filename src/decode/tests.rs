use crate::{diagnostic::Code, limits::ArtifactLimits};
use std::sync::Arc;

#[path = "../../tests/support/mod.rs"]
mod support;

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

    assert_eq!(container.header.section_count, 13);
    assert_eq!(container.header.payload_end, 1056);
    assert_eq!(container.directory.len(), 13);
    assert_eq!(container.bytes.len(), 1088);
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
    support::write_u16(&mut bytes, 4, 2);
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
    support::write_u32(&mut bytes, 720, 1);
    support::rehash(&mut bytes);
    assert_eq!(strings_error(bytes), Code::BadRecord);
}

#[test]
fn indexed_rejects_decreasing_offsets() {
    let mut bytes = support::minimal_vector();
    support::write_u32(&mut bytes, 724, 6);
    support::write_u32(&mut bytes, 728, 5);
    support::rehash(&mut bytes);
    assert_eq!(strings_error(bytes), Code::BadRecord);
}

#[test]
fn indexed_rejects_last_offset_mismatch() {
    let mut bytes = support::minimal_vector();
    support::write_u32(&mut bytes, 728, 7);
    support::rehash(&mut bytes);
    assert_eq!(strings_error(bytes), Code::BadRecord);
}

#[test]
fn indexed_rejects_non_zero_envelope_padding() {
    let mut bytes = support::minimal_vector();
    bytes[732] = 1;
    support::rehash(&mut bytes);
    assert_eq!(strings_error(bytes), Code::BadRecord);
}

#[test]
fn indexed_rejects_directory_count_disagreement() {
    let mut bytes = support::minimal_vector();
    support::write_u32(&mut bytes, 704, 1);
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

#[test]
fn records_decode_spec_vector_a() {
    let bytes = support::minimal_vector();
    assert_eq!(bytes.len(), 1088);
    assert_eq!(
        support::artifact_hash(&bytes),
        support::hex32("88803a07260a3b0123ef230b482a682400e6cae03e90f3be0a117419406509d3")
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
    assert_eq!(
        artifact.modules[0].code[0].slice(&artifact.bytes),
        [0xe3, 0, 6, 0, 0xff, 0xff]
    );
    assert!(artifact.modules[0].exceptions.is_empty());
    assert!(artifact.modules[0].debug.is_empty());
    assert_eq!(
        artifact.modules[0].semantic_hash,
        support::hex32("f73d8f8699e060aac0df1079d820a9fd778a649dd391980c23ee2a4e3c17c2cc")
    );
}

#[test]
fn records_reject_module_count_disagreement() {
    let mut bytes = support::minimal_vector();
    support::write_u32(&mut bytes, 664, 0);
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
