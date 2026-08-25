pub(crate) mod cfg;
pub(crate) mod exceptions;
pub(crate) mod functions;
pub(crate) mod modules;

use std::sync::Arc;

use crate::{
    artifact::VerifiedArtifact, decode::records::decode_artifact, diagnostic::DiagnosticSet,
    limits::ArtifactLimits,
};

pub(crate) fn verify_artifact(
    bytes: Arc<[u8]>,
    limits: ArtifactLimits,
) -> Result<VerifiedArtifact, DiagnosticSet> {
    let decoded = decode_artifact(bytes, &limits)?;
    modules::verify_modules(&decoded, &limits)?;
    let exception_model = exceptions::verify_exceptions(&decoded, &limits)?;
    functions::verify_functions(&decoded, &exception_model, &limits)?;
    functions::verify_entry_arguments(&decoded, &limits)?;
    exceptions::verify_semantic_features(&decoded, &limits)?;
    Ok(VerifiedArtifact::new(decoded))
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
