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

use std::collections::BTreeMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::quota::{MutationCost, QuotaLedger};
use super::rom::RomEntryKind;
use super::worker::{ComputerPersistence, PersistenceMutation};
use super::{
    CheckpointNode, FileCapability, FileHandle, FileRights, FileSystemError, FileSystemLimits,
    HandleTable, JournalOperation, OpenFile, OpenMode, RecoveredState, RomImage, RomImageError,
    VirtualPath,
};

type Directory = BTreeMap<Box<str>, Node>;
type ObjectId = [u8; 32];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeMetadata {
    pub kind: NodeKind,
    pub logical_size: u64,
    pub generation: u64,
    pub executable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Node {
    metadata: NodeMetadata,
    contents: NodeContents,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum NodeContents {
    File(ObjectId),
    Directory(Directory),
}

impl Node {
    fn directory(generation: u64) -> Self {
        Self {
            metadata: NodeMetadata {
                kind: NodeKind::Directory,
                logical_size: 0,
                generation,
                executable: false,
            },
            contents: NodeContents::Directory(BTreeMap::new()),
        }
    }

    fn file(object: ObjectId, logical_size: u64, generation: u64, executable: bool) -> Self {
        Self {
            metadata: NodeMetadata {
                kind: NodeKind::File,
                logical_size,
                generation,
                executable,
            },
            contents: NodeContents::File(object),
        }
    }

    fn object_id(&self) -> Option<ObjectId> {
        match self.contents {
            NodeContents::File(object) => Some(object),
            NodeContents::Directory(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoredObject {
    bytes: Arc<[u8]>,
    references: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ObjectStore(BTreeMap<ObjectId, StoredObject>);

impl ObjectStore {
    fn bytes(&self, object: &ObjectId) -> &[u8] {
        self.0
            .get(object)
            .expect("every file node references an admitted object")
            .bytes
            .as_ref()
    }

    fn replace(&mut self, previous: Option<ObjectId>, next: ObjectId, bytes: Arc<[u8]>) {
        if previous == Some(next) {
            return;
        }
        let stored = self.0.entry(next).or_insert(StoredObject {
            bytes,
            references: 0,
        });
        stored.references = stored
            .references
            .checked_add(1)
            .expect("reference count is bounded by the node quota");
        if let Some(previous) = previous {
            self.remove(previous);
        }
    }

    fn remove(&mut self, object: ObjectId) {
        let remove = {
            let stored = self
                .0
                .get_mut(&object)
                .expect("every file node references an admitted object");
            stored.references -= 1;
            stored.references == 0
        };
        if remove {
            self.0.remove(&object);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mount {
    Rom,
    Home,
}

#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSystemSnapshot {
    home: Directory,
    objects: ObjectStore,
    quota: QuotaLedger,
    generation: u64,
}

/// One computer's deterministic in-memory namespace and logical object store.
#[derive(Debug)]
pub struct ComputerFileSystem {
    rom: Directory,
    home: Directory,
    handles: HandleTable,
    objects: ObjectStore,
    quota: QuotaLedger,
    limits: FileSystemLimits,
    generation: u64,
    persistence: Option<ComputerPersistence>,
}

impl ComputerFileSystem {
    pub fn with_limits(limits: FileSystemLimits) -> Self {
        Self {
            rom: BTreeMap::new(),
            home: BTreeMap::new(),
            handles: HandleTable::new(limits.maximum_open_handles as usize),
            objects: ObjectStore::default(),
            quota: QuotaLedger::new(&limits, 2),
            limits,
            generation: 0,
            persistence: None,
        }
    }

    pub const fn limits(&self) -> &FileSystemLimits {
        &self.limits
    }

    pub fn with_rom(limits: FileSystemLimits, image: RomImage) -> Result<Self, RomImageError> {
        let mut filesystem = Self::with_limits(limits);
        for entry in image.entries() {
            let components = entry.path.component_slice();
            let (_, relative) = components
                .split_first()
                .ok_or(RomImageError::NonCanonical)?;
            let (name, parent) = split_name(relative).map_err(|_| RomImageError::NonCanonical)?;
            match entry.kind {
                RomEntryKind::Directory => {
                    find_directory_mut(&mut filesystem.rom, parent)
                        .map_err(|_| RomImageError::NonCanonical)?
                        .insert(Box::from(name), Node::directory(0));
                }
                RomEntryKind::File => {
                    let object = object_id(&entry.content);
                    find_directory_mut(&mut filesystem.rom, parent)
                        .map_err(|_| RomImageError::NonCanonical)?
                        .insert(
                            Box::from(name),
                            Node::file(object, entry.content.len() as u64, 0, entry.executable),
                        );
                    filesystem
                        .objects
                        .replace(None, object, Arc::clone(&entry.content));
                }
            }
        }
        Ok(filesystem)
    }

    #[doc(hidden)]
    pub fn testing() -> Self {
        Self::with_limits(FileSystemLimits::testing())
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn attach_persistence(&mut self, persistence: ComputerPersistence) {
        self.persistence = Some(persistence);
    }

    pub(crate) fn restore_recovered(
        &mut self,
        recovered: &RecoveredState,
        objects: &BTreeMap<ObjectId, Arc<[u8]>>,
    ) -> Result<(), FileSystemError> {
        let logical_bytes = recovered.nodes().try_fold(0_u64, |total, node| {
            node.object()
                .map_or(Some(total), |(_, size)| total.checked_add(size))
        });
        let reservation = self.quota.reserve(MutationCost {
            logical_bytes_added: logical_bytes.ok_or(FileSystemError::StorageFaulted)?,
            nodes_added: u32::try_from(recovered.nodes().len())
                .map_err(|_| FileSystemError::StorageFaulted)?,
            ..MutationCost::default()
        })?;
        for node in recovered.nodes() {
            let (_, components) = writable_path(node.path())?;
            let (name, parent) = split_name(components)?;
            let restored = match node {
                CheckpointNode::Directory {
                    node_generation, ..
                } => Node::directory(*node_generation),
                CheckpointNode::File {
                    node_generation,
                    logical_size,
                    object_id,
                    executable,
                    ..
                } => {
                    let bytes = objects
                        .get(object_id)
                        .ok_or(FileSystemError::StorageFaulted)?;
                    if bytes.len() as u64 != *logical_size {
                        return Err(FileSystemError::StorageFaulted);
                    }
                    self.objects.replace(None, *object_id, Arc::clone(bytes));
                    Node::file(*object_id, *logical_size, *node_generation, *executable)
                }
            };
            find_directory_mut(&mut self.home, parent)?.insert(Box::from(name), restored);
        }
        self.quota.commit(reservation);
        self.generation = recovered.generation();
        Ok(())
    }

    pub fn logical_bytes(&self) -> u64 {
        self.quota.logical_bytes()
    }

    pub fn object_count(&self) -> usize {
        self.objects.0.len()
    }

    pub fn stat(
        &self,
        capability: &FileCapability,
        path: &VirtualPath,
    ) -> Result<NodeMetadata, FileSystemError> {
        require(capability, path, FileRights::INSPECT)?;
        let (mount, components) = split_mount(path)?;
        if components.is_empty() {
            return Ok(Node::directory(0).metadata);
        }
        Ok(find_node(self.directory(mount), components)?
            .metadata
            .clone())
    }

    pub fn list(
        &self,
        capability: &FileCapability,
        path: &VirtualPath,
    ) -> Result<Vec<Box<str>>, FileSystemError> {
        require(capability, path, FileRights::LIST)?;
        let (mount, components) = split_mount(path)?;
        let directory = if components.is_empty() {
            self.directory(mount)
        } else {
            match &find_node(self.directory(mount), components)?.contents {
                NodeContents::Directory(directory) => directory,
                NodeContents::File(_) => return Err(FileSystemError::NotDirectory),
            }
        };
        Ok(directory.keys().cloned().collect())
    }

    pub fn create_directory(
        &mut self,
        capability: &FileCapability,
        path: &VirtualPath,
    ) -> Result<(), FileSystemError> {
        let (mount, components) = mutable_target(capability, path, FileRights::CREATE)?;
        let (name, parent_components) = split_name(components)?;
        let parent = find_directory(self.directory(mount), parent_components)?;
        if parent.contains_key(name) {
            return Err(FileSystemError::AlreadyExists);
        }
        self.check_directory_entry_limit(parent.len())?;
        let reservation = self.quota.reserve(MutationCost {
            nodes_added: 1,
            ..MutationCost::default()
        })?;
        let generation = self.next_generation()?;
        let persistence = self.prepare_persistence(
            generation,
            None,
            JournalOperation::create_directory(path.clone(), generation),
        )?;
        find_directory_mut(self.directory_mut(mount), parent_components)?
            .insert(Box::from(name), Node::directory(generation));
        self.quota.commit(reservation);
        self.generation = generation;
        if let Some(persistence) = persistence {
            persistence.publish();
        }
        Ok(())
    }

    pub fn write_file(
        &mut self,
        capability: &FileCapability,
        path: &VirtualPath,
        bytes: &[u8],
        executable: bool,
    ) -> Result<(), FileSystemError> {
        let (mount, components) = writable_path(path)?;
        let (name, parent_components) = split_name(components)?;
        let parent = find_directory(self.directory(mount), parent_components)?;
        let previous = parent.get(name).cloned();
        if previous.is_some() {
            require(capability, path, FileRights::WRITE)?;
        } else {
            require(capability, path, FileRights::CREATE | FileRights::WRITE)?;
            self.check_directory_entry_limit(parent.len())?;
        }
        if matches!(
            previous.as_ref().map(|node| &node.contents),
            Some(NodeContents::Directory(_))
        ) {
            return Err(FileSystemError::IsDirectory);
        }
        let size = u64::try_from(bytes.len()).map_err(|_| FileSystemError::QuotaExceeded)?;
        self.check_file_size(size)?;
        let previous_size = previous
            .as_ref()
            .map_or(0, |node| node.metadata.logical_size);
        let reservation = self.quota.reserve(MutationCost {
            logical_bytes_added: size.saturating_sub(previous_size),
            nodes_added: u32::from(previous.is_none()),
            ..MutationCost::default()
        })?;
        let generation = self.next_generation()?;
        let bytes: Arc<[u8]> = Arc::from(bytes);
        let object = object_id(&bytes);
        let persistence = self.prepare_persistence(
            generation,
            Some((object, Arc::clone(&bytes))),
            JournalOperation::put_file(path.clone(), generation, size, object, executable),
        )?;
        find_directory_mut(self.directory_mut(mount), parent_components)?.insert(
            Box::from(name),
            Node::file(object, size, generation, executable),
        );
        self.objects
            .replace(previous.as_ref().and_then(Node::object_id), object, bytes);
        self.quota.commit(reservation);
        self.quota.release(previous_size.saturating_sub(size), 0);
        self.generation = generation;
        if let Some(persistence) = persistence {
            persistence.publish();
        }
        Ok(())
    }

    pub fn open(
        &mut self,
        capability: &FileCapability,
        path: &VirtualPath,
        mode: OpenMode,
    ) -> Result<FileHandle, FileSystemError> {
        let mut rights = FileRights::INSPECT;
        if mode.readable() {
            rights |= FileRights::READ;
        }
        if mode.writable() {
            rights |= FileRights::WRITE;
        }
        require(capability, path, rights)?;
        let (mount, components) = split_mount(path)?;
        match find_node(self.directory(mount), components)?.contents {
            NodeContents::File(_) => {}
            NodeContents::Directory(_) => return Err(FileSystemError::IsDirectory),
        }
        if self.handles.open_count() >= capability.handle_limit() as usize {
            return Err(FileSystemError::QuotaExceeded);
        }
        self.handles.open(OpenFile::new(path.clone(), mode))
    }

    pub fn read(
        &self,
        handle: FileHandle,
        offset: u64,
        maximum_bytes: usize,
    ) -> Result<Vec<u8>, FileSystemError> {
        let open = self.handles.get(handle)?;
        if !open.mode().readable() {
            return Err(FileSystemError::PermissionDenied);
        }
        let bytes = self.file_bytes(open.path())?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let accepted = maximum_bytes.min(self.limits.maximum_io_bytes);
        let end = start.saturating_add(accepted).min(bytes.len());
        Ok(bytes[start..end].to_vec())
    }

    pub fn write(
        &mut self,
        handle: FileHandle,
        offset: u64,
        bytes: &[u8],
    ) -> Result<usize, FileSystemError> {
        let open = self.handles.get(handle)?.clone();
        if !open.mode().writable() {
            return Err(FileSystemError::PermissionDenied);
        }
        let accepted = bytes.len().min(self.limits.maximum_io_bytes);
        if accepted == 0 {
            return Ok(0);
        }
        let accepted_u64 = u64::try_from(accepted).map_err(|_| FileSystemError::QuotaExceeded)?;
        let end = offset
            .checked_add(accepted_u64)
            .ok_or(FileSystemError::QuotaExceeded)?;
        self.check_file_size(end)?;
        let end = usize::try_from(end).map_err(|_| FileSystemError::QuotaExceeded)?;
        let offset = usize::try_from(offset).map_err(|_| FileSystemError::QuotaExceeded)?;
        let mut replacement = self.file_bytes(open.path())?.to_vec();
        replacement.resize(replacement.len().max(end), 0);
        replacement[offset..end].copy_from_slice(&bytes[..accepted]);
        self.replace_existing(open.path(), replacement)?;
        Ok(accepted)
    }

    pub fn truncate(&mut self, handle: FileHandle, size: u64) -> Result<(), FileSystemError> {
        let open = self.handles.get(handle)?.clone();
        if !open.mode().writable() {
            return Err(FileSystemError::PermissionDenied);
        }
        self.check_file_size(size)?;
        let size = usize::try_from(size).map_err(|_| FileSystemError::QuotaExceeded)?;
        let mut replacement = self.file_bytes(open.path())?.to_vec();
        if replacement.len() == size {
            return Ok(());
        }
        replacement.resize(size, 0);
        self.replace_existing(open.path(), replacement)
    }

    pub fn close(&mut self, handle: FileHandle) -> Result<(), FileSystemError> {
        self.handles.close(handle)
    }

    pub fn remove(
        &mut self,
        capability: &FileCapability,
        path: &VirtualPath,
    ) -> Result<(), FileSystemError> {
        let (mount, components) = mutable_target(capability, path, FileRights::DELETE)?;
        let (name, parent_components) = split_name(components)?;
        let node = find_directory(self.directory(mount), parent_components)?
            .get(name)
            .ok_or(FileSystemError::NotFound)?
            .clone();
        if matches!(&node.contents, NodeContents::Directory(children) if !children.is_empty()) {
            return Err(FileSystemError::NotEmpty);
        }
        let generation = self.next_generation()?;
        let persistence =
            self.prepare_persistence(generation, None, JournalOperation::remove(path.clone()))?;
        find_directory_mut(self.directory_mut(mount), parent_components)?.remove(name);
        if let Some(object) = node.object_id() {
            self.objects.remove(object);
        }
        self.quota.release(node.metadata.logical_size, 1);
        self.generation = generation;
        if let Some(persistence) = persistence {
            persistence.publish();
        }
        Ok(())
    }

    pub fn rename(
        &mut self,
        capability: &FileCapability,
        source: &VirtualPath,
        destination: &VirtualPath,
        replace: bool,
    ) -> Result<(), FileSystemError> {
        let (source_mount, source_components) =
            mutable_target(capability, source, FileRights::RENAME)?;
        let (destination_mount, destination_components) =
            mutable_target(capability, destination, FileRights::RENAME)?;
        if source_mount != destination_mount {
            return Err(FileSystemError::PermissionDenied);
        }
        if source == destination {
            return Ok(());
        }
        if destination.is_within(source) {
            return Err(FileSystemError::InvalidPath);
        }
        let (source_name, source_parent) = split_name(source_components)?;
        let (destination_name, destination_parent) = split_name(destination_components)?;
        let directory = self.directory(source_mount);
        let source_node = find_directory(directory, source_parent)?
            .get(source_name)
            .ok_or(FileSystemError::NotFound)?;
        let existing = find_directory(directory, destination_parent)?
            .get(destination_name)
            .cloned();
        if let Some(existing) = &existing {
            if !replace {
                return Err(FileSystemError::AlreadyExists);
            }
            if matches!(&existing.contents, NodeContents::Directory(children) if !children.is_empty())
            {
                return Err(FileSystemError::NotEmpty);
            }
            if existing.metadata.kind != source_node.metadata.kind {
                return Err(match existing.metadata.kind {
                    NodeKind::Directory => FileSystemError::IsDirectory,
                    NodeKind::File => FileSystemError::NotDirectory,
                });
            }
        } else {
            self.check_directory_entry_limit(find_directory(directory, destination_parent)?.len())?;
        }
        let generation = self.next_generation()?;
        let persistence = self.prepare_persistence(
            generation,
            None,
            JournalOperation::rename(source.clone(), destination.clone(), replace),
        )?;
        let mut node = find_directory_mut(self.directory_mut(source_mount), source_parent)?
            .remove(source_name)
            .expect("source was validated before mutation");
        node.metadata.generation = generation;
        let destination_directory =
            find_directory_mut(self.directory_mut(destination_mount), destination_parent)
                .expect("destination parent was validated before mutation");
        destination_directory.remove(destination_name);
        destination_directory.insert(Box::from(destination_name), node);
        if let Some(existing) = existing {
            if let Some(object) = existing.object_id() {
                self.objects.remove(object);
            }
            self.quota.release(existing.metadata.logical_size, 1);
        }
        self.generation = generation;
        if let Some(persistence) = persistence {
            persistence.publish();
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn snapshot_for_test(&self) -> FileSystemSnapshot {
        FileSystemSnapshot {
            home: self.home.clone(),
            objects: self.objects.clone(),
            quota: self.quota.clone(),
            generation: self.generation,
        }
    }

    #[doc(hidden)]
    pub fn read_file_for_test(&self, path: &VirtualPath) -> Result<Vec<u8>, FileSystemError> {
        Ok(self.file_bytes(path)?.to_vec())
    }

    fn replace_existing(
        &mut self,
        path: &VirtualPath,
        bytes: Vec<u8>,
    ) -> Result<(), FileSystemError> {
        let (mount, components) = writable_path(path)?;
        let previous = find_node(self.directory(mount), components)?.clone();
        let previous_object = previous.object_id().ok_or(FileSystemError::IsDirectory)?;
        let size = u64::try_from(bytes.len()).map_err(|_| FileSystemError::QuotaExceeded)?;
        self.check_file_size(size)?;
        let reservation = self.quota.reserve(MutationCost {
            logical_bytes_added: size.saturating_sub(previous.metadata.logical_size),
            ..MutationCost::default()
        })?;
        let generation = self.next_generation()?;
        let bytes: Arc<[u8]> = bytes.into();
        let object = object_id(&bytes);
        let persistence = self.prepare_persistence(
            generation,
            Some((object, Arc::clone(&bytes))),
            JournalOperation::put_file(
                path.clone(),
                generation,
                size,
                object,
                previous.metadata.executable,
            ),
        )?;
        *find_node_mut(self.directory_mut(mount), components)? =
            Node::file(object, size, generation, previous.metadata.executable);
        self.objects.replace(Some(previous_object), object, bytes);
        self.quota.commit(reservation);
        self.quota
            .release(previous.metadata.logical_size.saturating_sub(size), 0);
        self.generation = generation;
        if let Some(persistence) = persistence {
            persistence.publish();
        }
        Ok(())
    }

    fn file_bytes(&self, path: &VirtualPath) -> Result<&[u8], FileSystemError> {
        let (mount, components) = split_mount(path)?;
        let node = find_node(self.directory(mount), components)?;
        match node.contents {
            NodeContents::File(object) => Ok(self.objects.bytes(&object)),
            NodeContents::Directory(_) => Err(FileSystemError::IsDirectory),
        }
    }

    fn directory(&self, mount: Mount) -> &Directory {
        match mount {
            Mount::Rom => &self.rom,
            Mount::Home => &self.home,
        }
    }

    fn directory_mut(&mut self, mount: Mount) -> &mut Directory {
        match mount {
            Mount::Rom => &mut self.rom,
            Mount::Home => &mut self.home,
        }
    }

    fn next_generation(&self) -> Result<u64, FileSystemError> {
        self.generation
            .checked_add(1)
            .ok_or(FileSystemError::StorageFaulted)
    }

    fn check_file_size(&self, size: u64) -> Result<(), FileSystemError> {
        (size <= self.limits.maximum_file_bytes)
            .then_some(())
            .ok_or(FileSystemError::QuotaExceeded)
    }

    fn check_directory_entry_limit(&self, current: usize) -> Result<(), FileSystemError> {
        (current < self.limits.maximum_directory_entries as usize)
            .then_some(())
            .ok_or(FileSystemError::QuotaExceeded)
    }

    fn prepare_persistence(
        &self,
        generation: u64,
        object: Option<(ObjectId, Arc<[u8]>)>,
        operation: JournalOperation,
    ) -> Result<Option<PersistenceMutation>, FileSystemError> {
        self.persistence
            .as_ref()
            .map(|persistence| persistence.prepare(generation, object, operation))
            .transpose()
    }
}

fn object_id(bytes: &[u8]) -> ObjectId {
    Sha256::digest(bytes).into()
}

fn require(
    capability: &FileCapability,
    path: &VirtualPath,
    rights: FileRights,
) -> Result<(), FileSystemError> {
    capability
        .permits(path, rights)
        .then_some(())
        .ok_or(FileSystemError::PermissionDenied)
}

fn writable_path(path: &VirtualPath) -> Result<(Mount, &[Box<str>]), FileSystemError> {
    let (mount, components) = split_mount(path)?;
    if mount == Mount::Rom {
        return Err(FileSystemError::ReadOnly);
    }
    Ok((mount, components))
}

fn mutable_target<'a>(
    capability: &FileCapability,
    path: &'a VirtualPath,
    rights: FileRights,
) -> Result<(Mount, &'a [Box<str>]), FileSystemError> {
    let (mount, components) = writable_path(path)?;
    require(capability, path, rights)?;
    Ok((mount, components))
}

fn split_mount(path: &VirtualPath) -> Result<(Mount, &[Box<str>]), FileSystemError> {
    let components = path.component_slice();
    let (mount, rest) = components
        .split_first()
        .ok_or(FileSystemError::PermissionDenied)?;
    match mount.as_ref() {
        "rom" => Ok((Mount::Rom, rest)),
        "home" => Ok((Mount::Home, rest)),
        _ => Err(FileSystemError::NotFound),
    }
}

fn split_name(components: &[Box<str>]) -> Result<(&str, &[Box<str>]), FileSystemError> {
    let (name, parent) = components
        .split_last()
        .ok_or(FileSystemError::PermissionDenied)?;
    Ok((name, parent))
}

fn find_node<'a>(
    directory: &'a Directory,
    components: &[Box<str>],
) -> Result<&'a Node, FileSystemError> {
    let (first, rest) = components.split_first().ok_or(FileSystemError::NotFound)?;
    let node = directory
        .get(first.as_ref())
        .ok_or(FileSystemError::NotFound)?;
    if rest.is_empty() {
        return Ok(node);
    }
    match &node.contents {
        NodeContents::Directory(children) => find_node(children, rest),
        NodeContents::File(_) => Err(FileSystemError::NotDirectory),
    }
}

fn find_node_mut<'a>(
    directory: &'a mut Directory,
    components: &[Box<str>],
) -> Result<&'a mut Node, FileSystemError> {
    let (first, rest) = components.split_first().ok_or(FileSystemError::NotFound)?;
    let node = directory
        .get_mut(first.as_ref())
        .ok_or(FileSystemError::NotFound)?;
    if rest.is_empty() {
        return Ok(node);
    }
    match &mut node.contents {
        NodeContents::Directory(children) => find_node_mut(children, rest),
        NodeContents::File(_) => Err(FileSystemError::NotDirectory),
    }
}

fn find_directory<'a>(
    mut directory: &'a Directory,
    components: &[Box<str>],
) -> Result<&'a Directory, FileSystemError> {
    for component in components {
        let node = directory
            .get(component.as_ref())
            .ok_or(FileSystemError::NotFound)?;
        directory = match &node.contents {
            NodeContents::Directory(children) => children,
            NodeContents::File(_) => return Err(FileSystemError::NotDirectory),
        };
    }
    Ok(directory)
}

fn find_directory_mut<'a>(
    mut directory: &'a mut Directory,
    components: &[Box<str>],
) -> Result<&'a mut Directory, FileSystemError> {
    for component in components {
        let node = directory
            .get_mut(component.as_ref())
            .ok_or(FileSystemError::NotFound)?;
        directory = match &mut node.contents {
            NodeContents::Directory(children) => children,
            NodeContents::File(_) => return Err(FileSystemError::NotDirectory),
        };
    }
    Ok(directory)
}
