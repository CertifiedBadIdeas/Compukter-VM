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

use super::codec::{path_record_len, root_descendant, Cursor, Writer, DIGEST_BYTES};
use super::{ComputerId, PersistenceCodecError};
use crate::filesystem::{FileSystemLimits, VirtualPath};

const MAGIC: &[u8; 8] = b"CPKTJNL\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JournalOperation {
    /// One flat, bounded transaction, published as a single journal record.
    Batch {
        operations: Vec<JournalOperation>,
    },
    CreateDirectory {
        path: VirtualPath,
        node_generation: u64,
    },
    PutFile {
        path: VirtualPath,
        node_generation: u64,
        logical_size: u64,
        object_id: [u8; 32],
        executable: bool,
    },
    Remove {
        path: VirtualPath,
    },
    Rename {
        source: VirtualPath,
        destination: VirtualPath,
        replace: bool,
    },
}

impl JournalOperation {
    pub fn create_directory(path: VirtualPath, node_generation: u64) -> Self {
        Self::CreateDirectory {
            path,
            node_generation,
        }
    }

    pub fn put_file(
        path: VirtualPath,
        node_generation: u64,
        logical_size: u64,
        object_id: [u8; 32],
        executable: bool,
    ) -> Self {
        Self::PutFile {
            path,
            node_generation,
            logical_size,
            object_id,
            executable,
        }
    }

    pub fn remove(path: VirtualPath) -> Self {
        Self::Remove { path }
    }

    pub fn rename(source: VirtualPath, destination: VirtualPath, replace: bool) -> Self {
        Self::Rename {
            source,
            destination,
            replace,
        }
    }

    pub(crate) fn encode(
        &self,
        limits: &FileSystemLimits,
    ) -> Result<Vec<u8>, PersistenceCodecError> {
        let length = self.encoded_len(limits)?;
        if length > limits.maximum_journal_payload_bytes {
            return Err(PersistenceCodecError::LimitExceeded);
        }
        let mut writer = Writer::with_capacity(length)?;
        match self {
            Self::Batch { operations } => {
                writer.u8(5);
                writer.u8(0);
                writer.u16(0);
                writer.u32(
                    u32::try_from(operations.len())
                        .map_err(|_| PersistenceCodecError::LimitExceeded)?,
                );
                for operation in operations {
                    let bytes = operation.encode(limits)?;
                    writer.u32(bytes.len() as u32);
                    writer.bytes(&bytes);
                }
            }
            Self::CreateDirectory {
                path,
                node_generation,
            } => {
                writer.u8(1);
                writer.u8(0);
                writer.u16(0);
                writer.path(path)?;
                writer.u64(*node_generation);
            }
            Self::PutFile {
                path,
                node_generation,
                logical_size,
                object_id,
                executable,
            } => {
                writer.u8(2);
                writer.u8(u8::from(*executable));
                writer.u16(0);
                writer.path(path)?;
                writer.u64(*node_generation);
                writer.u64(*logical_size);
                writer.bytes(object_id);
            }
            Self::Remove { path } => {
                writer.u8(3);
                writer.u8(0);
                writer.u16(0);
                writer.path(path)?;
            }
            Self::Rename {
                source,
                destination,
                replace,
            } => {
                writer.u8(4);
                writer.u8(u8::from(*replace));
                writer.u16(0);
                writer.path(source)?;
                writer.path(destination)?;
            }
        }
        Ok(writer.into_bytes())
    }

