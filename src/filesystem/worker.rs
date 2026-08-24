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

use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use sha2::{Digest, Sha256};

use super::{
    ComputerId, FileSystemError, FileSystemLimits, JournalOperation, JournalRecord,
    PersistenceCodecError, StoreError, StoreHealth,
};

#[derive(Debug)]
struct QueueState {
    health: StoreHealth,
    queue: VecDeque<Command>,
    queued_bytes: usize,
    reserved_records: usize,
    reserved_bytes: usize,
    maximum_records: usize,
    maximum_bytes: usize,
    admitted: BTreeMap<ComputerId, u64>,
    durable: BTreeMap<ComputerId, u64>,
    stop_enqueued: bool,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<QueueState>,
    changed: Condvar,
}

#[derive(Clone, Debug)]
pub(crate) struct PersistenceGate {
    shared: Arc<Shared>,
}

#[derive(Clone, Debug)]
pub(crate) struct ComputerPersistence {
    computer_id: ComputerId,
    gate: PersistenceGate,
    limits: FileSystemLimits,
}

#[derive(Debug)]
struct MutationCommand {
    computer_id: ComputerId,
    generation: u64,
    object: Option<([u8; 32], Arc<[u8]>)>,
    journal: Arc<[u8]>,
}

#[derive(Debug)]
enum Command {
    Mutation(MutationCommand),
    Tombstone(TombstoneCommand),
    Collect(CollectCommand),
    Stop,
}

#[derive(Debug)]
struct TombstoneCommand {
    computer_id: ComputerId,
    present: bool,
    completion: Completion<()>,
}

#[derive(Debug)]
struct CollectCommand {
    objects: Arc<[[u8; 32]]>,
    completion: Completion<usize>,
}

type CompletionState<T> = (Mutex<Option<Result<T, StoreError>>>, Condvar);

#[derive(Clone, Debug)]
struct Completion<T>(Arc<CompletionState<T>>);

impl<T: Copy> Completion<T> {
    fn new() -> Self {
        Self(Arc::new((Mutex::new(None), Condvar::new())))
    }

    fn complete(&self, result: Result<T, StoreError>) {
        let (result_slot, changed) = &*self.0;
        *result_slot
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(result);
        changed.notify_all();
    }

    fn wait(&self) -> Result<T, StoreError> {
        let (result_slot, changed) = &*self.0;
        let mut result = result_slot
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        loop {
            if let Some(result) = *result {
                return result;
            }
            result = changed
                .wait(result)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }
}

impl Command {
    fn byte_cost(&self) -> usize {
        match self {
            Self::Mutation(command) => command
                .object
                .as_ref()
                .map_or(0, |(_, bytes)| bytes.len())
                .saturating_add(command.journal.len()),
            Self::Tombstone(_) => 0,
            Self::Collect(command) => command.objects.len().saturating_mul(32),
            Self::Stop => 0,
        }
    }

    fn fail(self) {
        match self {
            Self::Tombstone(command) => {
                command.completion.complete(Err(StoreError::StorageFaulted));
            }
            Self::Collect(command) => {
                command.completion.complete(Err(StoreError::StorageFaulted));
            }
            Self::Mutation(_) | Self::Stop => {}
        }
    }
}

pub(crate) struct PersistenceMutation {
    reservation: Option<QueueReservation>,
    command: Option<Command>,
}

impl PersistenceMutation {
    pub fn publish(mut self) {
        let reservation = self.reservation.take().expect("one reservation");
        let command = self.command.take().expect("one immutable command");
        reservation.publish(command);
    }
}

struct QueueReservation {
    gate: PersistenceGate,
    byte_cost: usize,
    active: bool,
}

impl QueueReservation {
    fn publish(mut self, command: Command) {
        debug_assert_eq!(self.byte_cost, command.byte_cost());
        let mut state = self
            .gate
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.reserved_records -= 1;
        state.reserved_bytes -= self.byte_cost;
        state.queued_bytes += self.byte_cost;
        if let Command::Mutation(command) = &command {
            state
                .admitted
                .insert(command.computer_id, command.generation);
        }
        state.queue.push_back(command);
        self.active = false;
        self.gate.shared.changed.notify_all();
    }
}

impl Drop for QueueReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .gate
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.reserved_records -= 1;
        state.reserved_bytes -= self.byte_cost;
        self.gate.shared.changed.notify_all();
    }
}

