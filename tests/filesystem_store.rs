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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use compukter_vm::{
    ComputerId, FileCapability, FileRights, FileSystemError, FileSystemLimits, OpenMode, RomImage,
    StoreError, StoreHealth, StoreOpenError, VirtualPath, WorldFileSystemStore,
};
use sha2::{Digest, Sha256};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let base = std::env::temp_dir().join("compukters-vfs-tests");
        std::fs::create_dir_all(&base).unwrap();
        let root = base.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let parent_is_safe = self
            .0
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "compukters-vfs-tests");
        let name_is_safe = self
            .0
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&format!("{}-", std::process::id())));
        assert!(parent_is_safe && name_is_safe);
        if self.0.exists() {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

fn empty_rom(limits: &FileSystemLimits) -> Arc<RomImage> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"CPKTROM\0");
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    let digest = Sha256::digest(&bytes);
    bytes.extend_from_slice(&digest);
    Arc::new(RomImage::admit(bytes.into(), limits).unwrap())
}

fn path(value: &str) -> VirtualPath {
    VirtualPath::parse_utf8(value, &FileSystemLimits::testing()).unwrap()
}

fn owner() -> FileCapability {
    FileCapability::new(path("/home"), FileRights::OWNER)
}

#[test]
fn world_root_is_canonical_exclusive_and_explicitly_closed() {
    let root = TestRoot::new();
    assert!(matches!(
        WorldFileSystemStore::open(Path::new("relative"), FileSystemLimits::testing()),
        Err(StoreOpenError::RootNotAbsolute)
    ));

    let store = WorldFileSystemStore::open(root.path(), FileSystemLimits::testing()).unwrap();
    assert_eq!(store.health(), StoreHealth::Active);
    assert!(matches!(
        WorldFileSystemStore::open(root.path(), FileSystemLimits::testing()),
        Err(StoreOpenError::Locked)
    ));

    store.close().unwrap();
    assert_eq!(store.health(), StoreHealth::Closed);
    store.close().unwrap();
    assert!(!root.path().join("lock").exists());
}

