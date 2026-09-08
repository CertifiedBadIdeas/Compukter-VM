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

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use compukter_vm::filesystem::{
    FileSystemError, FileSystemLimits, PersistenceAtomicPhase, PersistenceAtomicTarget, StoreError,
    WorldFileSystemStore,
};
use persistence_crash_fixture::{
    empty_rom, mutation_point_name, path, seed, BASELINE_BYTES, BASELINE_PATH, COMPUTER_ID,
    CRASH_EXIT_CODE, NEXT_BYTES, NEXT_PATH,
};
use sha2::{Digest, Sha256};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
static SUBPROCESS_SCENARIOS: Mutex<()> = Mutex::new(());
const TARGETS: [PersistenceAtomicTarget; 3] = [
    PersistenceAtomicTarget::Object,
    PersistenceAtomicTarget::Journal,
    PersistenceAtomicTarget::Confirmed,
];
const PHASES: [PersistenceAtomicPhase; 5] = [
    PersistenceAtomicPhase::TemporaryCreated,
    PersistenceAtomicPhase::BytesWritten,
    PersistenceAtomicPhase::FileSynced,
    PersistenceAtomicPhase::Renamed,
    PersistenceAtomicPhase::DirectorySynced,
];

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let base = std::env::temp_dir().join("compukters-persistence-crash-tests");
        std::fs::create_dir_all(&base).expect("test base is created");
        let root = base.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&root).expect("test root is created");
        Self(root.canonicalize().expect("test root is canonical"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let safe_parent = self
            .0
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "compukters-persistence-crash-tests");
        assert!(safe_parent, "refusing to remove an unexpected path");
        if self.0.exists() {
            std::fs::remove_dir_all(&self.0).expect("test root is removed");
        }
    }
}

#[test]
fn mutation_crash_matrix_recovers_only_complete_atomic_publications() {
    let _serial = SUBPROCESS_SCENARIOS.lock().expect("scenario lock");
    for target in TARGETS {
        for phase in PHASES {
            let root = TestRoot::new();
            seed(root.path());
            let point = mutation_point_name(target, phase);
            let status = Command::new(env!("CARGO_BIN_EXE_persistence-crash-fixture"))
                .arg(root.path())
                .arg(&point)
                .status()
                .expect("crash fixture starts");
            assert_eq!(
                Some(CRASH_EXIT_CODE),
                status.code(),
                "unexpected child status at {point}: {status}",
            );

            let expects_next = target == PersistenceAtomicTarget::Confirmed
                || (target == PersistenceAtomicTarget::Journal
                    && matches!(
                        phase,
                        PersistenceAtomicPhase::Renamed | PersistenceAtomicPhase::DirectorySynced
                    ));
            assert_recovered(root.path(), expects_next, &point);
        }
    }
}

#[test]
fn temporary_entries_are_exact_and_count_against_scan_limits() {
    let _serial = SUBPROCESS_SCENARIOS.lock().expect("scenario lock");
    let root = TestRoot::new();
    seed(root.path());
    let computer = root.path().join("computers").join("59".repeat(16));
    let journal = computer.join("journal");

    let malformed = journal.join("unexpected.tmp");
    std::fs::write(&malformed, b"not an atomic temporary").expect("malformed temporary is written");
    let limits = FileSystemLimits::testing();
    let store = WorldFileSystemStore::open(root.path(), limits).expect("store opens");
    assert!(matches!(
        store.open_computer(COMPUTER_ID, empty_rom(&limits)),
        Err(StoreError::StorageFaulted),
    ));
    store.close().expect("faulted store closes");
    std::fs::remove_file(malformed).expect("malformed temporary is removed");

    let valid_journal_temporary = journal.join("0000000000000002.tmp");
    std::fs::write(&valid_journal_temporary, b"bounded temporary")
        .expect("valid temporary is written");
    let mut bounded = FileSystemLimits::testing();
    bounded.maximum_recovery_records = 1;
    let store = WorldFileSystemStore::open(root.path(), bounded).expect("bounded store opens");
    assert!(matches!(
        store.open_computer(COMPUTER_ID, empty_rom(&bounded)),
        Err(StoreError::StorageFaulted),
    ));
    store.close().expect("bounded store closes");
    std::fs::remove_file(valid_journal_temporary).expect("journal temporary is removed");

    let temporary_id: [u8; 32] = Sha256::digest(b"orphan temporary").into();
    let encoded = temporary_id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let object_temporary = root
        .path()
        .join("objects")
        .join(&encoded[..2])
        .join(format!("{encoded}.tmp"));
    std::fs::create_dir_all(object_temporary.parent().expect("object shard"))
        .expect("object shard is created");
    std::fs::write(&object_temporary, b"orphan temporary").expect("object temporary is written");

    let store = WorldFileSystemStore::open(root.path(), limits).expect("store reopens");
    store
        .open_computer(COMPUTER_ID, empty_rom(&limits))
        .expect("computer reopens");
    assert_eq!(
        Err(StoreError::Busy),
        store.collect_unreachable_objects(1, 1),
    );
    assert_eq!(0, store.collect_unreachable_objects(1, 2).unwrap());
    assert!(object_temporary.is_file());
    store.close().expect("store closes");
}

fn assert_recovered(root: &Path, expects_next: bool, point: &str) {
    let limits = FileSystemLimits::testing();
    let store = WorldFileSystemStore::open(root, limits).expect("store reopens after crash");
    let filesystem = store
        .open_computer(COMPUTER_ID, empty_rom(&limits))
        .expect("computer recovers after crash");
    assert_eq!(
        BASELINE_BYTES,
        filesystem
            .read_file_for_test(&path(BASELINE_PATH))
            .expect("baseline remains readable"),
        "baseline mismatch at {point}",
    );
    if expects_next {
        assert_eq!(2, filesystem.generation(), "generation mismatch at {point}");
        assert_eq!(
            NEXT_BYTES,
            filesystem
                .read_file_for_test(&path(NEXT_PATH))
                .expect("published mutation is readable"),
            "next file mismatch at {point}",
        );
    } else {
        assert_eq!(1, filesystem.generation(), "generation mismatch at {point}");
        assert_eq!(
            Err(FileSystemError::NotFound),
            filesystem.read_file_for_test(&path(NEXT_PATH)),
            "partial mutation became visible at {point}",
        );
    }
    assert_eq!(
        filesystem.generation(),
        store
            .durable_generation(COMPUTER_ID)
            .expect("recovered generation is durable"),
        "durable generation mismatch at {point}",
    );
    store
        .collect_unreachable_objects(1, 16)
        .expect("post-crash collection succeeds");
    store.close().expect("recovered store closes");
}
