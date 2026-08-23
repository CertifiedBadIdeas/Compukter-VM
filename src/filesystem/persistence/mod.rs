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