    fn encoded_len(&self, limits: &FileSystemLimits) -> Result<usize, PersistenceCodecError> {
        if let Self::Batch { operations } = self {
            if operations.is_empty()
                || operations.len() > limits.maximum_nodes as usize
                || operations.iter().any(|op| matches!(op, Self::Batch { .. }))
            {
                return Err(PersistenceCodecError::NonCanonical);
            }
            return operations.iter().try_fold(8_usize, |length, op| {
                length
                    .checked_add(4)
                    .and_then(|length| length.checked_add(op.encoded_len(limits).ok()?))
                    .ok_or(PersistenceCodecError::LimitExceeded)
            });
        }
        let mut length = 4_usize;
        let mut add_path = |path: &VirtualPath| -> Result<(), PersistenceCodecError> {
            if path.encoded_len() > limits.maximum_path_bytes {
                return Err(PersistenceCodecError::LimitExceeded);
            }
            length = length
                .checked_add(path_record_len(path)?)
                .ok_or(PersistenceCodecError::LimitExceeded)?;
            Ok(())
        };
        match self {
            Self::Batch { .. } => unreachable!(),
            Self::CreateDirectory { path, .. }
            | Self::PutFile { path, .. }
            | Self::Remove { path } => add_path(path)?,
            Self::Rename {
                source,
                destination,
                ..
            } => {
                add_path(source)?;
                add_path(destination)?;
            }
        }
        length = length
            .checked_add(match self {
                Self::Batch { .. } => unreachable!(),
                Self::CreateDirectory { .. } => 8,
                Self::PutFile { .. } => 8 + 8 + 32,
                Self::Remove { .. } | Self::Rename { .. } => 0,
            })
            .ok_or(PersistenceCodecError::LimitExceeded)?;
        Ok(length)
    }

    fn decode(bytes: &[u8], limits: &FileSystemLimits) -> Result<Self, PersistenceCodecError> {
        let mut cursor = Cursor::plain(bytes);
        let operation = cursor.u8()?;
        let flags = cursor.u8()?;
        if cursor.u16()? != 0 {
            return Err(PersistenceCodecError::Malformed);
        }
        let result = match operation {
            5 if flags == 0 => {
                let count = cursor.u32()? as usize;
                if count == 0
                    || count > limits.maximum_nodes as usize
                    || count > cursor.remaining() / 8
                {
                    return Err(PersistenceCodecError::LimitExceeded);
                }
                let mut operations = Vec::new();
                operations
                    .try_reserve_exact(count)
                    .map_err(|_| PersistenceCodecError::LimitExceeded)?;
                for _ in 0..count {
                    let length = cursor.u32()? as usize;
                    let bytes = cursor.exact(length)?;
                    if bytes.first() == Some(&5) {
                        return Err(PersistenceCodecError::NonCanonical);
                    }
                    operations.push(Self::decode(bytes, limits)?);
                }
                Self::Batch { operations }
            }
            1 if flags == 0 => Self::create_directory(cursor.path(limits)?, cursor.u64()?),
            2 if flags & !1 == 0 => {
                let path = cursor.path(limits)?;
                let node_generation = cursor.u64()?;
                let logical_size = cursor.u64()?;
                if logical_size > limits.maximum_file_bytes {
                    return Err(PersistenceCodecError::LimitExceeded);
                }
                let object_id = cursor.exact(32)?.try_into().expect("exact width");
                Self::put_file(
                    path,
                    node_generation,
                    logical_size,
                    object_id,
                    flags & 1 != 0,
                )
            }
            3 if flags == 0 => Self::remove(cursor.path(limits)?),
            4 if flags & !1 == 0 => {
                Self::rename(cursor.path(limits)?, cursor.path(limits)?, flags & 1 != 0)
            }
            _ => return Err(PersistenceCodecError::Malformed),
        };
        cursor.finish()?;
        if !result.paths_are_root_descendants() {
            return Err(PersistenceCodecError::NonCanonical);
        }
        Ok(result)
    }

    fn paths_are_root_descendants(&self) -> bool {
        match self {
            Self::Batch { operations } => {
                !operations.is_empty()
                    && operations.iter().all(|op| {
                        !matches!(op, Self::Batch { .. }) && op.paths_are_root_descendants()
                    })
            }
            Self::CreateDirectory { path, .. }
            | Self::PutFile { path, .. }
            | Self::Remove { path } => root_descendant(path),
            Self::Rename {
                source,
                destination,
                ..
            } => root_descendant(source) && root_descendant(destination) && source != destination,
        }
    }

