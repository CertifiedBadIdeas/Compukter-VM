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
