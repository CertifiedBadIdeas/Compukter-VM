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

mod capability;
mod error;
mod handle;
mod limits;
mod path;
mod persistence;
mod quota;
mod rom;
mod store;
mod tree;
mod worker;

pub use capability::{FileCapability, FileRights};
pub use error::{FileSystemError, StoreHealth};
pub use handle::{FileHandle, HandleTable, OpenFile, OpenMode};
pub use limits::FileSystemLimits;
pub use path::VirtualPath;
pub use persistence::{
    recover, Checkpoint, CheckpointNode, ComputerId, JournalOperation, JournalRecord,
    PersistenceCodecError, RecoveredState, RecoveryCheckpoint, RecoveryError, RecoveryInput,
    RecoveryJournalRecord,
};
pub use rom::{RomImage, RomImageError};
pub use store::{StoreError, StoreOpenError, WorldFileSystemStore};
pub use tree::{
    ComputerFileSystem, ExecutableRevision, FileSystemSnapshot, NodeKind, NodeMetadata,
};
