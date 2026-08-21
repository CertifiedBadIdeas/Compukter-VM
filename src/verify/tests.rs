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
