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

mod checkpoint;
mod codec;
mod journal;
mod recovery;

pub use checkpoint::{Checkpoint, CheckpointNode};
pub use journal::{JournalOperation, JournalRecord};
pub use recovery::{
    recover, RecoveredState, RecoveryCheckpoint, RecoveryError, RecoveryInput,
    RecoveryJournalRecord,
};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ComputerId([u8; 16]);

impl ComputerId {
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub const fn into_bytes(self) -> [u8; 16] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistenceCodecError {
    Malformed,
    UnsupportedVersion,
    LimitExceeded,
    DigestMismatch,
    NonCanonical,
}
