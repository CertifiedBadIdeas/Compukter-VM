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

use super::{FileCapability, FileRights, FileSystemError, VirtualPath};

type Directory = BTreeMap<Box<str>, Node>;

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
    #[allow(dead_code)] // Populated by the byte-I/O layer in the next filesystem stage.
    File,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mount {
    Rom,
    Home,
}

/// One computer's deterministic in-memory namespace.
#[derive(Debug)]
pub struct ComputerFileSystem {
    rom: Directory,
    home: Directory,
    generation: u64,
}

impl ComputerFileSystem {
    #[doc(hidden)]
    pub fn testing() -> Self {
        Self {
            rom: BTreeMap::new(),
            home: BTreeMap::new(),
            generation: 0,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
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
                NodeContents::File => return Err(FileSystemError::NotDirectory),
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
        let generation = self.next_generation()?;
        let parent = find_directory_mut(self.directory_mut(mount), parent_components)?;
        if parent.contains_key(name) {
            return Err(FileSystemError::AlreadyExists);
        }
        parent.insert(Box::from(name), Node::directory(generation));
        self.generation = generation;
        Ok(())
    }

    pub fn remove(
        &mut self,
        capability: &FileCapability,
        path: &VirtualPath,
    ) -> Result<(), FileSystemError> {
        let (mount, components) = mutable_target(capability, path, FileRights::DELETE)?;
        let (name, parent_components) = split_name(components)?;
        let parent = find_directory_mut(self.directory_mut(mount), parent_components)?;
        let node = parent.get(name).ok_or(FileSystemError::NotFound)?;
        if matches!(&node.contents, NodeContents::Directory(children) if !children.is_empty()) {
            return Err(FileSystemError::NotEmpty);
        }
        parent.remove(name);
        self.generation = self.next_generation()?;
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
        let existing = find_directory(directory, destination_parent)?.get(destination_name);
        if let Some(existing) = existing {
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
        }

        let generation = self.next_generation()?;
        let node = find_directory_mut(self.directory_mut(source_mount), source_parent)?
            .remove(source_name)
            .expect("source was validated before mutation");
        let destination_directory =
            find_directory_mut(self.directory_mut(destination_mount), destination_parent)
                .expect("destination parent was validated before mutation");
        destination_directory.remove(destination_name);
        destination_directory.insert(Box::from(destination_name), node);
        self.generation = generation;
        Ok(())
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
) -> Result<(Mount, &'a [Box<str>]), FileSystemError> {
    let (mount, components) = split_mount(path)?;
    if mount == Mount::Rom {
        return Err(FileSystemError::ReadOnly);
    }
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
        NodeContents::File => Err(FileSystemError::NotDirectory),
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
            NodeContents::File => return Err(FileSystemError::NotDirectory),
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
            NodeContents::File => return Err(FileSystemError::NotDirectory),
        };
    }
    Ok(directory)
}
