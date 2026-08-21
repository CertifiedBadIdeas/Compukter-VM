#[allow(dead_code)]
mod support;

use std::{fs, path::PathBuf, sync::Arc};

use compukter_vm::{verify_artifact, ArtifactLimits};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn artifact_goldens_are_committed_and_reproducible() {
    for (name, generated, modules) in artifact_cases() {
        let committed = fs::read(fixture(&format!("{name}.cpkt"))).unwrap();
        assert_eq!(committed, generated, "{name} bytes changed");
        let committed_manifest =
            fs::read_to_string(fixture(&format!("{name}.manifest.md"))).unwrap();
        assert_eq!(
            committed_manifest,
            support::artifact_manifest(name, &committed),
            "{name} manifest changed"
        );
        let verified = verify_artifact(Arc::from(committed), ArtifactLimits::default()).unwrap();
        assert_eq!(verified.module_count(), modules);
    }
}

#[test]
#[ignore = "explicitly rewrites committed golden fixtures"]
fn regenerate_committed_fixtures() {
    fs::create_dir_all(fixture("")).unwrap();
    for (name, bytes, _) in artifact_cases() {
        fs::write(fixture(&format!("{name}.cpkt")), &bytes).unwrap();
        fs::write(
            fixture(&format!("{name}.manifest.md")),
            support::artifact_manifest(name, &bytes),
        )
        .unwrap();
    }
}

fn artifact_cases() -> [(&'static str, Vec<u8>, usize); 4] {
    [
        ("vector-a", support::minimal_vector(), 1),
        ("two-module", support::two_module_vector(), 2),
        ("language-runtime", support::language_runtime_vector(), 1),
        ("debug", support::debug_vector(), 1),
    ]
}
