use crate::{diagnostic::Code, limits::ArtifactLimits};

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
