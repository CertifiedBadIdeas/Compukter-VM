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

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use sha2::{Digest, Sha256};

use super::worker::{computer_path, object_path, verify_object, write_confirmed, PersistenceGate};
use super::{
    recover, ComputerFileSystem, ComputerId, FileSystemLimits, RecoveryCheckpoint, RecoveryInput,
    RecoveryJournalRecord, RomImage, StoreHealth,
};

type ObjectMap = BTreeMap<[u8; 32], Arc<[u8]>>;
type GenerationFile = (u64, Arc<[u8]>);

struct RecoveredComputer {
    state: super::RecoveredState,
    objects: ObjectMap,
    confirmed_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreOpenError {
    RootNotAbsolute,
    RootNotCanonical,
    RootNotDirectory,
    Locked,
    Io,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreError {
    NotFound,
    Busy,
    StorageFaulted,
    Closed,
    InvalidGeneration,
    Io,
}

pub struct WorldFileSystemStore {
    root: PathBuf,
    lock_file: Mutex<Option<File>>,
    health: Mutex<StoreHealth>,
    persistence: PersistenceGate,
    worker: Mutex<Option<JoinHandle<()>>>,
    limits: FileSystemLimits,
}

impl WorldFileSystemStore {
    pub fn open(root: &Path, limits: FileSystemLimits) -> Result<Arc<Self>, StoreOpenError> {
        if !root.is_absolute() {
            return Err(StoreOpenError::RootNotAbsolute);
        }
        let metadata = std::fs::symlink_metadata(root).map_err(|_| StoreOpenError::Io)?;
        if metadata.file_type().is_symlink() {
            return Err(StoreOpenError::RootNotCanonical);
        }
        if !metadata.is_dir() {
            return Err(StoreOpenError::RootNotDirectory);
        }
        let canonical = root.canonicalize().map_err(|_| StoreOpenError::Io)?;
        if canonical != root {
            return Err(StoreOpenError::RootNotCanonical);
        }
        let lock_path = root.join("lock");
        let lock_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
            .map_err(|error| match error.kind() {
                ErrorKind::AlreadyExists => StoreOpenError::Locked,
                _ => StoreOpenError::Io,
            })?;
        std::fs::create_dir_all(root.join("objects")).map_err(|_| StoreOpenError::Io)?;
        std::fs::create_dir_all(root.join("computers")).map_err(|_| StoreOpenError::Io)?;
        let (persistence, worker) =
            PersistenceGate::start(root.to_owned(), &limits).map_err(|_| StoreOpenError::Io)?;
        Ok(Arc::new(Self {
            root: root.to_owned(),
            lock_file: Mutex::new(Some(lock_file)),
            health: Mutex::new(StoreHealth::Active),
            persistence,
            worker: Mutex::new(Some(worker)),
            limits,
        }))
    }

    pub fn health(&self) -> StoreHealth {
        self.persistence.health()
    }

    pub const fn limits(&self) -> &FileSystemLimits {
        &self.limits
    }

    pub fn open_computer(
        &self,
        id: ComputerId,
        rom: Arc<RomImage>,
    ) -> Result<ComputerFileSystem, StoreError> {
        match self.health() {
            StoreHealth::Active => {}
            StoreHealth::Draining | StoreHealth::Closed => return Err(StoreError::Closed),
            StoreHealth::Faulted => return Err(StoreError::StorageFaulted),
        }
        if read_tombstone(&computer_path(&self.root, id).join("tombstone"), id)? {
            return Err(StoreError::NotFound);
        }
        let result = self.recover_computer(id);
        let recovered = match result {
            Ok(result) => result,
            Err(error) => {
                self.persistence.fault();
                return Err(error);
            }
        };
        if recovered.state.generation() > recovered.confirmed_generation {
            let confirmed_path = computer_path(&self.root, id).join("confirmed");
            if write_confirmed(&confirmed_path, recovered.state.generation()).is_err() {
                self.persistence.fault();
                return Err(StoreError::StorageFaulted);
            }
        }
        let mut filesystem = ComputerFileSystem::with_rom(self.limits, (*rom).clone())
            .map_err(|_| StoreError::Io)?;
        filesystem
            .restore_recovered(&recovered.state, &recovered.objects)
            .map_err(|_| StoreError::StorageFaulted)?;
        self.persistence
            .register_computer(id, recovered.state.generation())?;
        filesystem.attach_persistence(self.persistence.computer(id, self.limits));
        Ok(filesystem)
    }

    pub fn durable_generation(&self, id: ComputerId) -> Result<u64, StoreError> {
        self.persistence.durable_generation(id)
    }

    pub fn flush(&self, id: ComputerId, generation: u64) -> Result<(), StoreError> {
        self.persistence.flush(id, generation)
    }

    pub fn tombstone(&self, id: ComputerId) -> Result<(), StoreError> {
        self.persistence.tombstone(id, true)
    }

    pub fn recover_tombstone(&self, id: ComputerId) -> Result<(), StoreError> {
        self.persistence.tombstone(id, false)
    }

    pub fn collect_unreachable_objects(
        &self,
        maximum_computers: usize,
        maximum_objects: usize,
    ) -> Result<usize, StoreError> {
        self.persistence.flush_all()?;
        let mut reachable = BTreeSet::new();
        let computers = self.root.join("computers");
        let mut computer_count = 0_usize;
        for entry in std::fs::read_dir(&computers).map_err(|_| StoreError::Io)? {
            let entry = entry.map_err(|_| StoreError::Io)?;
            let file_type = entry.file_type().map_err(|_| StoreError::Io)?;
            if file_type.is_symlink() || !file_type.is_dir() {
                return Err(StoreError::StorageFaulted);
            }
            computer_count = computer_count.checked_add(1).ok_or(StoreError::Busy)?;
            if computer_count > maximum_computers {
                return Err(StoreError::Busy);
            }
            let name = entry.file_name();
            let id = decode_hex::<16>(name.to_str().ok_or(StoreError::StorageFaulted)?)
                .ok_or(StoreError::StorageFaulted)?;
            let recovered = self.recover_computer(ComputerId::from_bytes(id))?;
            reachable.extend(recovered.objects.keys().copied());
        }

        let mut stored = Vec::new();
        for shard in std::fs::read_dir(self.root.join("objects")).map_err(|_| StoreError::Io)? {
            let shard = shard.map_err(|_| StoreError::Io)?;
            let shard_type = shard.file_type().map_err(|_| StoreError::Io)?;
            if shard_type.is_symlink() || !shard_type.is_dir() {
                return Err(StoreError::StorageFaulted);
            }
            let shard_name = shard.file_name();
            let shard_name = shard_name.to_str().ok_or(StoreError::StorageFaulted)?;
            if shard_name.len() != 2 || decode_hex::<1>(shard_name).is_none() {
                return Err(StoreError::StorageFaulted);
            }
            for object in std::fs::read_dir(shard.path()).map_err(|_| StoreError::Io)? {
                let object = object.map_err(|_| StoreError::Io)?;
                let object_type = object.file_type().map_err(|_| StoreError::Io)?;
                if object_type.is_symlink() || !object_type.is_file() {
                    return Err(StoreError::StorageFaulted);
                }
                if stored.len() >= maximum_objects {
                    return Err(StoreError::Busy);
                }
                let name = object.file_name();
                let name = name.to_str().ok_or(StoreError::StorageFaulted)?;
                let id = decode_hex::<32>(name).ok_or(StoreError::StorageFaulted)?;
                if &name[..2] != shard_name {
                    return Err(StoreError::StorageFaulted);
                }
                stored.push(id);
            }
        }
        let unreachable = stored
            .into_iter()
            .filter(|object| !reachable.contains(object))
            .collect();
        self.persistence.collect(unreachable)
    }

    fn recover_computer(&self, id: ComputerId) -> Result<RecoveredComputer, StoreError> {
        let computer = computer_path(&self.root, id);
        let confirmed = read_confirmed(&computer.join("confirmed"))?;
        let mut input = RecoveryInput::new(id, confirmed);
        let mut recovery_bytes = 0_usize;
        let mut recovery_records = 0_usize;
        for (generation, bytes) in read_generation_files(
            &computer.join("checkpoints"),
            self.limits.maximum_checkpoint_bytes,
            &mut recovery_bytes,
            self.limits.maximum_recovery_bytes,
            &mut recovery_records,
            self.limits.maximum_recovery_records,
        )? {
            input = input.with_checkpoint(RecoveryCheckpoint::new(generation, bytes));
        }
        for (sequence, bytes) in read_generation_files(
            &computer.join("journal"),
            self.limits.maximum_journal_record_bytes,
            &mut recovery_bytes,
            self.limits.maximum_recovery_bytes,
            &mut recovery_records,
            self.limits.maximum_recovery_records,
        )? {
            input = input.with_journal(RecoveryJournalRecord::new(sequence, bytes));
        }
        let recovered = recover(&input, &self.limits).map_err(|_| StoreError::StorageFaulted)?;
        let mut objects = ObjectMap::new();
        for node in recovered.nodes() {
            let Some((object_id, logical_size)) = node.object() else {
                continue;
            };
            if objects.contains_key(&object_id) {
                continue;
            }
            let bytes = verify_object(&object_path(&self.root, object_id), object_id)
                .map_err(|_| StoreError::StorageFaulted)?;
            if bytes.len() as u64 != logical_size {
                return Err(StoreError::StorageFaulted);
            }
            objects.insert(object_id, bytes);
        }
        Ok(RecoveredComputer {
            state: recovered,
            objects,
            confirmed_generation: confirmed,
        })
    }

    pub fn close(&self) -> Result<(), StoreError> {
        let mut health = self
            .health
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if *health == StoreHealth::Closed {
            return Ok(());
        }
        *health = StoreHealth::Draining;
        self.persistence.begin_draining();
        self.persistence.stop();
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
        {
            worker.join().map_err(|_| StoreError::StorageFaulted)?;
        }
        let mut lock = self
            .lock_file
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        lock.take();
        std::fs::remove_file(self.root.join("lock")).map_err(|_| StoreError::Io)?;
        *health = StoreHealth::Closed;
        self.persistence.mark_closed();
        Ok(())
    }
}

fn read_confirmed(path: &Path) -> Result<u64, StoreError> {
    if !path.exists() {
        return Ok(0);
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| StoreError::Io)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != 40 {
        return Err(StoreError::StorageFaulted);
    }
    let mut bytes = [0_u8; 40];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| StoreError::Io)?;
    if Sha256::digest(&bytes[..8]).as_slice() != &bytes[8..] {
        return Err(StoreError::StorageFaulted);
    }
    Ok(u64::from_le_bytes(
        bytes[..8].try_into().expect("exact width"),
    ))
}

fn read_tombstone(path: &Path, id: ComputerId) -> Result<bool, StoreError> {
    if !path.exists() {
        return Ok(false);
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| StoreError::Io)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != 57 {
        return Err(StoreError::StorageFaulted);
    }
    let mut bytes = [0_u8; 57];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| StoreError::Io)?;
    if &bytes[..9] != b"CPKTTOMB\0"
        || bytes[9..25] != id.into_bytes()
        || Sha256::digest(&bytes[..25]).as_slice() != &bytes[25..]
    {
        return Err(StoreError::StorageFaulted);
    }
    Ok(true)
}