impl PersistenceGate {
    pub fn start(root: PathBuf, limits: &FileSystemLimits) -> io::Result<(Self, JoinHandle<()>)> {
        let mut queue = VecDeque::new();
        queue
            .try_reserve(
                limits
                    .maximum_persistence_queue_records
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("queue capacity"))?,
            )
            .map_err(|_| io::Error::other("queue capacity"))?;
        let gate = Self {
            shared: Arc::new(Shared {
                state: Mutex::new(QueueState {
                    health: StoreHealth::Active,
                    queue,
                    queued_bytes: 0,
                    reserved_records: 0,
                    reserved_bytes: 0,
                    maximum_records: limits.maximum_persistence_queue_records,
                    maximum_bytes: limits.maximum_persistence_queue_bytes,
                    admitted: BTreeMap::new(),
                    durable: BTreeMap::new(),
                    stop_enqueued: false,
                }),
                changed: Condvar::new(),
            }),
        };
        let worker_gate = gate.clone();
        let handle = thread::Builder::new()
            .name("compukters-vfs".into())
            .spawn(move || worker_loop(worker_gate, root))?;
        Ok((gate, handle))
    }

    pub fn health(&self) -> StoreHealth {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .health
    }

    pub fn register_computer(&self, id: ComputerId, generation: u64) -> Result<(), StoreError> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        require_active_store(state.health)?;
        state.admitted.insert(id, generation);
        state.durable.insert(id, generation);
        Ok(())
    }

    pub fn computer(&self, id: ComputerId, limits: FileSystemLimits) -> ComputerPersistence {
        ComputerPersistence {
            computer_id: id,
            gate: self.clone(),
            limits,
        }
    }

    pub fn durable_generation(&self, id: ComputerId) -> Result<u64, StoreError> {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match state.health {
            StoreHealth::Faulted => return Err(StoreError::StorageFaulted),
            StoreHealth::Closed => return Err(StoreError::Closed),
            StoreHealth::Active | StoreHealth::Draining => {}
        }
        state.durable.get(&id).copied().ok_or(StoreError::NotFound)
    }

    pub fn flush(&self, id: ComputerId, generation: u64) -> Result<(), StoreError> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.admitted.get(&id).copied().unwrap_or(0) < generation {
            return Err(StoreError::InvalidGeneration);
        }
        loop {
            match state.health {
                StoreHealth::Faulted => return Err(StoreError::StorageFaulted),
                StoreHealth::Closed => return Err(StoreError::Closed),
                StoreHealth::Active | StoreHealth::Draining => {}
            }
            if state.durable.get(&id).copied().unwrap_or(0) >= generation {
                return Ok(());
            }
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }

    pub fn tombstone(&self, id: ComputerId, present: bool) -> Result<(), StoreError> {
        let completion = Completion::<()>::new();
        let command = Command::Tombstone(TombstoneCommand {
            computer_id: id,
            present,
            completion: completion.clone(),
        });
        let reservation = self
            .reserve(command.byte_cost())
            .map_err(filesystem_to_store)?;
        reservation.publish(command);
        completion.wait()
    }

    pub fn flush_all(&self) -> Result<(), StoreError> {
        let admitted = {
            let state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state
                .admitted
                .iter()
                .map(|(id, generation)| (*id, *generation))
                .collect::<Vec<_>>()
        };
        for (id, generation) in admitted {
            self.flush(id, generation)?;
        }
        Ok(())
    }

    pub fn collect(&self, objects: Vec<[u8; 32]>) -> Result<usize, StoreError> {
        let completion = Completion::<usize>::new();
        let command = Command::Collect(CollectCommand {
            objects: objects.into(),
            completion: completion.clone(),
        });
        let reservation = self
            .reserve(command.byte_cost())
            .map_err(filesystem_to_store)?;
        reservation.publish(command);
        completion.wait()
    }

    fn reserve(&self, byte_cost: usize) -> Result<QueueReservation, FileSystemError> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        require_active_filesystem(state.health)?;
        let records = state
            .queue
            .len()
            .checked_add(state.reserved_records)
            .ok_or(FileSystemError::Busy)?;
        let bytes = state
            .queued_bytes
            .checked_add(state.reserved_bytes)
            .and_then(|total| total.checked_add(byte_cost))
            .ok_or(FileSystemError::Busy)?;
        if records >= state.maximum_records || bytes > state.maximum_bytes {
            return Err(FileSystemError::Busy);
        }
        state
            .queue
            .try_reserve(1)
            .map_err(|_| FileSystemError::Busy)?;
        state.reserved_records += 1;
        state.reserved_bytes += byte_cost;
        Ok(QueueReservation {
            gate: self.clone(),
            byte_cost,
            active: true,
        })
    }

    pub fn begin_draining(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.health == StoreHealth::Active {
            state.health = StoreHealth::Draining;
        }
        self.shared.changed.notify_all();
    }

    pub fn stop(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while state.reserved_records != 0
            && !matches!(state.health, StoreHealth::Faulted | StoreHealth::Closed)
        {
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        if !state.stop_enqueued {
            state.queue.push_back(Command::Stop);
            state.stop_enqueued = true;
        }
        self.shared.changed.notify_all();
    }

    pub fn mark_closed(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.health != StoreHealth::Faulted {
            state.health = StoreHealth::Closed;
        }
        self.shared.changed.notify_all();
    }

    pub fn fault(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.health = StoreHealth::Faulted;
        self.shared.changed.notify_all();
    }
}

