/*
 * The Compukters Developers
 *
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The standalone managed runtime for Compukter bytecode.
//!
//! Artifact loading accepts immutable bytes and caller-selected resource
//! limits. It publishes only a fully verified artifact:
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use compukter_vm::{verify_artifact, ArtifactLimits, DiagnosticSet, VerifiedArtifact};
//!
//! fn load(bytes: Arc<[u8]>) -> Result<VerifiedArtifact, DiagnosticSet> {
//!     verify_artifact(bytes, ArtifactLimits::default())
//! }
//! ```
//!
//! The artifact digest proves integrity, not publisher authenticity. Host trust
//! policy, device admission, and execution remain separate from verification.

#[cfg_attr(not(test), allow(dead_code))]
mod artifact;
#[cfg_attr(not(test), allow(dead_code))]
mod bytes;
mod computer;
#[cfg_attr(not(test), allow(dead_code))]
mod decode;
mod deployment;
pub mod diagnostic;
mod execution;
pub mod filesystem;
pub mod limits;
mod process;
mod stdio;
pub mod terminal;

#[cfg(test)]
mod test_encode;
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
#[cfg_attr(not(test), allow(dead_code))]
mod verify;

pub use artifact::{EntryArguments, EntryPoint, VerifiedArtifact};
pub use computer::{
    CompilationRequest, CompilationSource, ComputerAdvanceOutcome, ComputerError,
    ComputerHostRequest, ComputerMachine, ComputerStartError, ComputerTerminalEventKind,
    ComputerValue,
};
pub use deployment::{DeploymentCandidate, DeploymentFailure, HostDeployError, HostVerifyError};
pub use diagnostic::{Code, Diagnostic, DiagnosticSet, Family, Location};
pub use execution::{
    AccountingSnapshot, AdmissionError, AdvanceOutcome, CapabilityBinding, EntryArgumentLimit,
    EntryArgumentLimits, EntryValue, ExecutionProfile, GuestTrap, HostArguments, HostFailure,
    HostFailureKind, HostRequestView, HostResponse, HostValueInput, HostValueType, HostValueView,
    ManagedAllocationFailure, OperationSchema, QuotaExhaustion, QuotaKind, RequestId, ResumeError,
    RunError, Session, TaskId, VmFault,
};
pub use filesystem::{
    recover, Checkpoint, CheckpointNode, ComputerFileSystem, ComputerId, ExecutableRevision,
    FileCapability, FileHandle, FileRights, FileSystemError, FileSystemLimits, FileSystemSnapshot,
    HandleTable, JournalOperation, JournalRecord, NodeKind, NodeMetadata, OpenFile, OpenMode,
    PersistenceCodecError, RecoveredState, RecoveryCheckpoint, RecoveryError, RecoveryInput,
    RecoveryJournalRecord, RomImage, RomImageError, StoreError, StoreHealth, StoreOpenError,
    VirtualPath, WorldFileSystemStore,
};
pub use limits::ArtifactLimits;
pub use process::{
    ProcessArgumentLimits, ProcessCompletion, ProcessContractError, ProcessFailureReason,
    ProcessLimits,
};
pub use stdio::{CanonicalLineSubmissionError, InputOwnershipError};
pub use terminal::{
    TerminalCell, TerminalChange, TerminalCommit, TerminalConfig, TerminalDelta, TerminalDevice,
    TerminalError, TerminalInputError, TerminalInputEvent, TerminalInputLimits, TerminalKey,
    TerminalKeyAction, TerminalKeyEvent, TerminalModifiers, TerminalPosition, TerminalRectangle,
    TerminalSnapshot, TerminalUpdate, TERMINAL_HEIGHT, TERMINAL_PALETTE_SIZE, TERMINAL_WIDTH,
};

/// Decodes and verifies an untrusted Compukter artifact before publishing it.
///
/// `limits` bounds parsing and verification work. Successful verification does
/// not authenticate, admit, reserve resources for, or execute the artifact.
pub fn verify_artifact(
    bytes: std::sync::Arc<[u8]>,
    limits: ArtifactLimits,
) -> Result<VerifiedArtifact, DiagnosticSet> {
    verify::verify_artifact(bytes, limits)
}