#[cfg(unix)]
#[test]
fn symlink_world_root_is_rejected() {
    use std::os::unix::fs::symlink;

    let root = TestRoot::new();
    let link = root.path().parent().unwrap().join(format!(
        "{}-link-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    symlink(root.path(), &link).unwrap();
    assert!(matches!(
        WorldFileSystemStore::open(&link, FileSystemLimits::testing()),
        Err(StoreOpenError::RootNotCanonical)
    ));
    std::fs::remove_file(link).unwrap();
}

#[test]
fn queue_backpressure_and_store_close_precede_visible_mutation() {
    let root = TestRoot::new();
    let mut limits = FileSystemLimits::testing();
    limits.maximum_persistence_queue_records = 0;
    let store = WorldFileSystemStore::open(root.path(), limits).unwrap();
    let mut filesystem = store
        .open_computer(ComputerId::from_bytes([1; 16]), empty_rom(&limits))
        .unwrap();
    let before = filesystem.snapshot_for_test();

    assert_eq!(
        filesystem.write_file(&owner(), &path("/home/a"), b"a", false),
        Err(FileSystemError::Busy),
    );
    assert_eq!(filesystem.snapshot_for_test(), before);

    store.close().unwrap();
    assert_eq!(
        filesystem.write_file(&owner(), &path("/home/a"), b"a", false),
        Err(FileSystemError::Closed),
    );
    assert_eq!(filesystem.snapshot_for_test(), before);
}

#[test]
fn admitted_mutation_becomes_durable_and_recovers_after_restart() {
    let root = TestRoot::new();
    let limits = FileSystemLimits::testing();
    let computer = ComputerId::from_bytes([2; 16]);
    {
        let store = WorldFileSystemStore::open(root.path(), limits).unwrap();
        let mut filesystem = store.open_computer(computer, empty_rom(&limits)).unwrap();
        filesystem
            .write_file(
                &owner(),
                &path("/home/program"),
                b"persistent artifact",
                true,
            )
            .unwrap();

        store.flush(computer, filesystem.generation()).unwrap();
        assert_eq!(store.durable_generation(computer).unwrap(), 1);
        store.close().unwrap();
    }

    let store = WorldFileSystemStore::open(root.path(), limits).unwrap();
    let filesystem = store.open_computer(computer, empty_rom(&limits)).unwrap();
    assert_eq!(filesystem.generation(), 1);
    assert_eq!(
        filesystem
            .read_file_for_test(&path("/home/program"))
            .unwrap(),
        b"persistent artifact",
    );
    assert!(
        filesystem
            .stat(&owner(), &path("/home/program"))
            .unwrap()
            .executable
    );
    store.close().unwrap();
}

#[test]
fn every_namespace_and_byte_mutation_replays_in_generation_order() {
    let root = TestRoot::new();
    let limits = FileSystemLimits::testing();
    let computer = ComputerId::from_bytes([3; 16]);
    {
        let store = WorldFileSystemStore::open(root.path(), limits).unwrap();
        let mut filesystem = store.open_computer(computer, empty_rom(&limits)).unwrap();
        filesystem
            .write_file(&owner(), &path("/home/a"), b"abcdef", false)
            .unwrap();
        filesystem
            .create_directory(&owner(), &path("/home/dir"))
            .unwrap();
        filesystem
            .rename(
                &owner(),
                &path("/home/a"),
                &path("/home/dir/program"),
                false,
            )
            .unwrap();
        let handle = filesystem
            .open(&owner(), &path("/home/dir/program"), OpenMode::ReadWrite)
            .unwrap();
        filesystem.write(handle, 1, b"XY").unwrap();
        filesystem.truncate(handle, 4).unwrap();
        filesystem.close(handle).unwrap();
        filesystem
            .write_file(&owner(), &path("/home/temporary"), b"gone", false)
            .unwrap();
        filesystem
            .remove(&owner(), &path("/home/temporary"))
            .unwrap();
        assert_eq!(filesystem.generation(), 7);

        store.flush(computer, 7).unwrap();
        store.close().unwrap();
    }

    let store = WorldFileSystemStore::open(root.path(), limits).unwrap();
    let filesystem = store.open_computer(computer, empty_rom(&limits)).unwrap();
    assert_eq!(filesystem.generation(), 7);
    assert_eq!(
        filesystem
            .read_file_for_test(&path("/home/dir/program"))
            .unwrap(),
        b"aXYd",
    );
    assert!(filesystem
        .read_file_for_test(&path("/home/temporary"))
        .is_err());
    store.close().unwrap();
}

#[test]
fn corrupt_object_faults_recovery_without_exposing_host_details() {
    let root = TestRoot::new();
    let limits = FileSystemLimits::testing();
    let computer = ComputerId::from_bytes([4; 16]);
    {
        let store = WorldFileSystemStore::open(root.path(), limits).unwrap();
        let mut filesystem = store.open_computer(computer, empty_rom(&limits)).unwrap();
        filesystem
            .write_file(&owner(), &path("/home/a"), b"verified object", false)
            .unwrap();
        store.flush(computer, 1).unwrap();
        store.close().unwrap();
    }
    let shard = std::fs::read_dir(root.path().join("objects"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let object = std::fs::read_dir(shard)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(object, b"corrupt").unwrap();

    let store = WorldFileSystemStore::open(root.path(), limits).unwrap();
    assert!(matches!(
        store.open_computer(computer, empty_rom(&limits)),
        Err(StoreError::StorageFaulted)
    ));
    assert_eq!(store.health(), StoreHealth::Faulted);
    store.close().unwrap();
}

#[test]
fn worker_io_fault_degrades_existing_computer_to_read_only() {
    let root = TestRoot::new();
    let limits = FileSystemLimits::testing();
    let computer = ComputerId::from_bytes([5; 16]);
    let store = WorldFileSystemStore::open(root.path(), limits).unwrap();
    let mut filesystem = store.open_computer(computer, empty_rom(&limits)).unwrap();
    filesystem
        .write_file(&owner(), &path("/home/stable"), b"stable", false)
        .unwrap();
    store.flush(computer, 1).unwrap();

    let journal = root
        .path()
        .join("computers")
        .join("05".repeat(16))
        .join("journal");
    std::fs::remove_dir_all(&journal).unwrap();
    std::fs::write(&journal, b"not a directory").unwrap();
    filesystem
        .write_file(&owner(), &path("/home/volatile"), b"visible", false)
        .unwrap();
    assert_eq!(store.flush(computer, 2), Err(StoreError::StorageFaulted));
    assert_eq!(store.health(), StoreHealth::Faulted);
    assert_eq!(
        filesystem
            .read_file_for_test(&path("/home/stable"))
            .unwrap(),
        b"stable",
    );
    let before = filesystem.snapshot_for_test();
    assert_eq!(
        filesystem.write_file(&owner(), &path("/home/rejected"), b"x", false),
        Err(FileSystemError::StorageFaulted),
    );
    assert_eq!(filesystem.snapshot_for_test(), before);
    store.close().unwrap();
}

#[test]
fn tombstone_is_durable_recoverable_and_blocks_open() {
    let root = TestRoot::new();
    let limits = FileSystemLimits::testing();
    let computer = ComputerId::from_bytes([6; 16]);
    let store = WorldFileSystemStore::open(root.path(), limits).unwrap();
    let mut filesystem = store.open_computer(computer, empty_rom(&limits)).unwrap();
    filesystem
        .write_file(&owner(), &path("/home/a"), b"recoverable", false)
        .unwrap();
    store.flush(computer, 1).unwrap();

    store.tombstone(computer).unwrap();
    assert!(matches!(
        store.open_computer(computer, empty_rom(&limits)),
        Err(StoreError::NotFound)
    ));
    store.recover_tombstone(computer).unwrap();
    let recovered = store.open_computer(computer, empty_rom(&limits)).unwrap();
    assert_eq!(
        recovered.read_file_for_test(&path("/home/a")).unwrap(),
        b"recoverable",
    );
    store.close().unwrap();

    let store = WorldFileSystemStore::open(root.path(), limits).unwrap();
    let recovered = store.open_computer(computer, empty_rom(&limits)).unwrap();
    assert_eq!(recovered.generation(), 1);
    store.close().unwrap();
}

#[test]
fn bounded_collection_keeps_reachable_objects_and_removes_only_unreachable_ones() {
    let root = TestRoot::new();
    let limits = FileSystemLimits::testing();
    let computer = ComputerId::from_bytes([7; 16]);
    let store = WorldFileSystemStore::open(root.path(), limits).unwrap();
    let mut filesystem = store.open_computer(computer, empty_rom(&limits)).unwrap();
    filesystem
        .write_file(&owner(), &path("/home/a"), b"reachable", false)
        .unwrap();
    store.flush(computer, 1).unwrap();

    let reachable: [u8; 32] = Sha256::digest(b"reachable").into();
    let unreachable: [u8; 32] = Sha256::digest(b"unreachable").into();
    let object_path = |digest: [u8; 32]| {
        let encoded = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        root.path()
            .join("objects")
            .join(&encoded[..2])
            .join(encoded)
    };
    let unreachable_path = object_path(unreachable);
    std::fs::create_dir_all(unreachable_path.parent().unwrap()).unwrap();
    std::fs::write(&unreachable_path, b"unreachable").unwrap();

    assert_eq!(
        store.collect_unreachable_objects(10, 1),
        Err(StoreError::Busy),
    );
    assert!(unreachable_path.exists());
    assert_eq!(store.collect_unreachable_objects(10, 10).unwrap(), 1);
    assert!(object_path(reachable).exists());
    assert!(!unreachable_path.exists());
    store.close().unwrap();
}
