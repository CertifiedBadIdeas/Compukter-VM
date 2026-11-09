pub(crate) mod cfg;
pub(crate) mod exceptions;
pub(crate) mod functions;
pub(crate) mod modules;

use std::sync::Arc;

use crate::{
    artifact::{DecodedArtifact, VerifiedArtifact},
    decode::records::decode_artifact,
    diagnostic::DiagnosticSet,
    limits::ArtifactLimits,
};

pub(crate) fn verify_artifact(
    bytes: Arc<[u8]>,
    limits: ArtifactLimits,
) -> Result<VerifiedArtifact, DiagnosticSet> {
    verify_decoded(bytes, limits).map(VerifiedArtifact::new)
}

/// Benchmark control only: same verification and admission without identity reuse.
#[cfg(all(feature = "program-sharing-benchmark", not(test)))]
pub(crate) fn verify_artifact_unshared(
    bytes: Arc<[u8]>,
    limits: ArtifactLimits,
) -> Result<VerifiedArtifact, DiagnosticSet> {
    verify_decoded(bytes, limits).map(VerifiedArtifact::new_unshared)
}

fn verify_decoded(
    bytes: Arc<[u8]>,
    limits: ArtifactLimits,
) -> Result<DecodedArtifact, DiagnosticSet> {
    let decoded = decode_artifact(bytes, &limits)?;
    modules::verify_modules(&decoded, &limits)?;
    let exception_model = exceptions::verify_exceptions(&decoded, &limits)?;
    functions::verify_functions(&decoded, &exception_model, &limits)?;
    functions::verify_entry_arguments(&decoded, &limits)?;
    exceptions::verify_semantic_features(&decoded, &limits)?;
    Ok(decoded)
}

#[cfg(test)]
pub(crate) fn verify_execution_fixture(
    bytes: Arc<[u8]>,
    limits: ArtifactLimits,
) -> Result<VerifiedArtifact, DiagnosticSet> {
    let decoded = decode_artifact(bytes, &limits)?;
    modules::verify_modules(&decoded, &limits)?;
    let exception_model = exceptions::verify_exceptions(&decoded, &limits)?;
    functions::verify_functions(&decoded, &exception_model, &limits)?;
    exceptions::verify_semantic_features(&decoded, &limits)?;
    Ok(VerifiedArtifact::new(decoded))
}

#[cfg(test)]
mod tests;
