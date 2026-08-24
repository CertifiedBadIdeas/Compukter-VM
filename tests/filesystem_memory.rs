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

use compukter_vm::{
    ComputerFileSystem, FileCapability, FileRights, FileSystemError, FileSystemLimits, OpenMode,
    VirtualPath,
};

fn path(value: &str) -> VirtualPath {
    VirtualPath::parse_utf16(
        &value.encode_utf16().collect::<Vec<_>>(),
        &FileSystemLimits::testing(),
    )
    .unwrap()
}

fn owner() -> FileCapability {
    FileCapability::new(path("/home"), FileRights::OWNER)
}

#[test]
fn file_handles_support_bounded_read_write_truncate_and_close() {
    let mut limits = FileSystemLimits::testing();
    limits.maximum_io_bytes = 3;
    let mut filesystem = ComputerFileSystem::with_limits(limits);
    let owner = owner();
    filesystem
        .write_file(&owner, &path("/home/program"), b"abcdef", true)
        .unwrap();

    let metadata = filesystem.stat(&owner, &path("/home/program")).unwrap();
    assert_eq!(metadata.logical_size, 6);
    assert!(metadata.executable);

    let handle = filesystem
        .open(&owner, &path("/home/program"), OpenMode::ReadWrite)
        .unwrap();
    assert_eq!(filesystem.read(handle, 1, 20).unwrap(), b"bcd");
    assert_eq!(filesystem.write(handle, 2, b"WXYZ").unwrap(), 3);
    assert_eq!(filesystem.read(handle, 0, 20).unwrap(), b"abW");
    filesystem.truncate(handle, 8).unwrap();
    assert_eq!(filesystem.read(handle, 5, 20).unwrap(), [b'f', 0, 0]);
    filesystem.close(handle).unwrap();
    assert_eq!(
        filesystem.read(handle, 0, 1),
        Err(FileSystemError::StaleHandle),
    );
}

#[test]
fn failed_reservation_leaves_bytes_generation_tree_and_objects_unchanged() {
    let mut limits = FileSystemLimits::testing();
    limits.maximum_logical_bytes = 4;
    let mut filesystem = ComputerFileSystem::with_limits(limits);
    let owner = owner();
    filesystem
        .write_file(&owner, &path("/home/a"), b"1234", false)
        .unwrap();
    let before = filesystem.snapshot_for_test();

    assert_eq!(
        filesystem.write_file(&owner, &path("/home/b"), b"x", false),
        Err(FileSystemError::QuotaExceeded),
    );
    assert_eq!(filesystem.snapshot_for_test(), before);
    assert_eq!(
        filesystem.read_file_for_test(&path("/home/a")).unwrap(),
        b"1234"
    );
}

#[test]
fn physical_deduplication_does_not_discount_logical_quota() {
    let mut limits = FileSystemLimits::testing();
    limits.maximum_logical_bytes = 8;
    let mut filesystem = ComputerFileSystem::with_limits(limits);
    let owner = owner();

    filesystem
        .write_file(&owner, &path("/home/a"), b"same", false)
        .unwrap();
    filesystem
        .write_file(&owner, &path("/home/b"), b"same", false)
        .unwrap();

    assert_eq!(filesystem.logical_bytes(), 8);
    assert_eq!(filesystem.object_count(), 1);
    assert_eq!(
        filesystem.write_file(&owner, &path("/home/c"), b"x", false),
        Err(FileSystemError::QuotaExceeded),
    );
}

#[test]
fn file_and_node_limits_are_checked_before_visible_mutation() {
    let mut limits = FileSystemLimits::testing();
    limits.maximum_file_bytes = 3;
    limits.maximum_nodes = 3;
    let mut filesystem = ComputerFileSystem::with_limits(limits);
    let owner = owner();

    assert_eq!(
        filesystem.write_file(&owner, &path("/home/large"), b"1234", false),
        Err(FileSystemError::QuotaExceeded),
    );
    filesystem
        .write_file(&owner, &path("/home/file"), b"123", false)
        .unwrap();
    let before = filesystem.snapshot_for_test();
    assert_eq!(
        filesystem.create_directory(&owner, &path("/home/directory")),
        Err(FileSystemError::QuotaExceeded),
    );
    assert_eq!(filesystem.snapshot_for_test(), before);
}

#[test]
fn executable_reads_require_authority_and_executable_metadata() {
    let mut filesystem = ComputerFileSystem::with_limits(FileSystemLimits::testing());
    let owner = owner();
    filesystem
        .write_file(&owner, &path("/home/tool"), b"verified artifact", true)
        .unwrap();
    filesystem
        .write_file(&owner, &path("/home/data"), b"ordinary data", false)
        .unwrap();
    filesystem
        .create_directory(&owner, &path("/home/directory"))
        .unwrap();
    let reader = FileCapability::new(path("/home"), FileRights::INSPECT | FileRights::READ);
    let executor = FileCapability::new(
        path("/home"),
        FileRights::INSPECT | FileRights::READ | FileRights::EXECUTE,
    );

    assert_eq!(
        filesystem.read_executable(&reader, &path("/home/tool")),
        Err(FileSystemError::PermissionDenied),
    );
    assert_eq!(
        filesystem.read_executable(&executor, &path("/home/data")),
        Err(FileSystemError::NotExecutable),
    );
    assert_eq!(
        filesystem.read_executable(&executor, &path("/home/directory")),
        Err(FileSystemError::NotExecutable),
    );
    assert_eq!(
        filesystem.read_executable(&executor, &path("/home/missing")),
        Err(FileSystemError::NotFound),
    );
    assert_eq!(
        filesystem
            .read_executable(&executor, &path("/home/tool"))
            .unwrap(),
        b"verified artifact",
    );
}