fn read_generation_files(
    directory: &Path,
    maximum_file_bytes: usize,
    recovery_bytes: &mut usize,
    maximum_recovery_bytes: usize,
    recovery_records: &mut usize,
    maximum_recovery_records: usize,
) -> Result<Vec<GenerationFile>, StoreError> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let metadata = std::fs::symlink_metadata(directory).map_err(|_| StoreError::Io)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(StoreError::StorageFaulted);
    }
    let mut result = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(|_| StoreError::Io)? {
        let entry = entry.map_err(|_| StoreError::Io)?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(StoreError::StorageFaulted)?;
        if name.ends_with(".tmp") {
            continue;
        }
        *recovery_records = recovery_records
            .checked_add(1)
            .ok_or(StoreError::StorageFaulted)?;
        if *recovery_records > maximum_recovery_records {
            return Err(StoreError::StorageFaulted);
        }
        if name.len() != 16 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StoreError::StorageFaulted);
        }
        let generation = u64::from_str_radix(name, 16).map_err(|_| StoreError::StorageFaulted)?;
        if format!("{generation:016x}") != name {
            return Err(StoreError::StorageFaulted);
        }
        let file_type = entry.file_type().map_err(|_| StoreError::Io)?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(StoreError::StorageFaulted);
        }
        let metadata = entry.metadata().map_err(|_| StoreError::Io)?;
        let length = usize::try_from(metadata.len()).map_err(|_| StoreError::StorageFaulted)?;
        *recovery_bytes = recovery_bytes
            .checked_add(length)
            .ok_or(StoreError::StorageFaulted)?;
        if length > maximum_file_bytes || *recovery_bytes > maximum_recovery_bytes {
            return Err(StoreError::StorageFaulted);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| StoreError::StorageFaulted)?;
        File::open(entry.path())
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|_| StoreError::Io)?;
        if bytes.len() != length {
            return Err(StoreError::StorageFaulted);
        }
        result.push((generation, bytes.into()));
    }
    Ok(result)
}

fn decode_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != N.checked_mul(2)?
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut result = [0_u8; N];
    for (index, byte) in result.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = u8::from_str_radix(&value[offset..offset + 2], 16).ok()?;
    }
    Some(result)
}

impl Drop for WorldFileSystemStore {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
