/*
 * The Compukters Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
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
pub mod diagnostic;
mod execution;
pub mod filesystem;
pub mod limits;
pub mod terminal;

#[cfg(test)]
mod test_encode;
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
#[cfg_attr(not(test), allow(dead_code))]
mod verify;

pub use artifact::{EntryPoint, VerifiedArtifact};
pub use computer::{
    ComputerAdvanceOutcome, ComputerError, ComputerHostRequest, ComputerMachine,
    ComputerStartError, ComputerTerminalEventKind, ComputerValue,
};
pub use diagnostic::{Code, Diagnostic, DiagnosticSet, Family, Location};
pub use execution::{
    AccountingSnapshot, AdmissionError, AdvanceOutcome, CapabilityBinding, EntryValue,
    ExecutionProfile, GuestTrap, HostArguments, HostFailure, HostFailureKind, HostRequestView,
    HostResponse, HostValueInput, HostValueType, HostValueView, ManagedAllocationFailure,
    OperationSchema, QuotaExhaustion, QuotaKind, RequestId, ResumeError, RunError, Session,
    VmFault,
};
pub use filesystem::{
    ComputerFileSystem, FileCapability, FileHandle, FileRights, FileSystemError, FileSystemLimits,
    FileSystemSnapshot, HandleTable, NodeKind, NodeMetadata, OpenFile, OpenMode, RomImage,
    RomImageError, StoreHealth, VirtualPath,
};
pub use limits::ArtifactLimits;
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
