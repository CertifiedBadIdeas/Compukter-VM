#[allow(dead_code)]
mod support;

use std::sync::Arc;

use compukter_vm::{verify_artifact, ArtifactLimits, Code};

#[test]
fn public_api_rejects_bad_container() {
    let mut bytes = support::minimal_vector();
    bytes[0] = b'X';
    support::rehash(&mut bytes);
    let error = verify_artifact(Arc::from(bytes), ArtifactLimits::default()).unwrap_err();
    assert_eq!(error.first().unwrap().code, Code::BadMagic);
}
