#[allow(dead_code)]
mod support;

use std::sync::Arc;

use compukter_vm::{verify_artifact, ArtifactLimits, EntryArguments, EntryPoint};

#[test]
fn publishes_exact_vector_a_only_after_verification() {
    let bytes = support::minimal_vector();
    let expected_hash = support::artifact_hash(&bytes);
    let artifact = verify_artifact(Arc::from(bytes), ArtifactLimits::default()).unwrap();

    assert_eq!(artifact.content_hash(), expected_hash);
    assert_eq!(
        artifact.entry(),
        EntryPoint {
            module: 0,
            function: 0,
            arguments: EntryArguments::None,
        }
    );
    assert_eq!(artifact.module_count(), 1);
}

#[test]
fn publishes_two_module_bundle() {
    let artifact = verify_artifact(
        Arc::from(support::two_module_vector()),
        ArtifactLimits::default(),
    )
    .unwrap();
    assert_eq!(artifact.module_count(), 2);
}
