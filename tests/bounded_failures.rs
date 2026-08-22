#[allow(dead_code)]
mod support;

use std::{panic::catch_unwind, sync::Arc};

use compukter_vm::{verify_artifact, ArtifactLimits, Code, DiagnosticSet, EntryPoint};

type Outcome = Result<([u8; 32], EntryPoint, usize), DiagnosticSet>;

fn verify(bytes: Vec<u8>, limits: ArtifactLimits) -> Outcome {
    verify_artifact(Arc::from(bytes), limits).map(|artifact| {
        (
            artifact.content_hash(),
            artifact.entry(),
            artifact.module_count(),
        )
    })
}

fn assert_limit(bytes: Vec<u8>, configure: impl FnOnce(&mut ArtifactLimits)) {
    let mut limits = ArtifactLimits::default();
    configure(&mut limits);
    let diagnostics = verify(bytes, limits).unwrap_err();
    assert_eq!(
        diagnostics.first().unwrap().code,
        Code::LimitExceeded,
        "{:?}",
        diagnostics.first().unwrap()
    );
}

#[test]
fn bounded_fixture_is_valid_at_default_limits() {
    verify(support::bounded_vector(), ArtifactLimits::default()).unwrap();
}

#[test]
fn every_single_byte_mutation_is_panic_free_and_deterministic() {
    let original = support::minimal_vector();
    let mut random = 0x4d59_5df4_d0f3_3173_u64;

    for index in 0..original.len() {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let replacements = [0, 0xff, original[index] ^ (1 << (random & 7)) as u8];

        for replacement in replacements {
            if replacement == original[index] {
                continue;
            }
            let mut bytes = original.clone();
            bytes[index] = replacement;
            let repeated = bytes.clone();

            let first = catch_unwind(|| verify(bytes, ArtifactLimits::default()));
            let second = catch_unwind(|| verify(repeated, ArtifactLimits::default()));
            assert!(first.is_ok(), "verification panicked at byte {index}");
            assert!(
                second.is_ok(),
                "repeated verification panicked at byte {index}"
            );
            let first = first.unwrap();
            let second = second.unwrap();
            assert!(first.is_err(), "mutation at byte {index} remained valid");
            assert_eq!(first, second, "byte {index}");
        }
    }
}

#[test]
fn artifact_byte_limit_is_enforced() {
    let bytes = support::minimal_vector();
    let limit = bytes.len() - 1;
    assert_limit(bytes, |limits| limits.artifact_bytes = limit);
}

#[test]
fn section_limit_is_enforced() {
    assert_limit(support::minimal_vector(), |limits| limits.sections = 12);
}

#[test]
fn module_limit_is_enforced() {
    assert_limit(support::minimal_vector(), |limits| limits.modules = 0);
}

#[test]
fn record_limit_is_enforced() {
    assert_limit(support::minimal_vector(), |limits| {
        limits.records_per_section = 1
    });
}

#[test]
fn string_byte_limit_is_enforced() {
    assert_limit(support::minimal_vector(), |limits| limits.strings_bytes = 7);
}

#[test]
fn utf16_literal_code_unit_limit_is_enforced() {
    let literal = [0x41, 0x00];
    assert_limit(
        support::minimal_vector_with_utf16_literal_records(&[&literal]),
        |limits| limits.utf16_literal_code_units = 0,
    );
}

#[test]
fn code_byte_limit_is_enforced() {
    assert_limit(support::minimal_vector(), |limits| limits.code_bytes = 5);
}

#[test]
fn function_limit_is_enforced() {
    assert_limit(support::minimal_vector(), |limits| limits.functions = 0);
}

#[test]
fn block_limit_is_enforced() {
    assert_limit(support::minimal_vector(), |limits| limits.blocks = 0);
}

#[test]
fn import_limit_is_enforced() {
    assert_limit(support::two_module_vector(), |limits| limits.imports = 0);
}

#[test]
fn register_limit_is_enforced() {
    assert_limit(support::bounded_vector(), |limits| {
        limits.registers_per_function = 0
    });
}

#[test]
fn exception_limit_is_enforced() {
    assert_limit(support::bounded_vector(), |limits| limits.exceptions = 0);
}

#[test]
fn capability_limit_is_enforced() {
    assert_limit(support::bounded_vector(), |limits| limits.capabilities = 0);
}

#[test]
fn debug_byte_limit_is_enforced() {
    assert_limit(support::bounded_vector(), |limits| limits.debug_bytes = 35);
}

#[test]
fn diagnostic_limit_bounds_public_failures() {
    let limits = ArtifactLimits {
        diagnostics: 0,
        ..ArtifactLimits::default()
    };
    let diagnostics = verify(vec![0; 64], limits).unwrap_err();
    assert!(diagnostics.is_empty());
}
