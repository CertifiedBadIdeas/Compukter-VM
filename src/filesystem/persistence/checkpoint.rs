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

use std::collections::BTreeSet;
use std::sync::Arc;

use super::codec::{home_descendant, path_record_len, Cursor, Writer, DIGEST_BYTES};
use super::{ComputerId, PersistenceCodecError};
use crate::filesystem::{FileSystemLimits, VirtualPath};

const MAGIC: &[u8; 8] = b"CPKTCHK\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckpointNode {
    Directory {
        path: VirtualPath,
        node_generation: u64,
    },
    File {
        path: VirtualPath,
        node_generation: u64,
        logical_size: u64,
        object_id: [u8; 32],
        executable: bool,
    },
}

impl CheckpointNode {
    pub fn directory(path: VirtualPath, node_generation: u64) -> Self {
        Self::Directory {
            path,
            node_generation,
        }
    }

    pub fn file(
        path: VirtualPath,
        node_generation: u64,
        logical_size: u64,
        object_id: [u8; 32],
        executable: bool,
    ) -> Self {
        Self::File {
            path,
            node_generation,
            logical_size,
            object_id,
            executable,
        }
    }

    pub fn path(&self) -> &VirtualPath {
        match self {
            Self::Directory { path, .. } | Self::File { path, .. } => path,
        }
    }

    pub fn executable(&self) -> bool {
        matches!(
            self,
            Self::File {
                executable: true,
                ..
            }
        )
    }

    pub(crate) fn is_directory(&self) -> bool {
        matches!(self, Self::Directory { .. })
    }

    pub(crate) fn object(&self) -> Option<([u8; 32], u64)> {
        match self {
            Self::File {
                object_id,
                logical_size,
                ..
            } => Some((*object_id, *logical_size)),
            Self::Directory { .. } => None,
        }
    }

    pub(crate) fn with_path(&self, path: VirtualPath, generation: u64) -> Self {
        match self {
            Self::Directory { .. } => Self::directory(path, generation),
            Self::File {
                logical_size,
                object_id,
                executable,
                ..
            } => Self::file(path, generation, *logical_size, *object_id, *executable),
        }
    }

    fn encode(&self, writer: &mut Writer) -> Result<(), PersistenceCodecError> {
        writer.path(self.path())?;
        match self {
            Self::Directory {
                node_generation, ..
            } => {
                writer.u8(1);
                writer.u8(0);
                writer.u16(0);
                writer.u64(*node_generation);
                writer.u64(0);
                writer.bytes(&[0; 32]);
            }
            Self::File {
                node_generation,
                logical_size,
                object_id,
                executable,
                ..
            } => {
                writer.u8(2);
                writer.u8(u8::from(*executable));
                writer.u16(0);
                writer.u64(*node_generation);
                writer.u64(*logical_size);
                writer.bytes(object_id);
            }
        }
        Ok(())
    }

