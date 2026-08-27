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

use std::sync::Arc;

use crate::{AdmissionError, DiagnosticSet, ExecutionProfile, FileSystemError, VerifiedArtifact};

#[derive(Debug)]
pub struct DeploymentCandidate {
    pub(crate) machine: Arc<()>,
    pub(crate) profile: ExecutionProfile,
    pub(crate) _artifact: VerifiedArtifact,
    pub(crate) bytes: Arc<[u8]>,
}

impl DeploymentCandidate {
    pub(crate) fn new(
        machine: Arc<()>,
        profile: ExecutionProfile,
        artifact: VerifiedArtifact,
        bytes: Arc<[u8]>,
    ) -> Self {
        Self {
            machine,
            profile,
            _artifact: artifact,
            bytes,
        }
    }
}

#[derive(Debug)]
pub enum HostVerifyError {
    Artifact(DiagnosticSet),
    Admission(AdmissionError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostDeployError {
    WrongMachine,
    ProfileChanged,
    FileSystem(FileSystemError),
}

#[derive(Debug)]
pub struct DeploymentFailure {
    error: HostDeployError,
    candidate: DeploymentCandidate,
}

impl DeploymentFailure {
    pub(crate) const fn new(error: HostDeployError, candidate: DeploymentCandidate) -> Self {
        Self { error, candidate }
    }

    pub const fn error(&self) -> HostDeployError {
        self.error
    }

    pub fn into_candidate(self) -> DeploymentCandidate {
        self.candidate
    }
}
