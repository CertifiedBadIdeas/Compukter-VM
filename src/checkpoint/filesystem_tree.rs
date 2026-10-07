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

use super::*;
use crate::execution::checkpoint::{
    checkpoint_enum, Checkpoint as LogicalCheckpoint, CheckpointError, Reader, Result, Writer,
};
use crate::filesystem::{
    recover, Checkpoint as NamespaceCheckpoint, ComputerId, RecoveryCheckpoint, RecoveryInput,
};

impl ComputerFileSystem {
    fn checkpoint_rom_identity(&self) -> Result<[u8; 32]> {
        fn write(directory: &Directory, writer: &mut Writer) -> Result<()> {
            directory.len().write(writer)?;
            for (name, node) in directory {
                name.write(writer)?;
                node.metadata.executable.write(writer)?;
                match &node.contents {
                    NodeContents::File(object) => {
                        0_u8.write(writer)?;
                        object.write(writer)?;
                    }
                    NodeContents::Directory(directory) => {
                        1_u8.write(writer)?;
                        write(directory, writer)?;
                    }
                }
            }
            Ok(())
        }
        let mut writer = Writer::new(self.limits.maximum_checkpoint_bytes);
        write(&self.rom, &mut writer)?;
        Ok(Sha256::digest(writer.finish()).into())
    }

    fn checkpoint_home_nodes(&self) -> Result<Vec<CheckpointNode>> {
        fn visit(
            directory: &Directory,
            prefix: &str,
            limits: &FileSystemLimits,
            nodes: &mut Vec<CheckpointNode>,
        ) -> Result<()> {
            for (name, node) in directory {
                let path = VirtualPath::parse_utf8(&format!("{prefix}/{name}"), limits)
                    .map_err(|_| CheckpointError::InvalidState)?;
                nodes
                    .try_reserve(1)
                    .map_err(|_| CheckpointError::Allocation)?;
                match &node.contents {
                    NodeContents::File(object) => nodes.push(CheckpointNode::file(
                        path,
                        node.metadata.generation,
                        node.metadata.logical_size,
                        *object,
                        node.metadata.executable,
                    )),
                    NodeContents::Directory(directory) => {
                        nodes.push(CheckpointNode::directory(
                            path.clone(),
                            node.metadata.generation,
                        ));
                        visit(directory, &path.to_string(), limits, nodes)?;
                    }
                }
            }
            Ok(())
        }
        let mut nodes = Vec::new();
        visit(&self.home, "/home", &self.limits, &mut nodes)?;
        nodes.sort_unstable_by(|left, right| left.path().cmp(right.path()));
        Ok(nodes)
    }

    pub(crate) fn write_checkpoint_state(&self, id: ComputerId, writer: &mut Writer) -> Result<()> {
        self.limits.write(writer)?;
        self.checkpoint_rom_identity()?.write(writer)?;
        let nodes = self.checkpoint_home_nodes()?;
        let objects = nodes
            .iter()
            .filter_map(CheckpointNode::object)
            .map(|(object, _)| object)
            .collect::<std::collections::BTreeSet<_>>();
        let namespace = NamespaceCheckpoint::new(id, self.generation, nodes)
            .map_err(|_| CheckpointError::InvalidState)?;
        namespace
            .encode(&self.limits)
            .map_err(|_| CheckpointError::Limit)?
            .write(writer)?;
        objects.len().write(writer)?;
        for object in objects {
            object.write(writer)?;
            self.objects
                .0
                .get(&object)
                .ok_or(CheckpointError::InvalidState)?
                .bytes
                .write(writer)?;
        }
        self.handles.write(writer)?;
        self.quota.logical_bytes().write(writer)?;
        self.quota.nodes().write(writer)?;
        Ok(())
    }