impl ComputerPersistence {
    pub fn prepare(
        &self,
        generation: u64,
        object: Option<([u8; 32], Arc<[u8]>)>,
        operation: JournalOperation,
    ) -> Result<PersistenceMutation, FileSystemError> {
        let journal = JournalRecord::new(self.computer_id, generation, generation - 1, operation)
            .and_then(|record| record.encode(&self.limits))
            .map_err(codec_to_filesystem)?;
        let command = Command::Mutation(MutationCommand {
            computer_id: self.computer_id,
            generation,
            object,
            journal,
        });
        let reservation = self.gate.reserve(command.byte_cost())?;
        Ok(PersistenceMutation {
            reservation: Some(reservation),
            command: Some(command),
        })
    }
}

fn worker_loop(gate: PersistenceGate, root: PathBuf) {
    loop {
        let command = {
            let mut state = gate
                .shared
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            while state.queue.is_empty() {
                state = gate
                    .shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poison| poison.into_inner());
            }
            let command = state.queue.pop_front().expect("non-empty queue");
            state.queued_bytes -= command.byte_cost();
            gate.shared.changed.notify_all();
            command
        };
        match command {
            Command::Mutation(command) => {
                if persist_mutation(&root, &command).is_err() {
                    fault_worker(&gate);
                } else {
                    let mut state = gate
                        .shared
                        .state
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    state
                        .durable
                        .insert(command.computer_id, command.generation);
                    gate.shared.changed.notify_all();
                }
            }
            Command::Tombstone(command) => {
                let result = persist_tombstone(&root, command.computer_id, command.present)
                    .map_err(|_| StoreError::StorageFaulted);
                if result.is_err() {
                    fault_worker(&gate);
                }
                command.completion.complete(result);
            }
            Command::Collect(command) => {
                let result = collect_objects(&root, &command.objects)
                    .map_err(|_| StoreError::StorageFaulted);
                if result.is_err() {
                    fault_worker(&gate);
                }
                command.completion.complete(result);
            }
            Command::Stop => break,
        }
    }
}

fn fault_worker(gate: &PersistenceGate) {
    let mut state = gate
        .shared
        .state
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    state.health = StoreHealth::Faulted;
    state.queued_bytes = 0;
    while let Some(command) = state.queue.pop_front() {
        command.fail();
    }
    gate.shared.changed.notify_all();
}

fn persist_tombstone(root: &Path, id: ComputerId, present: bool) -> io::Result<()> {
    let computer = computer_path(root, id);
    let path = computer.join("tombstone");
    if present {
        std::fs::create_dir_all(&computer)?;
        let mut bytes = b"CPKTTOMB\0".to_vec();
        bytes.extend_from_slice(&id.into_bytes());
        let digest = Sha256::digest(&bytes);
        bytes.extend_from_slice(&digest);
        write_atomic(&path, &bytes)
    } else {
        std::fs::remove_file(path)?;
        File::open(computer)?.sync_all()
    }
}

fn collect_objects(root: &Path, objects: &[[u8; 32]]) -> io::Result<usize> {
    for object in objects {
        let path = object_path(root, *object);
        std::fs::remove_file(&path)?;
        File::open(path.parent().expect("fixed object shard"))?.sync_all()?;
    }
    Ok(objects.len())
}

fn persist_mutation(root: &Path, command: &MutationCommand) -> io::Result<()> {
    if let Some((object_id, object)) = &command.object {
        if Sha256::digest(object).as_slice() != object_id {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "object digest"));
        }
        let object_path = object_path(root, *object_id);
        if object_path.exists() {
            verify_object(&object_path, *object_id)?;
        } else {
            write_atomic(&object_path, object)?;
        }
    }

    let computer = computer_path(root, command.computer_id);
    let journal = computer.join("journal");
    std::fs::create_dir_all(&journal)?;
    let record_path = journal.join(format!("{:016x}", command.generation));
    write_atomic(&record_path, &command.journal)?;
    write_confirmed(&computer.join("confirmed"), command.generation)?;
    Ok(())
}

