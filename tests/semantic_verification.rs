#[allow(dead_code)]
mod support;

use std::sync::Arc;

use compukter_vm::{verify_artifact, ArtifactLimits, Code};

#[test]
fn public_api_rejects_stale_module_hash() {
    let mut bytes = support::minimal_vector();
    let function_type = support::indexed_record_offset(&bytes, 0x0101, 1, 0);
    support::write_u32(&mut bytes, function_type + 4, 0);
    support::rehash(&mut bytes);
    let error = verify_artifact(Arc::from(bytes), ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadModule);
}

#[test]
fn public_api_rejects_missing_module_import_feature() {
    let mut bytes = support::two_module_vector();
    support::write_u32(&mut bytes, 20, 0);
    support::rehash(&mut bytes);
    let error = verify_artifact(Arc::from(bytes), ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadModule);
}
