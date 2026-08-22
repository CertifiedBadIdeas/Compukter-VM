use std::sync::Arc;

use crate::{verify_artifact, ArtifactLimits, VerifiedArtifact};

use super::{image::ExecutionProfile, TypeKey};

pub(super) fn scalar_artifact() -> VerifiedArtifact {
    verified_with_stack(crate::test_support::minimal_vector(), 32)
}

pub(super) fn artifact_with_new_object() -> VerifiedArtifact {
    verified_with_stack(crate::test_support::language_runtime_vector(), 160)
}

pub(super) fn profile() -> ExecutionProfile {
    ExecutionProfile {
        heap_bytes: u32::MAX,
        frame_storage_bytes: 1024 * 1024,
        maximum_call_depth: 64,
        maximum_coroutines: 64,
        maximum_host_requests: 64,
        maximum_events: 64,
        maximum_slice_budget: u32::MAX,
        compiler_abi: [0; 32],
        standard_library_abi: [0; 32],
        capability_mask: u32::MAX,
        host_references: Box::new([]),
    }
}

pub(super) fn profiles_below_each_manifest_limit() -> Vec<ExecutionProfile> {
    let mut profiles = Vec::new();
    let mut compiler = profile();
    compiler.compiler_abi = [1; 32];
    profiles.push(compiler);
    let mut standard_library = profile();
    standard_library.standard_library_abi = [1; 32];
    profiles.push(standard_library);
    let mut frame_storage = profile();
    frame_storage.frame_storage_bytes = 31;
    profiles.push(frame_storage);
    let mut call_depth = profile();
    call_depth.maximum_call_depth = 0;
    profiles.push(call_depth);
    let mut coroutines = profile();
    coroutines.maximum_coroutines = 0;
    profiles.push(coroutines);
    let mut slice = profile();
    slice.maximum_slice_budget = 0;
    profiles.push(slice);
    profiles
}

fn verified_with_stack(mut bytes: Vec<u8>, required_stack_bytes: u32) -> VerifiedArtifact {
    let manifest = section_offset(&bytes, crate::artifact::format::MANIFEST, 0);
    bytes[manifest + 4..manifest + 8].copy_from_slice(&required_stack_bytes.to_le_bytes());
    crate::test_support::rehash(&mut bytes);
    verify_artifact(Arc::from(bytes), ArtifactLimits::default()).unwrap()
}

fn section_offset(bytes: &[u8], kind: u16, scope: u32) -> usize {
    let count = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
    for index in 0..count {
        let entry = crate::artifact::format::HEADER_SIZE
            + index * crate::artifact::format::DIRECTORY_ENTRY_SIZE;
        let entry_kind = u16::from_le_bytes(bytes[entry..entry + 2].try_into().unwrap());
        let entry_scope = u32::from_le_bytes(bytes[entry + 4..entry + 8].try_into().unwrap());
        if entry_kind == kind && entry_scope == scope {
            return u64::from_le_bytes(bytes[entry + 8..entry + 16].try_into().unwrap()) as usize;
        }
    }
    panic!("fixture section must exist")
}

#[allow(dead_code)]
pub(super) fn reference_type(module: u32, ty: u32) -> TypeKey {
    TypeKey { module, ty }
}