pub(crate) fn write_confirmed(path: &Path, generation: u64) -> io::Result<()> {
    let mut bytes = generation.to_le_bytes().to_vec();
    let digest = Sha256::digest(&bytes);
    bytes.extend_from_slice(&digest);
    write_atomic(path, &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension("tmp");
    if temporary.exists() {
        std::fs::remove_file(&temporary)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub(crate) fn verify_object(path: &Path, expected: [u8; 32]) -> io::Result<Arc<[u8]>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if Sha256::digest(&bytes).as_slice() != expected {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "object digest"));
    }
    Ok(bytes.into())
}

pub(crate) fn computer_path(root: &Path, id: ComputerId) -> PathBuf {
    root.join("computers").join(hex(&id.into_bytes()))
}

pub(crate) fn object_path(root: &Path, id: [u8; 32]) -> PathBuf {
    let encoded = hex(&id);
    root.join("objects").join(&encoded[..2]).join(encoded)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    result
}

fn require_active_store(health: StoreHealth) -> Result<(), StoreError> {
    match health {
        StoreHealth::Active => Ok(()),
        StoreHealth::Draining | StoreHealth::Closed => Err(StoreError::Closed),
        StoreHealth::Faulted => Err(StoreError::StorageFaulted),
    }
}

fn require_active_filesystem(health: StoreHealth) -> Result<(), FileSystemError> {
    match health {
        StoreHealth::Active => Ok(()),
        StoreHealth::Draining | StoreHealth::Closed => Err(FileSystemError::Closed),
        StoreHealth::Faulted => Err(FileSystemError::StorageFaulted),
    }
}

fn codec_to_filesystem(error: PersistenceCodecError) -> FileSystemError {
    match error {
        PersistenceCodecError::LimitExceeded => FileSystemError::QuotaExceeded,
        PersistenceCodecError::Malformed
        | PersistenceCodecError::UnsupportedVersion
        | PersistenceCodecError::DigestMismatch
        | PersistenceCodecError::NonCanonical => FileSystemError::StorageFaulted,
    }
}

fn filesystem_to_store(error: FileSystemError) -> StoreError {
    match error {
        FileSystemError::Busy => StoreError::Busy,
        FileSystemError::Closed => StoreError::Closed,
        FileSystemError::StorageFaulted => StoreError::StorageFaulted,
        FileSystemError::InvalidPath
        | FileSystemError::NotFound
        | FileSystemError::AlreadyExists
        | FileSystemError::NotDirectory
        | FileSystemError::IsDirectory
        | FileSystemError::NotExecutable
        | FileSystemError::NotEmpty
        | FileSystemError::ReadOnly
        | FileSystemError::PermissionDenied
        | FileSystemError::StaleHandle
        | FileSystemError::QuotaExceeded => StoreError::Io,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_fault_completes_every_queued_control_command() {
        let tombstone = Completion::<()>::new();
        let collection = Completion::<usize>::new();
        let mut queue = VecDeque::new();
        queue.push_back(Command::Tombstone(TombstoneCommand {
            computer_id: ComputerId::from_bytes([1; 16]),
            present: true,
            completion: tombstone.clone(),
        }));
        queue.push_back(Command::Collect(CollectCommand {
            objects: Arc::new([[2; 32]]),
            completion: collection.clone(),
        }));
        let gate = PersistenceGate {
            shared: Arc::new(Shared {
                state: Mutex::new(QueueState {
                    health: StoreHealth::Active,
                    queue,
                    queued_bytes: 32,
                    reserved_records: 0,
                    reserved_bytes: 0,
                    maximum_records: 2,
                    maximum_bytes: 32,
                    admitted: BTreeMap::new(),
                    durable: BTreeMap::new(),
                    stop_enqueued: false,
                }),
                changed: Condvar::new(),
            }),
        };

        fault_worker(&gate);

        assert_eq!(gate.health(), StoreHealth::Faulted);
        assert_eq!(tombstone.wait(), Err(StoreError::StorageFaulted));
        assert_eq!(collection.wait(), Err(StoreError::StorageFaulted));
        let state = gate
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        assert!(state.queue.is_empty());
        assert_eq!(state.queued_bytes, 0);
    }
}
