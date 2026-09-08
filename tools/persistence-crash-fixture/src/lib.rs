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

use std::path::Path;
use std::sync::Arc;

use compukter_vm::filesystem::{
    ComputerId, FileCapability, FileRights, FileSystemLimits, PersistenceAtomicPhase,
    PersistenceAtomicTarget, PersistenceCrashPoint, RomImage, VirtualPath, WorldFileSystemStore,
};
use sha2::{Digest, Sha256};

pub const CRASH_EXIT_CODE: i32 = 86;
pub const COMPUTER_ID: ComputerId = ComputerId::from_bytes([0x59; 16]);
pub const BASELINE_PATH: &str = "/home/baseline";
pub const NEXT_PATH: &str = "/home/next";
pub const BASELINE_BYTES: &[u8] = b"durable baseline";
pub const NEXT_BYTES: &[u8] = b"crash point payload";

pub fn seed(root: &Path) {
    let limits = FileSystemLimits::testing();
    let store = WorldFileSystemStore::open(root, limits).expect("seed store opens");
    let mut filesystem = store
        .open_computer(COMPUTER_ID, empty_rom(&limits))
        .expect("seed filesystem opens");
    filesystem
        .write_file(&owner(), &path(BASELINE_PATH), BASELINE_BYTES, false)
        .expect("baseline mutation publishes");
    store
        .flush(COMPUTER_ID, filesystem.generation())
        .expect("baseline is durable");
    store.close().expect("seed store closes");
}

pub fn run_mutation(root: &Path, point: PersistenceCrashPoint) {
    let limits = FileSystemLimits::testing();
    let store = WorldFileSystemStore::open_with_persistence_crash_point(root, limits, point)
        .expect("crash store opens");
    let mut filesystem = store
        .open_computer(COMPUTER_ID, empty_rom(&limits))
        .expect("crash filesystem opens");
    filesystem
        .write_file(&owner(), &path(NEXT_PATH), NEXT_BYTES, false)
        .expect("crash mutation publishes");
    store
        .flush(COMPUTER_ID, filesystem.generation())
        .expect("an armed crash point must terminate before flush returns");
    panic!("armed persistence crash point did not terminate the process");
}

pub fn parse_mutation_point(value: &str) -> Option<PersistenceCrashPoint> {
    let (target, phase) = value.split_once('.')?;
    let target = match target {
        "object" => PersistenceAtomicTarget::Object,
        "journal" => PersistenceAtomicTarget::Journal,
        "confirmed" => PersistenceAtomicTarget::Confirmed,
        _ => return None,
    };
    let phase = match phase {
        "temporary-created" => PersistenceAtomicPhase::TemporaryCreated,
        "bytes-written" => PersistenceAtomicPhase::BytesWritten,
        "file-synced" => PersistenceAtomicPhase::FileSynced,
        "renamed" => PersistenceAtomicPhase::Renamed,
        "directory-synced" => PersistenceAtomicPhase::DirectorySynced,
        _ => return None,
    };
    Some(PersistenceCrashPoint::Atomic { target, phase })
}

pub fn mutation_point_name(
    target: PersistenceAtomicTarget,
    phase: PersistenceAtomicPhase,
) -> String {
    let target = match target {
        PersistenceAtomicTarget::Object => "object",
        PersistenceAtomicTarget::Journal => "journal",
        PersistenceAtomicTarget::Confirmed => "confirmed",
        PersistenceAtomicTarget::Tombstone => "tombstone",
    };
    let phase = match phase {
        PersistenceAtomicPhase::TemporaryCreated => "temporary-created",
        PersistenceAtomicPhase::BytesWritten => "bytes-written",
        PersistenceAtomicPhase::FileSynced => "file-synced",
        PersistenceAtomicPhase::Renamed => "renamed",
        PersistenceAtomicPhase::DirectorySynced => "directory-synced",
    };
    format!("{target}.{phase}")
}

pub fn empty_rom(limits: &FileSystemLimits) -> Arc<RomImage> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"CPKTROM\0");
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    let digest = Sha256::digest(&bytes);
    bytes.extend_from_slice(&digest);
    Arc::new(RomImage::admit(bytes.into(), limits).expect("empty ROM is valid"))
}

pub fn owner() -> FileCapability {
    FileCapability::new(path("/home"), FileRights::OWNER)
}

pub fn path(value: &str) -> VirtualPath {
    VirtualPath::parse_utf8(value, &FileSystemLimits::testing()).expect("fixture path is valid")
}
