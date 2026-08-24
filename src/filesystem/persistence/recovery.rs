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

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{Checkpoint, CheckpointNode, ComputerId, JournalOperation, JournalRecord};
use crate::filesystem::{FileSystemLimits, VirtualPath};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryError {
    ConfirmedCorruption,
    LimitExceeded,
}

#[derive(Clone, Debug)]
pub struct RecoveryCheckpoint {
    claimed_generation: u64,
    bytes: Arc<[u8]>,
}

impl RecoveryCheckpoint {
    pub fn new(claimed_generation: u64, bytes: Arc<[u8]>) -> Self {
        Self {
            claimed_generation,
            bytes,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RecoveryJournalRecord {
    claimed_sequence: u64,
    bytes: Arc<[u8]>,
}

impl RecoveryJournalRecord {
    pub fn new(claimed_sequence: u64, bytes: Arc<[u8]>) -> Self {
        Self {
            claimed_sequence,
            bytes,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RecoveryInput {
    computer_id: ComputerId,
    confirmed_generation: u64,
    checkpoints: Vec<RecoveryCheckpoint>,
    journal_records: Vec<RecoveryJournalRecord>,
}

impl RecoveryInput {
    pub fn new(computer_id: ComputerId, confirmed_generation: u64) -> Self {
        Self {
            computer_id,
            confirmed_generation,
            checkpoints: Vec::new(),
            journal_records: Vec::new(),
        }
    }

    pub fn with_checkpoint(mut self, checkpoint: RecoveryCheckpoint) -> Self {
        self.checkpoints.push(checkpoint);
        self
    }

    pub fn with_journal(mut self, record: RecoveryJournalRecord) -> Self {
        self.journal_records.push(record);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredState {
    generation: u64,
    nodes: BTreeMap<VirtualPath, CheckpointNode>,
}

impl RecoveredState {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn node(&self, path: &VirtualPath) -> Option<&CheckpointNode> {
        self.nodes.get(path)
    }

    pub fn nodes(&self) -> impl ExactSizeIterator<Item = &CheckpointNode> {
        self.nodes.values()
    }
}

pub fn recover(
    input: &RecoveryInput,
    limits: &FileSystemLimits,
) -> Result<RecoveredState, RecoveryError> {
    bound_work(input, limits)?;

    let mut checkpoints: Vec<&RecoveryCheckpoint> = input
        .checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.claimed_generation <= input.confirmed_generation)
        .collect();
    checkpoints.sort_by_key(|checkpoint| checkpoint.claimed_generation);
    if checkpoints
        .windows(2)
        .any(|pair| pair[0].claimed_generation == pair[1].claimed_generation)
    {
        return Err(RecoveryError::ConfirmedCorruption);
    }

    let mut state = if let Some(candidate) = checkpoints.last() {
        let checkpoint = Checkpoint::decode(Arc::clone(&candidate.bytes), limits)
            .map_err(|_| RecoveryError::ConfirmedCorruption)?;
        if checkpoint.computer_id() != input.computer_id
            || checkpoint.generation() != candidate.claimed_generation
        {
            return Err(RecoveryError::ConfirmedCorruption);
        }
        RecoveredState {
            generation: checkpoint.generation(),
            nodes: checkpoint
                .nodes()
                .iter()
                .cloned()
                .map(|node| (node.path().clone(), node))
                .collect(),
        }
    } else {
        RecoveredState {
            generation: 0,
            nodes: BTreeMap::new(),
        }
    };
    validate_state(&state, limits).map_err(|_| RecoveryError::ConfirmedCorruption)?;

    let mut records: Vec<&RecoveryJournalRecord> = input.journal_records.iter().collect();
    records.sort_by_key(|record| record.claimed_sequence);
    if let Some(duplicate) = records
        .windows(2)
        .find(|pair| pair[0].claimed_sequence == pair[1].claimed_sequence)
        .map(|pair| pair[0].claimed_sequence)
    {
        if duplicate <= input.confirmed_generation {
            return Err(RecoveryError::ConfirmedCorruption);
        }
        records.retain(|record| record.claimed_sequence < duplicate);
    }

    for stored in records {
        if stored.claimed_sequence <= state.generation {
            continue;
        }
        let expected = state
            .generation
            .checked_add(1)
            .ok_or(RecoveryError::ConfirmedCorruption)?;
        if stored.claimed_sequence != expected {
            if expected <= input.confirmed_generation {
                return Err(RecoveryError::ConfirmedCorruption);
            }
            break;
        }
        let decoded = JournalRecord::decode(Arc::clone(&stored.bytes), limits);
        let record = match decoded {
            Ok(record)
                if record.computer_id() == input.computer_id
                    && record.sequence() == stored.claimed_sequence
                    && record.previous_sequence() == state.generation =>
            {
                record
            }
            _ if stored.claimed_sequence <= input.confirmed_generation => {
                return Err(RecoveryError::ConfirmedCorruption);
            }
            _ => break,
        };

        let mut next = state.clone();
        if apply(&mut next, record.operation(), record.sequence(), limits).is_err() {
            if stored.claimed_sequence <= input.confirmed_generation {
                return Err(RecoveryError::ConfirmedCorruption);
            }
            break;
        }
        next.generation = record.sequence();
        state = next;
    }

    if state.generation < input.confirmed_generation {
        return Err(RecoveryError::ConfirmedCorruption);
    }
    Ok(state)
}

fn bound_work(input: &RecoveryInput, limits: &FileSystemLimits) -> Result<(), RecoveryError> {
    let records = input
        .checkpoints
        .len()
        .checked_add(input.journal_records.len())
        .ok_or(RecoveryError::LimitExceeded)?;
    if records > limits.maximum_recovery_records {
        return Err(RecoveryError::LimitExceeded);
    }
    let bytes = input
        .checkpoints
        .iter()
        .map(|checkpoint| checkpoint.bytes.len())
        .chain(
            input
                .journal_records
                .iter()
                .map(|record| record.bytes.len()),
        )
        .try_fold(0_usize, |total, length| total.checked_add(length))
        .ok_or(RecoveryError::LimitExceeded)?;
    if bytes > limits.maximum_recovery_bytes {
        return Err(RecoveryError::LimitExceeded);
    }
    Ok(())
}

fn apply(
    state: &mut RecoveredState,
    operation: &JournalOperation,
    generation: u64,
    limits: &FileSystemLimits,
) -> Result<(), ()> {
    match operation {
        JournalOperation::CreateDirectory {
            path,
            node_generation,
        } if *node_generation == generation => {
            require_parent(state, path)?;
            if state.nodes.contains_key(path) {
                return Err(());
            }
            state.nodes.insert(
                path.clone(),
                CheckpointNode::directory(path.clone(), generation),
            );
        }
        JournalOperation::PutFile {
            path,
            node_generation,
            logical_size,
            object_id,
            executable,
        } if *node_generation == generation => {
            require_parent(state, path)?;
            if state
                .nodes
                .get(path)
                .is_some_and(CheckpointNode::is_directory)
            {
                return Err(());
            }
            state.nodes.insert(
                path.clone(),
                CheckpointNode::file(
                    path.clone(),
                    generation,
                    *logical_size,
                    *object_id,
                    *executable,
                ),
            );
        }
        JournalOperation::Remove { path } => {
            let node = state.nodes.get(path).ok_or(())?;
            if node.is_directory()
                && state
                    .nodes
                    .keys()
                    .any(|candidate| candidate != path && candidate.is_within(path))
            {
                return Err(());
            }
            state.nodes.remove(path);
        }
        JournalOperation::Rename {
            source,
            destination,
            replace,
        } => rename(state, source, destination, *replace, generation, limits)?,
        _ => return Err(()),
    }
    validate_state(state, limits)
}

fn rename(
    state: &mut RecoveredState,
    source: &VirtualPath,
    destination: &VirtualPath,
    replace: bool,
    generation: u64,
    limits: &FileSystemLimits,
) -> Result<(), ()> {
    if destination.is_within(source) {
        return Err(());
    }
    require_parent(state, destination)?;
    let source_node = state.nodes.get(source).ok_or(())?.clone();
    if let Some(existing) = state.nodes.get(destination) {
        if !replace || existing.is_directory() != source_node.is_directory() {
            return Err(());
        }
        if existing.is_directory()
            && state
                .nodes
                .keys()
                .any(|candidate| candidate != destination && candidate.is_within(destination))
        {
            return Err(());
        }
    }

    let moving: Vec<(VirtualPath, CheckpointNode)> = state
        .nodes
        .range(source.clone()..)
        .take_while(|(path, _)| path.is_within(source))
        .map(|(path, node)| (path.clone(), node.clone()))
        .collect();
    if moving.is_empty() {
        return Err(());
    }
    state.nodes.remove(destination);
    for (path, _) in &moving {
        state.nodes.remove(path);
    }
    for (path, node) in moving {
        let rebased = rebase(&path, source, destination, limits)?;
        let node_generation = if path == *source {
            generation
        } else {
            generation_of(&node)
        };
        state
            .nodes
            .insert(rebased.clone(), node.with_path(rebased, node_generation));
    }
    Ok(())
}

fn rebase(
    path: &VirtualPath,
    source: &VirtualPath,
    destination: &VirtualPath,
    limits: &FileSystemLimits,
) -> Result<VirtualPath, ()> {
    let source_components = source.components().count();
    let mut text = destination.to_string();
    for component in path.components().skip(source_components) {
        text.push('/');
        text.push_str(component);
    }
    VirtualPath::parse_utf8(&text, limits).map_err(|_| ())
}

fn require_parent(state: &RecoveredState, path: &VirtualPath) -> Result<(), ()> {
    let parent = path.parent().ok_or(())?;
    if parent.to_string() == "/home" {
        return Ok(());
    }
    state
        .nodes
        .get(&parent)
        .filter(|node| node.is_directory())
        .map(|_| ())
        .ok_or(())
}

fn generation_of(node: &CheckpointNode) -> u64 {
    match node {
        CheckpointNode::Directory {
            node_generation, ..
        }
        | CheckpointNode::File {
            node_generation, ..
        } => *node_generation,
    }
}

fn validate_state(state: &RecoveredState, limits: &FileSystemLimits) -> Result<(), ()> {
    if state.nodes.len() > limits.maximum_nodes.saturating_sub(2) as usize {
        return Err(());
    }
    let logical_bytes = state
        .nodes
        .values()
        .try_fold(0_u64, |total, node| match node {
            CheckpointNode::Directory { .. } => Some(total),
            CheckpointNode::File { logical_size, .. } => total.checked_add(*logical_size),
        })
        .ok_or(())?;
    if logical_bytes > limits.maximum_logical_bytes {
        return Err(());
    }
    let mut child_counts: BTreeMap<VirtualPath, usize> = BTreeMap::new();
    for path in state.nodes.keys() {
        let parent = path.parent().ok_or(())?;
        let count = child_counts.entry(parent).or_default();
        *count += 1;
        if *count > limits.maximum_directory_entries as usize {
            return Err(());
        }
    }
    Ok(())
}
