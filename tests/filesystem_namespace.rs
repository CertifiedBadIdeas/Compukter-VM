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
    ComputerFileSystem, FileCapability, FileRights, FileSystemError, FileSystemLimits, HandleTable,
    OpenFile, VirtualPath,
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
fn namespace_has_deterministic_isolated_mounts() {
    let mut filesystem = ComputerFileSystem::testing();
    let owner = owner();

    assert_eq!(
        filesystem.list(&owner, &path("/home")).unwrap(),
        Vec::<Box<str>>::new()
    );
    filesystem
        .create_directory(&owner, &path("/home/λ"))
        .unwrap();
    filesystem
        .create_directory(&owner, &path("/home/z"))
        .unwrap();
    filesystem
        .create_directory(&owner, &path("/home/😀"))
        .unwrap();
    filesystem
        .create_directory(&owner, &path("/home/a"))
        .unwrap();

    assert_eq!(
        filesystem.list(&owner, &path("/home")).unwrap(),
        ["a", "z", "λ", "😀"].map(Box::<str>::from),
    );
    assert_eq!(filesystem.generation(), 4);
}

#[test]
fn rom_rejects_mutation_even_with_overbroad_rights() {
    let mut filesystem = ComputerFileSystem::testing();
    let forged = FileCapability::new(path("/"), FileRights::OWNER);

    assert_eq!(
        filesystem.create_directory(&forged, &path("/rom/changed")),
        Err(FileSystemError::ReadOnly),
    );
    assert_eq!(filesystem.generation(), 0);
}

#[test]
fn delegation_can_only_narrow_authority() {
    let owner = FileCapability::new(
        path("/home/project"),
        FileRights::READ | FileRights::WRITE | FileRights::DELETE,
    );

    assert!(owner
        .delegate(path("/home/project/src"), FileRights::READ)
        .is_ok());
    assert_eq!(
        owner.delegate(path("/home"), FileRights::READ),
        Err(FileSystemError::PermissionDenied),
    );
    assert_eq!(
        owner.delegate(path("/home/project"), FileRights::EXECUTE),
        Err(FileSystemError::PermissionDenied),
    );
    assert!(!owner.permits(&path("/homebrew"), FileRights::READ));
}

#[test]
fn failed_rename_is_atomic_and_explicit_replace_preserves_the_source() {
    let mut filesystem = ComputerFileSystem::testing();
    let owner = owner();
    filesystem
        .create_directory(&owner, &path("/home/source"))
        .unwrap();
    filesystem
        .create_directory(&owner, &path("/home/source/child"))
        .unwrap();
    filesystem
        .create_directory(&owner, &path("/home/destination"))
        .unwrap();
    let generation = filesystem.generation();

    assert_eq!(
        filesystem.rename(
            &owner,
            &path("/home/source"),
            &path("/home/destination"),
            false,
        ),
        Err(FileSystemError::AlreadyExists),
    );
    assert_eq!(filesystem.generation(), generation);
    assert!(filesystem.stat(&owner, &path("/home/source/child")).is_ok());

    filesystem
        .rename(
            &owner,
            &path("/home/source"),
            &path("/home/destination"),
            true,
        )
        .unwrap();
    assert!(filesystem.stat(&owner, &path("/home/source")).is_err());
    assert!(filesystem
        .stat(&owner, &path("/home/destination/child"))
        .is_ok());
    assert_eq!(filesystem.generation(), generation + 1);
}

#[test]
fn non_empty_directories_cannot_be_removed() {
    let mut filesystem = ComputerFileSystem::testing();
    let owner = owner();
    filesystem
        .create_directory(&owner, &path("/home/a"))
        .unwrap();
    filesystem
        .create_directory(&owner, &path("/home/a/b"))
        .unwrap();
    let generation = filesystem.generation();

    assert_eq!(
        filesystem.remove(&owner, &path("/home/a")),
        Err(FileSystemError::NotEmpty),
    );
    assert_eq!(filesystem.generation(), generation);
}

#[test]
fn stale_handle_never_targets_a_reused_slot() {
    let mut handles = HandleTable::new(1);
    let first = handles.open(OpenFile::testing()).unwrap();
    handles.close(first).unwrap();
    let second = handles.open(OpenFile::testing()).unwrap();

    assert_ne!(first, second);
    assert_eq!(handles.get(first), Err(FileSystemError::StaleHandle));
    assert!(handles.get(second).is_ok());
}