    /// Rebuild logical state using a trusted ROM and persistence attachment. A
    /// live store may only attach to the exact already-published generation.
    pub(crate) fn read_checkpoint_state(
        mut expected: Self,
        id: ComputerId,
        reader: &mut Reader<'_>,
    ) -> Result<Self> {
        let limits = FileSystemLimits::read(reader)?;
        let rom: [u8; 32] = LogicalCheckpoint::read(reader)?;
        if limits != expected.limits || rom != expected.checkpoint_rom_identity()? {
            return Err(CheckpointError::Incompatible);
        }
        let namespace_bytes = reader.arc_bytes(limits.maximum_checkpoint_bytes)?;
        let namespace = NamespaceCheckpoint::decode(Arc::clone(&namespace_bytes), &limits)
            .map_err(|_| CheckpointError::InvalidState)?;
        if namespace.computer_id() != id {
            return Err(CheckpointError::Incompatible);
        }
        let needed = namespace
            .nodes()
            .iter()
            .filter_map(CheckpointNode::object)
            .map(|(object, _)| object)
            .collect::<std::collections::BTreeSet<_>>();
        let count = usize::read(reader)?;
        if count != needed.len() {
            return Err(CheckpointError::InvalidState);
        }
        reader.allocate::<(ObjectId, Arc<[u8]>)>(count)?;
        let mut objects = BTreeMap::new();
        for wanted in needed {
            let object: ObjectId = LogicalCheckpoint::read(reader)?;
            let bytes = reader.arc_bytes(
                usize::try_from(limits.maximum_file_bytes).map_err(|_| CheckpointError::Limit)?,
            )?;
            if object != wanted
                || object_id(&bytes) != object
                || bytes.len() as u64 > limits.maximum_file_bytes
            {
                return Err(CheckpointError::Integrity);
            }
            objects.insert(object, bytes);
        }
        let handles = HandleTable::read(reader)?;
        handles.validate_checkpoint(&limits)?;
        let logical_bytes = u64::read(reader)?;
        let nodes = u32::read(reader)?;
        let input = RecoveryInput::new(id, namespace.generation()).with_checkpoint(
            RecoveryCheckpoint::new(namespace.generation(), namespace_bytes),
        );
        let recovered = recover(&input, &limits).map_err(|_| CheckpointError::InvalidState)?;
        let mut filesystem = Self::with_limits(limits);
        filesystem.rom = expected.rom.clone();
        fn copy_rom(
            directory: &Directory,
            source: &ObjectStore,
            target: &mut ObjectStore,
        ) -> Result<()> {
            for node in directory.values() {
                match &node.contents {
                    NodeContents::File(object) => target.replace(
                        None,
                        *object,
                        Arc::clone(
                            &source
                                .0
                                .get(object)
                                .ok_or(CheckpointError::InvalidState)?
                                .bytes,
                        ),
                    ),
                    NodeContents::Directory(directory) => copy_rom(directory, source, target)?,
                }
            }
            Ok(())
        }
        copy_rom(&expected.rom, &expected.objects, &mut filesystem.objects)?;
        filesystem
            .restore_recovered(&recovered, &objects)
            .map_err(|_| CheckpointError::InvalidState)?;
        if filesystem.quota.logical_bytes() != logical_bytes
            || filesystem.quota.nodes() != nodes
            || expected.persistence.is_some()
                && (filesystem.generation != expected.generation
                    || filesystem.home != expected.home)
        {
            return Err(CheckpointError::Incompatible);
        }
        filesystem.handles = handles;
        filesystem.persistence = expected.persistence.take();
        Ok(filesystem)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: ComputerId = ComputerId::from_bytes([7; 16]);
    fn capability(filesystem: &ComputerFileSystem) -> FileCapability {
        FileCapability::new(
            VirtualPath::parse_utf8("/home", filesystem.limits()).unwrap(),
            FileRights::OWNER,
        )
    }
    fn encoded(filesystem: &ComputerFileSystem) -> Vec<u8> {
        let mut writer = Writer::new(4 * 1024 * 1024);
        filesystem.write_checkpoint_state(ID, &mut writer).unwrap();
        writer.finish()
    }
    #[test]
    fn checkpoint_restores_namespace_generation_objects_and_handle_generations() {
        let mut original = ComputerFileSystem::testing();
        let capability = capability(&original);
        let directory = VirtualPath::parse_utf8("/home/dir", original.limits()).unwrap();
        let path = VirtualPath::parse_utf8("/home/dir/file", original.limits()).unwrap();
        original.create_directory(&capability, &directory).unwrap();
        original
            .write_file(&capability, &path, b"saved-content", false)
            .unwrap();
        let stale = original
            .open(&capability, &path, OpenMode::ReadWrite)
            .unwrap();
        original.close(stale).unwrap();
        let live = original
            .open(&capability, &path, OpenMode::ReadWrite)
            .unwrap();
        let bytes = encoded(&original);
        let mut reader = Reader::new(&bytes, 4 * 1024 * 1024, 4 * 1024 * 1024).unwrap();
        let mut restored = ComputerFileSystem::read_checkpoint_state(
            ComputerFileSystem::testing(),
            ID,
            &mut reader,
        )
        .unwrap();
        reader.finish().unwrap();
        assert_eq!(original.snapshot_for_test(), restored.snapshot_for_test());
        assert_eq!(
            Err(FileSystemError::StaleHandle),
            restored.read(stale, 0, 16)
        );
        assert_eq!(
            b"saved-content",
            restored.read(live, 0, 16).unwrap().as_slice()
        );
        for filesystem in [&mut original, &mut restored] {
            filesystem.write(live, 0, b"other").unwrap();
            filesystem.close(live).unwrap();
        }
        assert_eq!(original.snapshot_for_test(), restored.snapshot_for_test());
        assert_eq!(
            original.open(&capability, &path, OpenMode::Read).unwrap(),
            restored.open(&capability, &path, OpenMode::Read).unwrap()
        );
    }
    #[test]
    fn checkpoint_rejects_other_computer_limits_and_corrupted_object() {
        let mut original = ComputerFileSystem::testing();
        let capability = capability(&original);
        let path = VirtualPath::parse_utf8("/home/file", original.limits()).unwrap();
        original
            .write_file(&capability, &path, b"saved-content", false)
            .unwrap();
        let mut bytes = encoded(&original);
        let mut reader = Reader::new(&bytes, 4 * 1024 * 1024, 4 * 1024 * 1024).unwrap();
        assert!(matches!(
            ComputerFileSystem::read_checkpoint_state(
                ComputerFileSystem::testing(),
                ComputerId::from_bytes([8; 16]),
                &mut reader
            ),
            Err(CheckpointError::Incompatible)
        ));
        let mut other = *original.limits();
        other.maximum_open_handles += 1;
        let mut reader = Reader::new(&bytes, 4 * 1024 * 1024, 4 * 1024 * 1024).unwrap();
        assert!(matches!(
            ComputerFileSystem::read_checkpoint_state(
                ComputerFileSystem::with_limits(other),
                ID,
                &mut reader
            ),
            Err(CheckpointError::Incompatible)
        ));
        let offset = bytes
            .windows(b"saved-content".len())
            .position(|value| value == b"saved-content")
            .unwrap();
        bytes[offset] ^= 1;
        let mut reader = Reader::new(&bytes, 4 * 1024 * 1024, 4 * 1024 * 1024).unwrap();
        assert!(matches!(
            ComputerFileSystem::read_checkpoint_state(
                ComputerFileSystem::testing(),
                ID,
                &mut reader
            ),
            Err(CheckpointError::Integrity)
        ));
    }
    #[test]
    fn checkpoint_retains_maximum_depth_paths_without_recursive_wire_decoding() {
        let mut original = ComputerFileSystem::with_limits(FileSystemLimits::default());
        let capability = capability(&original);
        let mut path = String::from("/home");
        for _ in 1..original.limits().maximum_components {
            path.push_str("/a");
            original
                .create_directory(
                    &capability,
                    &VirtualPath::parse_utf8(&path, original.limits()).unwrap(),
                )
                .unwrap();
        }
        let bytes = encoded(&original);
        let mut reader = Reader::new(&bytes, 4 * 1024 * 1024, 4 * 1024 * 1024).unwrap();
        let restored = ComputerFileSystem::read_checkpoint_state(
            ComputerFileSystem::with_limits(FileSystemLimits::default()),
            ID,
            &mut reader,
        )
        .unwrap();
        reader.finish().unwrap();
        assert_eq!(original.snapshot_for_test(), restored.snapshot_for_test());
    }

    #[test]
    fn checkpoint_reopens_persistent_generation_and_rejects_newer_files() {
        use crate::WorldFileSystemStore;
        let limits = FileSystemLimits::testing();
        let mut rom = b"CPKTROM\0".to_vec();
        rom.extend_from_slice(&1_u16.to_le_bytes());
        rom.extend_from_slice(&0_u16.to_le_bytes());
        rom.extend_from_slice(&0_u32.to_le_bytes());
        rom.extend_from_slice(&Sha256::digest(&rom));
        let rom = Arc::new(RomImage::admit(rom.into(), &limits).unwrap());
        let root = std::env::temp_dir().join(format!(
            "compukters-hibernation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = WorldFileSystemStore::open(&root, limits).unwrap();
        let mut original = store.open_computer(ID, Arc::clone(&rom)).unwrap();
        let capability = capability(&original);
        let path = VirtualPath::parse_utf8("/home/file", &limits).unwrap();
        original
            .write_file(&capability, &path, b"saved-content", false)
            .unwrap();
        let handle = original
            .open(&capability, &path, OpenMode::ReadWrite)
            .unwrap();
        let bytes = encoded(&original);
        let generation = original.generation();
        store.flush(ID, generation).unwrap();
        drop(original);
        store.close().unwrap();
        drop(store);
        let store = WorldFileSystemStore::open(&root, limits).unwrap();
        let recovered = store.open_computer(ID, Arc::clone(&rom)).unwrap();
        let mut reader = Reader::new(&bytes, 4 * 1024 * 1024, 4 * 1024 * 1024).unwrap();
        let mut restored =
            ComputerFileSystem::read_checkpoint_state(recovered, ID, &mut reader).unwrap();
        reader.finish().unwrap();
        assert_eq!(generation, restored.generation());
        assert_eq!(
            b"saved-content",
            restored.read(handle, 0, 32).unwrap().as_slice()
        );
        restored.write(handle, 0, b"newer").unwrap();
        store.flush(ID, restored.generation()).unwrap();
        drop(restored);
        store.close().unwrap();
        drop(store);
        let store = WorldFileSystemStore::open(&root, limits).unwrap();
        let recovered = store.open_computer(ID, rom).unwrap();
        let mut reader = Reader::new(&bytes, 4 * 1024 * 1024, 4 * 1024 * 1024).unwrap();
        assert!(matches!(
            ComputerFileSystem::read_checkpoint_state(recovered, ID, &mut reader),
            Err(CheckpointError::Incompatible)
        ));
        store.close().unwrap();
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn checkpoint_retains_handles_to_deleted_paths_as_catchable_failures() {
        let mut original = ComputerFileSystem::testing();
        let capability = capability(&original);
        let path = VirtualPath::parse_utf8("/home/file", original.limits()).unwrap();
        original
            .write_file(&capability, &path, b"value", false)
            .unwrap();
        let handle = original.open(&capability, &path, OpenMode::Read).unwrap();
        original.remove(&capability, &path).unwrap();
        let bytes = encoded(&original);
        let mut reader = Reader::new(&bytes, 4 * 1024 * 1024, 4 * 1024 * 1024).unwrap();
        let restored = ComputerFileSystem::read_checkpoint_state(
            ComputerFileSystem::testing(),
            ID,
            &mut reader,
        )
        .unwrap();
        reader.finish().unwrap();
        assert_eq!(original.read(handle, 0, 8), restored.read(handle, 0, 8));
        assert_eq!(Err(FileSystemError::NotFound), restored.read(handle, 0, 8));
    }
}

checkpoint_enum!(ExecutableRevision { 0 => Absent; 1 => Present(v0); });
