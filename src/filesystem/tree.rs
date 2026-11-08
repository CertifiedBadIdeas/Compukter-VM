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

use sha2::{Digest, Sha256};

use super::quota::{MutationCost, QuotaLedger};
use super::system_image::SystemEntryKind;
use super::worker::{ComputerPersistence, PersistenceMutation};
use super::{
    CheckpointNode, FileCapability, FileHandle, FileRights, FileSystemError, FileSystemLimits,
    HandleTable, JournalOperation, OpenFile, OpenMode, RecoveredState, SystemImage,
    SystemImageError, VirtualPath,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutableRevision {
    Absent,
    Present(u64),
}

impl ExecutableRevision {
    pub const fn generation(self) -> Option<u64> {
        match self {
            Self::Absent => None,
            Self::Present(generation) => Some(generation),
        }
    }
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
    bytes: crate::shared_bytes::SharedBytes,
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
            bytes: crate::shared_bytes::SharedBytes::new(bytes),
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

#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSystemSnapshot {
    root: Directory,
    objects: ObjectStore,
    quota: QuotaLedger,
    generation: u64,
}

/// One computer's deterministic in-memory namespace and logical object store.
#[derive(Debug)]
pub struct ComputerFileSystem {
    root: Directory,
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
            root: BTreeMap::from([(Box::from("home"), Node::directory(0))]),
            handles: HandleTable::new(limits.maximum_open_handles as usize),
            objects: ObjectStore::default(),
            quota: QuotaLedger::new(&limits, 1),
            limits,
            generation: 0,
            persistence: None,
        }
    }

    pub const fn limits(&self) -> &FileSystemLimits {
        &self.limits
    }

    pub fn with_system_image(
        limits: FileSystemLimits,
        image: SystemImage,
    ) -> Result<Self, SystemImageError> {
        let mut filesystem = Self::with_limits(limits);
        filesystem
            .install_system(&image)
            .map_err(|_| SystemImageError::LimitExceeded)?;
        Ok(filesystem)
    }

    pub(crate) fn empty(limits: FileSystemLimits) -> Self {
        let mut filesystem = Self::with_limits(limits);
        filesystem.root.clear();
        filesystem.quota = QuotaLedger::new(&limits, 0);
        filesystem
    }

    /// Reinstall packaged paths as one transaction. Other paths, including
    /// /home, retain their contents. Existing readers/processes pin old bytes.
    pub fn install_system(&mut self, image: &SystemImage) -> Result<(), FileSystemError> {
        let generation = self.next_generation()?;
        let mut staged = Self::empty(self.limits);
        staged.root = self.root.clone();
        staged.objects = self.objects.clone();
        staged.quota = self.quota.clone();
        staged.generation = self.generation;
        let owner = FileCapability::new(
            VirtualPath::parse_utf8("/", &self.limits)?,
            FileRights::OWNER,
        );
        let mut operations = Vec::new();
        let mut objects = BTreeMap::new();
        for text in ["/bin", "/boot", "/home"] {
            let path = VirtualPath::parse_utf8(text, &self.limits)?;
            match staged.stat(&owner, &path) {
                Ok(metadata) if metadata.kind == NodeKind::Directory => {}
                Ok(_) => return Err(FileSystemError::NotDirectory),
                Err(FileSystemError::NotFound) => {
                    staged.create_directory(&owner, &path)?;
                    find_node_mut(&mut staged.root, path.component_slice())?
                        .metadata
                        .generation = generation;
                    operations.push(JournalOperation::create_directory(path, generation));
                }
                Err(error) => return Err(error),
            }
        }
        for entry in image.entries() {
            match entry.kind {
                SystemEntryKind::Directory => match staged.stat(&owner, &entry.path) {
                    Ok(metadata) if metadata.kind == NodeKind::Directory => {}
                    Ok(_) => return Err(FileSystemError::NotDirectory),
                    Err(FileSystemError::NotFound) => {
                        staged.create_directory(&owner, &entry.path)?;
                        find_node_mut(&mut staged.root, entry.path.component_slice())?
                            .metadata
                            .generation = generation;
                        operations.push(JournalOperation::create_directory(
                            entry.path.clone(),
                            generation,
                        ));
                    }
                    Err(error) => return Err(error),
                },
                SystemEntryKind::File => {
                    staged.write_shared_file(
                        &owner,
                        &entry.path,
                        Arc::clone(&entry.content),
                        entry.executable,
                    )?;
                    find_node_mut(&mut staged.root, entry.path.component_slice())?
                        .metadata
                        .generation = generation;
                    let object = object_id(&entry.content);
                    // Queue the canonical shared bytes, not the image's input copy.
                    let bytes = staged.read_snapshot(&owner, &entry.path)?;
                    objects.insert(object, Arc::clone(&bytes));
                    operations.push(JournalOperation::put_file(
                        entry.path.clone(),
                        generation,
                        entry.content.len() as u64,
                        object,
                        entry.executable,
                    ));
                }
            }
        }
        if operations.is_empty() {
            return Ok(());
        }
        let operation = JournalOperation::Batch { operations };
        let persistence = self
            .persistence
            .as_ref()
            .map(|p| p.prepare_batch(generation, objects.into_iter().collect(), operation))
            .transpose()?;
        self.root = staged.root;
        self.objects = staged.objects;
        self.quota = staged.quota;
        self.generation = generation;
        if let Some(persistence) = persistence {
            persistence.publish();
        }
        Ok(())
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
        self.root.clear();
        self.objects = ObjectStore::default();
        self.quota = QuotaLedger::new(&self.limits, 0);
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
            let components = node.path().component_slice();
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
            find_directory_mut(&mut self.root, parent)?.insert(Box::from(name), restored);
        }
        self.quota.commit(reservation);
        self.generation = recovered.generation();
        Ok(())
    }

    pub fn logical_bytes(&self) -> u64 {
        self.quota.logical_bytes()
    }

    pub fn node_count(&self) -> u32 {
        self.quota.nodes()
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
        if path.components().len() == 0 {
            return Ok(Node::directory(0).metadata);
        }
        let components = path.component_slice();
        if components.is_empty() {
            return Ok(Node::directory(0).metadata);
        }
        Ok(find_node(&self.root, components)?.metadata.clone())
    }

    pub fn list(
        &self,
        capability: &FileCapability,
        path: &VirtualPath,
    ) -> Result<Vec<Box<str>>, FileSystemError> {
        require(capability, path, FileRights::LIST)?;
        if path.components().len() == 0 {
            return Ok(self.root.keys().cloned().collect());
        }
        let components = path.component_slice();
        let directory = if components.is_empty() {
            &self.root
        } else {
            match &find_node(&self.root, components)?.contents {
                NodeContents::Directory(directory) => directory,
                NodeContents::File(_) => return Err(FileSystemError::NotDirectory),
            }
        };
        Ok(directory.keys().cloned().collect())
    }

    /// Pins the exact immutable contents observed at open time. Namespace
    /// mutations cannot redirect a reader to another file at the same path.
    pub(crate) fn read_snapshot(
        &self,
        capability: &FileCapability,
        path: &VirtualPath,
    ) -> Result<crate::shared_bytes::SharedBytes, FileSystemError> {
        require(capability, path, FileRights::READ)?;
        if path.components().len() == 0 {
            return Err(FileSystemError::IsDirectory);
        }
        let components = path.component_slice();
        if components.is_empty() {
            return Err(FileSystemError::IsDirectory);
        }
        let node = find_node(&self.root, components)?;
        let object = node.object_id().ok_or(FileSystemError::IsDirectory)?;
        Ok(crate::shared_bytes::SharedBytes::clone(
            &self
                .objects
                .0
                .get(&object)
                .expect("admitted file object")
                .bytes,
        ))
    }

    /// Pins executable contents without making a second artifact-sized buffer.
    pub fn read_executable(
        &mut self,
        capability: &FileCapability,
        path: &VirtualPath,
    ) -> Result<Arc<[u8]>, FileSystemError> {
        require(
            capability,
            path,
            FileRights::INSPECT | FileRights::READ | FileRights::EXECUTE,
        )?;
        let metadata = self.stat(capability, path)?;
        if metadata.kind != NodeKind::File || !metadata.executable {
            return Err(FileSystemError::NotExecutable);
        }
        if metadata.logical_size > self.limits.maximum_file_bytes {
            return Err(FileSystemError::QuotaExceeded);
        }
        // Preserve handle admission limits even though the internal snapshot
        // needs no chunked reads or temporary byte buffer.
        let handle = self.open(capability, path, OpenMode::Read)?;
        let result = self
            .read_snapshot(capability, path)
            .map(|bytes| Arc::clone(&bytes));
        self.close(handle)?;
        result
    }

    pub fn create_directory(
        &mut self,
        capability: &FileCapability,
        path: &VirtualPath,
    ) -> Result<(), FileSystemError> {
        let components = mutable_target(capability, path, FileRights::CREATE)?;
        let (name, parent_components) = split_name(components)?;
        let parent = find_directory(&self.root, parent_components)?;
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
        find_directory_mut(&mut self.root, parent_components)?
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
        self.write_shared_file(capability, path, Arc::from(bytes), executable)
    }

    fn write_shared_file(
        &mut self,
        capability: &FileCapability,
        path: &VirtualPath,
        bytes: Arc<[u8]>,
        executable: bool,
    ) -> Result<(), FileSystemError> {
        require(capability, path, FileRights::WRITE)?;
        let components = path.component_slice();
        let (name, parent_components) = split_name(components)?;
        let parent = find_directory(&self.root, parent_components)?;
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
        let object = object_id(&bytes);
        let persistence = self.prepare_persistence(
            generation,
            Some((object, Arc::clone(&bytes))),
            JournalOperation::put_file(path.clone(), generation, size, object, executable),
        )?;
        find_directory_mut(&mut self.root, parent_components)?.insert(
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

    pub(crate) fn executable_install_revision(
        &self,
        capability: &FileCapability,
        path: &VirtualPath,
    ) -> Result<ExecutableRevision, FileSystemError> {
        self.replacement_revision(capability, path)
    }

    /// Captures permissions and a destination precondition without publishing
    /// an empty file or modifying its executable flag.
    pub(crate) fn replacement_revision(
        &self,
        capability: &FileCapability,
        path: &VirtualPath,
    ) -> Result<ExecutableRevision, FileSystemError> {
        require(capability, path, FileRights::WRITE)?;
        let components = path.component_slice();
        let (name, parent_components) = split_name(components)?;
        let parent = find_directory(&self.root, parent_components)?;
        match parent.get(name) {
            Some(node) => {
                require(capability, path, FileRights::WRITE)?;
                if node.metadata.kind != NodeKind::File {
                    return Err(FileSystemError::IsDirectory);
                }
                Ok(ExecutableRevision::Present(node.metadata.generation))
            }
            None => {
                require(capability, path, FileRights::CREATE | FileRights::WRITE)?;
                self.check_directory_entry_limit(parent.len())?;
                Ok(ExecutableRevision::Absent)
            }
        }
    }

    pub(crate) fn install_executable(
        &mut self,
        capability: &FileCapability,
        path: &VirtualPath,
        bytes: &[u8],
        expected: ExecutableRevision,
    ) -> Result<(), FileSystemError> {
        self.replace_file_if_revision(capability, path, bytes, expected, true)
    }

    pub(crate) fn replace_file_if_revision(
        &mut self,
        capability: &FileCapability,
        path: &VirtualPath,
        bytes: &[u8],
        expected: ExecutableRevision,
        executable: bool,
    ) -> Result<(), FileSystemError> {
        let actual = self.replacement_revision(capability, path)?;
        if actual != expected {
            return Err(FileSystemError::Busy);
        }
        self.write_file(capability, path, bytes, executable)
    }

    pub(crate) fn copy_executable(
        &mut self,
        capability: &FileCapability,
        source: &VirtualPath,
        destination: &VirtualPath,
    ) -> Result<(), FileSystemError> {
        require(
            capability,
            source,
            FileRights::READ | FileRights::INSPECT | FileRights::EXECUTE,
        )?;
        let source_bytes = self.read_snapshot(capability, source)?;
        let source_node = find_node(&self.root, source.component_slice())?;
        if !source_node.metadata.executable {
            return Err(FileSystemError::NotExecutable);
        }
        let destination_components = destination.component_slice();
        let (name, parent_components) = split_name(destination_components)?;
        let parent = find_directory(&self.root, parent_components)?;
        if parent.contains_key(name) {
            return Err(FileSystemError::AlreadyExists);
        }

        self.write_shared_file(capability, destination, Arc::clone(&source_bytes), true)
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
        let components = path.component_slice();
        match find_node(&self.root, components)?.contents {
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
        let components = mutable_target(capability, path, FileRights::DELETE)?;
        let (name, parent_components) = split_name(components)?;
        let node = find_directory(&self.root, parent_components)?
            .get(name)
            .ok_or(FileSystemError::NotFound)?
            .clone();
        if matches!(&node.contents, NodeContents::Directory(children) if !children.is_empty()) {
            return Err(FileSystemError::NotEmpty);
        }
        let generation = self.next_generation()?;
        let persistence =
            self.prepare_persistence(generation, None, JournalOperation::remove(path.clone()))?;
        find_directory_mut(&mut self.root, parent_components)?.remove(name);
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
        let source_components = mutable_target(capability, source, FileRights::RENAME)?;
        let destination_components = mutable_target(capability, destination, FileRights::RENAME)?;
        if source == destination {
            return Ok(());
        }
        if destination.is_within(source) {
            return Err(FileSystemError::InvalidPath);
        }
        let (source_name, source_parent) = split_name(source_components)?;
        let (destination_name, destination_parent) = split_name(destination_components)?;
        let directory = &self.root;
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
        let mut node = find_directory_mut(&mut self.root, source_parent)?
            .remove(source_name)
            .expect("source was validated before mutation");
        node.metadata.generation = generation;
        let destination_directory = find_directory_mut(&mut self.root, destination_parent)
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
            root: self.root.clone(),
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
        let components = path.component_slice();
        let previous = find_node(&self.root, components)?.clone();
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
        *find_node_mut(&mut self.root, components)? =
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
        let components = path.component_slice();
        let node = find_node(&self.root, components)?;
        match node.contents {
            NodeContents::File(object) => Ok(self.objects.bytes(&object)),
            NodeContents::Directory(_) => Err(FileSystemError::IsDirectory),
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

fn mutable_target<'a>(
    capability: &FileCapability,
    path: &'a VirtualPath,
    rights: FileRights,
) -> Result<&'a [Box<str>], FileSystemError> {
    require(capability, path, rights)?;
    Ok(path.component_slice())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn path(value: &str, limits: &FileSystemLimits) -> VirtualPath {
        VirtualPath::parse_utf8(value, limits).unwrap()
    }

    #[test]
    fn independent_computers_share_contents_with_private_mutation_and_executable_snapshots() {
        let mut first = ComputerFileSystem::testing();
        let mut second = ComputerFileSystem::testing();
        let root = path("/", first.limits());
        let owner = FileCapability::new(
            root,
            FileRights::INSPECT
                | FileRights::READ
                | FileRights::WRITE
                | FileRights::CREATE
                | FileRights::EXECUTE,
        );
        let file = path("/home/shared", first.limits());
        first
            .write_file(&owner, &file, b"shared executable contents", true)
            .unwrap();
        second
            .write_file(&owner, &file, b"shared executable contents", true)
            .unwrap();
        let running = first.read_executable(&owner, &file).unwrap();
        let copied = second.read_executable(&owner, &file).unwrap();
        assert!(Arc::ptr_eq(&running, &copied));
        first.write_file(&owner, &file, b"edited", true).unwrap();
        assert_eq!(
            second.read_snapshot(&owner, &file).unwrap().as_ref(),
            running.as_ref()
        );
        assert_eq!(b"shared executable contents", running.as_ref());
        assert_eq!(
            b"edited",
            first.read_snapshot(&owner, &file).unwrap().as_ref()
        );
    }

    #[test]
    fn one_thousand_computer_files_reference_one_live_payload() {
        let file = path("/home/shared-program", &FileSystemLimits::testing());
        let owner = FileCapability::new(VirtualPath::root(), FileRights::OWNER);
        let mut computers = Vec::new();
        for _ in 0..1000 {
            let mut filesystem = ComputerFileSystem::testing();
            // Every caller supplies an independent buffer, as compiler/transfer
            // boundaries do. Only immutable storage is shared afterwards.
            filesystem
                .write_file(&owner, &file, &vec![61; 8192], true)
                .unwrap();
            computers.push(filesystem);
        }
        let canonical = computers[0].read_snapshot(&owner, &file).unwrap();
        assert!(computers
            .iter()
            .all(|fs| Arc::ptr_eq(&canonical, &fs.read_snapshot(&owner, &file).unwrap())));
        assert_eq!(
            8_192_000,
            computers
                .iter()
                .map(ComputerFileSystem::logical_bytes)
                .sum::<u64>()
        );
        assert_eq!(8192, canonical.len());
    }

    #[test]
    fn executable_install_is_atomic_for_absent_and_existing_outputs() {
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(path("/home", &limits), FileRights::OWNER);
        let output = path("/home/program", &limits);
        let mut filesystem = ComputerFileSystem::with_limits(limits);

        filesystem
            .install_executable(&owner, &output, b"first", ExecutableRevision::Absent)
            .unwrap();
        let first = filesystem.stat(&owner, &output).unwrap();
        assert!(first.executable);
        assert_eq!(
            b"first",
            filesystem.read_file_for_test(&output).unwrap().as_slice()
        );

        filesystem
            .install_executable(
                &owner,
                &output,
                b"second",
                ExecutableRevision::Present(first.generation),
            )
            .unwrap();
        let second = filesystem.stat(&owner, &output).unwrap();
        assert!(second.executable);
        assert!(second.generation > first.generation);
        assert_eq!(
            b"second",
            filesystem.read_file_for_test(&output).unwrap().as_slice()
        );
    }

    #[test]
    fn executable_install_rejects_stale_preconditions_without_mutation() {
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(path("/home", &limits), FileRights::OWNER);
        let output = path("/home/program", &limits);
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        filesystem
            .write_file(&owner, &output, b"old", false)
            .unwrap();
        let before = filesystem.snapshot_for_test();

        assert_eq!(
            filesystem.install_executable(&owner, &output, b"new", ExecutableRevision::Absent),
            Err(FileSystemError::Busy),
        );
        assert_eq!(before, filesystem.snapshot_for_test());
    }

    #[test]
    fn virtual_root_exposes_entries_only_to_authorized_guests() {
        let limits = FileSystemLimits::testing();
        let filesystem = ComputerFileSystem::with_limits(limits);
        let root = VirtualPath::root();
        let owner = FileCapability::new(root.clone(), FileRights::OWNER);
        let home = FileCapability::new(path("/home", &limits), FileRights::OWNER);
        assert_eq!(
            NodeKind::Directory,
            filesystem.stat(&owner, &root).unwrap().kind
        );
        assert_eq!(
            vec![Box::<str>::from("home")],
            filesystem.list(&owner, &root).unwrap()
        );
        assert_eq!(
            Err(FileSystemError::IsDirectory),
            filesystem.read_snapshot(&owner, &root)
        );
        assert_eq!(
            Err(FileSystemError::PermissionDenied),
            filesystem.list(&home, &root)
        );
    }

    #[test]
    fn read_snapshot_keeps_bytes_across_replacement_rename_and_path_reuse() {
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(path("/home", &limits), FileRights::OWNER);
        let file = path("/home/file", &limits);
        let moved = path("/home/moved", &limits);
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        filesystem
            .write_file(&owner, &file, b"original", false)
            .unwrap();
        let snapshot = filesystem.read_snapshot(&owner, &file).unwrap();
        filesystem
            .write_file(&owner, &file, b"replacement", false)
            .unwrap();
        filesystem.rename(&owner, &file, &moved, false).unwrap();
        filesystem
            .write_file(&owner, &file, b"recreated", false)
            .unwrap();
        filesystem.remove(&owner, &moved).unwrap();
        assert_eq!(b"original", snapshot.as_ref());
        let mut another = ComputerFileSystem::with_limits(limits);
        another
            .write_file(&owner, &file, b"original", false)
            .unwrap();
        assert!(Arc::ptr_eq(
            &snapshot,
            &another.read_snapshot(&owner, &file).unwrap()
        ));
        assert_eq!(
            b"recreated",
            filesystem.read_snapshot(&owner, &file).unwrap().as_ref()
        );
        let denied = FileCapability::new(path("/home", filesystem.limits()), FileRights::INSPECT);
        assert_eq!(
            Err(FileSystemError::PermissionDenied),
            filesystem.read_snapshot(&denied, &file)
        );
        assert_eq!(
            Err(FileSystemError::IsDirectory),
            filesystem.read_snapshot(&owner, &path("/home", filesystem.limits()))
        );
    }

    #[test]
    fn conditional_replacement_rejects_recreated_destination_and_preserves_executable_until_commit()
    {
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(path("/home", &limits), FileRights::OWNER);
        let file = path("/home/file", &limits);
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        filesystem
            .write_file(&owner, &file, b"original", true)
            .unwrap();
        let revision = filesystem.replacement_revision(&owner, &file).unwrap();
        assert!(filesystem.stat(&owner, &file).unwrap().executable);
        filesystem.remove(&owner, &file).unwrap();
        filesystem
            .write_file(&owner, &file, b"recreated", true)
            .unwrap();
        let before = filesystem.snapshot_for_test();
        assert_eq!(
            Err(FileSystemError::Busy),
            filesystem.replace_file_if_revision(&owner, &file, b"draft", revision, false)
        );
        assert_eq!(before, filesystem.snapshot_for_test());
        let current = filesystem.replacement_revision(&owner, &file).unwrap();
        filesystem
            .replace_file_if_revision(&owner, &file, b"draft", current, false)
            .unwrap();
        assert_eq!(
            b"draft",
            filesystem.read_snapshot(&owner, &file).unwrap().as_ref()
        );
        assert!(!filesystem.stat(&owner, &file).unwrap().executable);
    }
}

#[path = "../checkpoint/filesystem_tree.rs"]
#[allow(dead_code)]
mod checkpoint_state;