    fn decode(
        cursor: &mut Cursor<'_>,
        limits: &FileSystemLimits,
    ) -> Result<Self, PersistenceCodecError> {
        let path = cursor.path(limits)?;
        let kind = cursor.u8()?;
        let flags = cursor.u8()?;
        if cursor.u16()? != 0 {
            return Err(PersistenceCodecError::Malformed);
        }
        let node_generation = cursor.u64()?;
        let logical_size = cursor.u64()?;
        let object_id: [u8; 32] = cursor.exact(32)?.try_into().expect("exact width");
        match kind {
            1 if flags == 0 && logical_size == 0 && object_id == [0; 32] => {
                Ok(Self::directory(path, node_generation))
            }
            2 if flags & !1 == 0 && logical_size <= limits.maximum_file_bytes => Ok(Self::file(
                path,
                node_generation,
                logical_size,
                object_id,
                flags & 1 != 0,
            )),
            2 if logical_size > limits.maximum_file_bytes => {
                Err(PersistenceCodecError::LimitExceeded)
            }
            _ => Err(PersistenceCodecError::Malformed),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checkpoint {
    computer_id: ComputerId,
    generation: u64,
    nodes: Arc<[CheckpointNode]>,
}

impl Checkpoint {
    pub fn new(
        computer_id: ComputerId,
        generation: u64,
        nodes: Vec<CheckpointNode>,
    ) -> Result<Self, PersistenceCodecError> {
        validate_nodes(&nodes, generation)?;
        Ok(Self {
            computer_id,
            generation,
            nodes: nodes.into(),
        })
    }

    pub fn encode(&self, limits: &FileSystemLimits) -> Result<Arc<[u8]>, PersistenceCodecError> {
        if self.nodes.len() > limits.maximum_nodes.saturating_sub(2) as usize {
            return Err(PersistenceCodecError::LimitExceeded);
        }
        let mut encoded_without_digest = 40_usize;
        for node in self.nodes.iter() {
            if node.path().encoded_len() > limits.maximum_path_bytes {
                return Err(PersistenceCodecError::LimitExceeded);
            }
            if matches!(node, CheckpointNode::File { logical_size, .. } if *logical_size > limits.maximum_file_bytes)
            {
                return Err(PersistenceCodecError::LimitExceeded);
            }
            encoded_without_digest = encoded_without_digest
                .checked_add(path_record_len(node.path())?)
                .and_then(|length| length.checked_add(52))
                .ok_or(PersistenceCodecError::LimitExceeded)?;
        }
        let total = encoded_without_digest
            .checked_add(DIGEST_BYTES)
            .ok_or(PersistenceCodecError::LimitExceeded)?;
        if total > limits.maximum_checkpoint_bytes {
            return Err(PersistenceCodecError::LimitExceeded);
        }
        let mut writer = Writer::with_capacity(encoded_without_digest)?;
        writer.bytes(MAGIC);
        writer.u16(1);
        writer.u16(0);
        writer.bytes(&self.computer_id.into_bytes());
        writer.u64(self.generation);
        writer.u32(
            u32::try_from(self.nodes.len()).map_err(|_| PersistenceCodecError::LimitExceeded)?,
        );
        for node in self.nodes.iter() {
            node.encode(&mut writer)?;
        }
        writer.finish_checked(limits.maximum_checkpoint_bytes)
    }

    pub fn decode(
        bytes: Arc<[u8]>,
        limits: &FileSystemLimits,
    ) -> Result<Self, PersistenceCodecError> {
        let mut cursor = Cursor::verified(&bytes, limits.maximum_checkpoint_bytes)?;
        if cursor.exact(8)? != MAGIC {
            return Err(PersistenceCodecError::Malformed);
        }
        if cursor.u16()? != 1 {
            return Err(PersistenceCodecError::UnsupportedVersion);
        }
        if cursor.u16()? != 0 {
            return Err(PersistenceCodecError::Malformed);
        }
        let computer_id =
            ComputerId::from_bytes(cursor.exact(16)?.try_into().expect("exact width"));
        let generation = cursor.u64()?;
        let count = cursor.u32()?;
        if count > limits.maximum_nodes.saturating_sub(2) {
            return Err(PersistenceCodecError::LimitExceeded);
        }
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(count as usize)
            .map_err(|_| PersistenceCodecError::LimitExceeded)?;
        for _ in 0..count {
            nodes.push(CheckpointNode::decode(&mut cursor, limits)?);
        }
        cursor.finish()?;
        Self::new(computer_id, generation, nodes)
    }

    pub fn computer_id(&self) -> ComputerId {
        self.computer_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn nodes(&self) -> &[CheckpointNode] {
        &self.nodes
    }
}

fn validate_nodes(
    nodes: &[CheckpointNode],
    checkpoint_generation: u64,
) -> Result<(), PersistenceCodecError> {
    let mut directories = BTreeSet::new();
    for (index, node) in nodes.iter().enumerate() {
        if !home_descendant(node.path())
            || index > 0 && nodes[index - 1].path() >= node.path()
            || node_generation(node) > checkpoint_generation
        {
            return Err(PersistenceCodecError::NonCanonical);
        }
        let parent = node
            .path()
            .parent()
            .ok_or(PersistenceCodecError::NonCanonical)?;
        if parent.to_string() != "/home" && !directories.contains(&parent) {
            return Err(PersistenceCodecError::NonCanonical);
        }
        if node.is_directory() {
            directories.insert(node.path().clone());
        }
    }
    Ok(())
}

fn node_generation(node: &CheckpointNode) -> u64 {
    match node {
        CheckpointNode::Directory {
            node_generation, ..
        }
        | CheckpointNode::File {
            node_generation, ..
        } => *node_generation,
    }
}