    fn generation_is_canonical(&self, sequence: u64) -> bool {
        match self {
            Self::Batch { operations } => operations
                .iter()
                .all(|op| op.generation_is_canonical(sequence)),
            Self::CreateDirectory {
                node_generation, ..
            }
            | Self::PutFile {
                node_generation, ..
            } => *node_generation == sequence,
            Self::Remove { .. } | Self::Rename { .. } => true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalRecord {
    computer_id: ComputerId,
    sequence: u64,
    previous_sequence: u64,
    operation: JournalOperation,
}

impl JournalRecord {
    pub fn new(
        computer_id: ComputerId,
        sequence: u64,
        previous_sequence: u64,
        operation: JournalOperation,
    ) -> Result<Self, PersistenceCodecError> {
        if sequence == 0
            || previous_sequence.checked_add(1) != Some(sequence)
            || !operation.paths_are_root_descendants()
            || !operation.generation_is_canonical(sequence)
        {
            return Err(PersistenceCodecError::NonCanonical);
        }
        Ok(Self {
            computer_id,
            sequence,
            previous_sequence,
            operation,
        })
    }

    pub fn encode(&self, limits: &FileSystemLimits) -> Result<Arc<[u8]>, PersistenceCodecError> {
        let payload = self.operation.encode(limits)?;
        let encoded_without_digest = 48_usize
            .checked_add(payload.len())
            .ok_or(PersistenceCodecError::LimitExceeded)?;
        let total = encoded_without_digest
            .checked_add(DIGEST_BYTES)
            .ok_or(PersistenceCodecError::LimitExceeded)?;
        if total > limits.maximum_journal_record_bytes {
            return Err(PersistenceCodecError::LimitExceeded);
        }
        let mut writer = Writer::with_capacity(encoded_without_digest)?;
        writer.bytes(MAGIC);
        writer.u16(2);
        writer.u16(0);
        writer.bytes(&self.computer_id.into_bytes());
        writer.u64(self.sequence);
        writer.u64(self.previous_sequence);
        writer.u32(u32::try_from(payload.len()).map_err(|_| PersistenceCodecError::LimitExceeded)?);
        writer.bytes(&payload);
        writer.finish_checked(limits.maximum_journal_record_bytes)
    }

    pub fn decode(
        bytes: Arc<[u8]>,
        limits: &FileSystemLimits,
    ) -> Result<Self, PersistenceCodecError> {
        let mut cursor = Cursor::verified(&bytes, limits.maximum_journal_record_bytes)?;
        if cursor.exact(8)? != MAGIC {
            return Err(PersistenceCodecError::Malformed);
        }
        if cursor.u16()? != 2 {
            return Err(PersistenceCodecError::UnsupportedVersion);
        }
        if cursor.u16()? != 0 {
            return Err(PersistenceCodecError::Malformed);
        }
        let computer_id =
            ComputerId::from_bytes(cursor.exact(16)?.try_into().expect("exact width"));
        let sequence = cursor.u64()?;
        let previous_sequence = cursor.u64()?;
        let payload_length = cursor.u32()? as usize;
        if payload_length > limits.maximum_journal_payload_bytes
            || payload_length != cursor.remaining()
        {
            return Err(if payload_length > limits.maximum_journal_payload_bytes {
                PersistenceCodecError::LimitExceeded
            } else {
                PersistenceCodecError::Malformed
            });
        }
        let operation = JournalOperation::decode(cursor.exact(payload_length)?, limits)?;
        cursor.finish()?;
        Self::new(computer_id, sequence, previous_sequence, operation)
    }

    pub fn computer_id(&self) -> ComputerId {
        self.computer_id
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn previous_sequence(&self) -> u64 {
        self.previous_sequence
    }

    pub fn operation(&self) -> &JournalOperation {
        &self.operation
    }
}
