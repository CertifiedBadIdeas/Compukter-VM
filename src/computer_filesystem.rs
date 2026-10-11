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

//! Process-owned immutable reads and atomic replacement drafts.
use super::*;
use crate::execution::checkpoint::{Checkpoint, CheckpointError, Reader, Writer};
use crate::filesystem::NodeMetadata;

const CHUNK_BYTES: usize = 4095;

#[derive(Debug)]
pub(super) struct FileTransfers {
    next_id: i32,
    resources: Vec<(i32, FileTransfer)>,
}

impl Default for FileTransfers {
    fn default() -> Self {
        Self {
            next_id: 1,
            resources: Vec::new(),
        }
    }
}

#[derive(Debug)]
enum FileTransfer {
    Read {
        path: VirtualPath,
        bytes: Arc<[u8]>,
        position: usize,
    },
    Write {
        path: VirtualPath,
        revision: ExecutableRevision,
        bytes: Vec<u8>,
        poisoned: bool,
    },
    Directory {
        path: VirtualPath,
        names: Vec<Box<str>>,
        position: usize,
    },
    Move {
        path: VirtualPath,
        generation: u64,
    },
}

impl FileTransfer {
    fn retained_bytes(&self) -> u64 {
        match self {
            Self::Read { bytes, .. } => bytes.len() as u64,
            Self::Write { bytes, .. } => bytes.capacity() as u64,
            Self::Directory { names, .. } => {
                names.iter().map(|n| n.len() as u64).sum::<u64>()
                    + (names.capacity() * std::mem::size_of::<Box<str>>()) as u64
            }
            Self::Move { .. } => 0,
        }
    }
    fn path(&self) -> &VirtualPath {
        match self {
            Self::Read { path, .. }
            | Self::Write { path, .. }
            | Self::Directory { path, .. }
            | Self::Move { path, .. } => path,
        }
    }
}

impl FileTransfers {
    fn insert(&mut self, resource: FileTransfer) -> Result<i32, FileSystemError> {
        let next = self
            .next_id
            .checked_add(1)
            .ok_or(FileSystemError::QuotaExceeded)?;
        self.resources
            .try_reserve(1)
            .map_err(|_| FileSystemError::QuotaExceeded)?;
        let id = self.next_id;
        self.next_id = next;
        self.resources.push((id, resource));
        Ok(id)
    }
    fn get(&self, id: i32) -> Result<&FileTransfer, FileSystemError> {
        self.resources
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, r)| r)
            .ok_or(FileSystemError::StaleHandle)
    }
    fn get_mut(&mut self, id: i32) -> Result<&mut FileTransfer, FileSystemError> {
        self.resources
            .iter_mut()
            .find(|(key, _)| *key == id)
            .map(|(_, r)| r)
            .ok_or(FileSystemError::StaleHandle)
    }
    fn close(&mut self, id: i32) -> Result<(), FileSystemError> {
        let index = self
            .resources
            .iter()
            .position(|(key, _)| *key == id)
            .ok_or(FileSystemError::StaleHandle)?;
        self.resources.remove(index);
        Ok(())
    }
    pub(super) fn release_all(&mut self) {
        self.resources = Vec::new();
    }
    pub(super) fn resources_count(&self) -> usize {
        self.resources.len()
    }
    fn retained_bytes(&self) -> u64 {
        self.resources.iter().map(|(_, r)| r.retained_bytes()).sum()
    }
    pub(super) fn resident_bytes(&self) -> u64 {
        self.retained_bytes()
            + (self.resources.capacity() * std::mem::size_of::<(i32, FileTransfer)>()) as u64
            + self
                .resources
                .iter()
                .map(|(_, r)| r.path().encoded_len() as u64)
                .sum::<u64>()
    }
}

pub(super) enum FileSystemV2Operation {
    Validate(Vec<u16>),
    Stat(Vec<u16>),
    OpenRead(Vec<u16>),
    Read(i32, i32),
    Size(i32),
    OpenWrite(Vec<u16>),
    Write(i32, Vec<u8>),
    Commit(i32),
    Close(i32),
    OpenDirectory(Vec<u16>),
    NextEntry(i32),
    CreateDirectory(Vec<u16>),
    Remove(Vec<u16>),
    OpenMove(Vec<u16>),
    Move(i32, Vec<u16>, bool),
}

pub(super) fn operations() -> [OperationSchema<'static>; 15] {
    use HostValueType::{Bool, ByteArray, String, I32};
    [
        OperationSchema::synchronous(&[String], I32), // validate
        OperationSchema::synchronous(&[String], ByteArray), // stat
        OperationSchema::synchronous(&[String], I32), // read snapshot
        OperationSchema::synchronous(&[I32, I32], ByteArray),
        OperationSchema::synchronous(&[I32], I32), // snapshot byte length
        OperationSchema::synchronous(&[String], I32), // draft
        OperationSchema::synchronous(&[I32, ByteArray], I32),
        OperationSchema::synchronous(&[I32], I32), // commit
        OperationSchema::synchronous(&[I32], I32), // close/abort
        OperationSchema::synchronous(&[String], I32), // directory snapshot
        OperationSchema::synchronous(&[I32], String),
        OperationSchema::synchronous(&[String], I32), // mkdir
        OperationSchema::synchronous(&[String], I32), // remove
        OperationSchema::synchronous(&[String], I32), // move source
        OperationSchema::synchronous(&[I32, String, Bool], I32),
    ]
}

pub(super) fn copy_request(request: HostRequestView<'_>) -> Result<TerminalRequest, ComputerError> {
    let invalid = || ComputerError::InvalidFileSystemRequest;
    if request.asynchronous() {
        return Err(invalid());
    }
    let string = |i| match request.arguments().get(i) {
        Some(HostValueView::String(s)) => Ok(s.to_vec()),
        _ => Err(invalid()),
    };
    let integer = |i| match request.arguments().get(i) {
        Some(HostValueView::I32(n)) => Ok(n),
        _ => Err(invalid()),
    };
    use FileSystemV2Operation::*;
    let operation = match (request.operation(), request.arguments().len()) {
        (0, 1) => Validate(string(0)?),
        (1, 1) => Stat(string(0)?),
        (2, 1) => OpenRead(string(0)?),
        (3, 2) => Read(integer(0)?, integer(1)?),
        (4, 1) => Size(integer(0)?),
        (5, 1) => OpenWrite(string(0)?),
        (6, 2) => Write(
            integer(0)?,
            match request.arguments().get(1) {
                Some(HostValueView::ByteArray(bytes)) => bytes.to_vec(),
                _ => return Err(invalid()),
            },
        ),
        (7, 1) => Commit(integer(0)?),
        (8, 1) => Close(integer(0)?),
        (9, 1) => OpenDirectory(string(0)?),
        (10, 1) => NextEntry(integer(0)?),
        (11, 1) => CreateDirectory(string(0)?),
        (12, 1) => Remove(string(0)?),
        (13, 1) => OpenMove(string(0)?),
        (14, 3) => Move(
            integer(0)?,
            string(1)?,
            match request.arguments().get(2) {
                Some(HostValueView::Bool(value)) => value,
                _ => return Err(invalid()),
            },
        ),
        _ => return Err(invalid()),
    };
    Ok(TerminalRequest::FileSystemV2 {
        task: request.task_id(),
        id: request.id(),
        operation,
    })
}

impl ComputerMachine {
    pub(super) fn transfer_capacity(
        &self,
        bytes: u64,
        new_handle: bool,
    ) -> Result<(), FileSystemError> {
        let limits = self.filesystem.limits();
        let handles: usize = self
            .sessions
            .iter()
            .map(|f| f.file_transfers.resources.len())
            .sum();
        let used: u64 = self
            .sessions
            .iter()
            .map(|f| f.file_transfers.retained_bytes())
            .sum();
        if (new_handle && handles >= limits.maximum_open_handles as usize)
            || used
                .checked_add(bytes)
                .is_none_or(|total| total > limits.maximum_logical_bytes)
        {
            return Err(FileSystemError::QuotaExceeded);
        }
        Ok(())
    }
    fn open_read_transfer(&mut self, path: VirtualPath) -> Result<i32, FileSystemError> {
        let bytes = self
            .filesystem
            .read_snapshot(&self.initial_file_capability, &path)?;
        if bytes.len() as u64 > self.filesystem.limits().maximum_file_bytes {
            return Err(FileSystemError::QuotaExceeded);
        }
        self.transfer_capacity(bytes.len() as u64, true)?;
        self.active_frame_mut()
            .file_transfers
            .insert(FileTransfer::Read {
                path,
                bytes,
                position: 0,
            })
    }
    fn open_write_transfer(&mut self, path: VirtualPath) -> Result<i32, FileSystemError> {
        let revision = self
            .filesystem
            .replacement_revision(&self.initial_file_capability, &path)?;
        self.transfer_capacity(0, true)?;
        self.active_frame_mut()
            .file_transfers
            .insert(FileTransfer::Write {
                path,
                revision,
                bytes: Vec::new(),
                poisoned: false,
            })
    }
    fn append_transfer(&mut self, id: i32, input: &[u8]) -> Result<(), FileSystemError> {
        let result = self.append_transfer_chunk(id, input);
        if result.is_err() {
            if let Ok(FileTransfer::Write { poisoned, .. }) =
                self.active_frame_mut().file_transfers.get_mut(id)
            {
                *poisoned = true;
            }
        }
        result
    }
    fn append_transfer_chunk(&mut self, id: i32, input: &[u8]) -> Result<(), FileSystemError> {
        if input.len() > CHUNK_BYTES.min(self.filesystem.limits().maximum_io_bytes) {
            return Err(FileSystemError::QuotaExceeded);
        }
        let (length, capacity) = match self.active_frame().file_transfers.get(id)? {
            FileTransfer::Write {
                bytes, poisoned, ..
            } => {
                if *poisoned {
                    return Err(FileSystemError::QuotaExceeded);
                }
                (bytes.len(), bytes.capacity())
            }
            _ => return Err(FileSystemError::StaleHandle),
        };
        let required = length
            .checked_add(input.len())
            .ok_or(FileSystemError::QuotaExceeded)?;
        let maximum = usize::try_from(self.filesystem.limits().maximum_file_bytes)
            .map_err(|_| FileSystemError::QuotaExceeded)?;
        if required > maximum {
            return Err(FileSystemError::QuotaExceeded);
        }
        let target = if required > capacity {
            required.max(capacity.saturating_mul(2)).min(maximum)
        } else {
            capacity
        };
        self.transfer_capacity((target - capacity) as u64, false)?;
        let FileTransfer::Write { bytes, .. } =
            self.active_frame_mut().file_transfers.get_mut(id)?
        else {
            unreachable!()
        };
        if target > capacity {
            bytes
                .try_reserve_exact(target - bytes.len())
                .map_err(|_| FileSystemError::QuotaExceeded)?;
        }
        bytes.extend_from_slice(input);
        Ok(())
    }
    fn commit_transfer(&mut self, id: i32) -> Result<(), FileSystemError> {
        let FileTransfer::Write {
            path,
            revision,
            poisoned,
            ..
        } = self.active_frame().file_transfers.get(id)?
        else {
            return Err(FileSystemError::StaleHandle);
        };
        if *poisoned {
            return Err(FileSystemError::QuotaExceeded);
        }
        // Borrow the separate fields directly: no repeated whole-file cloning.
        let index = self.sessions.len() - 1;
        let (path, revision) = (path.clone(), *revision);
        let FileTransfer::Write { bytes, .. } = self.sessions[index].file_transfers.get(id)? else {
            unreachable!()
        };
        self.filesystem.replace_file_if_revision(
            &self.initial_file_capability,
            &path,
            bytes,
            revision,
            false,
        )?;
        self.sessions[index].file_transfers.close(id)
    }
    fn read_transfer(&mut self, id: i32, maximum: i32) -> Result<Vec<u8>, FileSystemError> {
        if maximum <= 0
            || maximum as usize > CHUNK_BYTES.min(self.filesystem.limits().maximum_io_bytes)
        {
            return Err(FileSystemError::QuotaExceeded);
        }
        let FileTransfer::Read {
            bytes, position, ..
        } = self.active_frame_mut().file_transfers.get_mut(id)?
        else {
            return Err(FileSystemError::StaleHandle);
        };
        let end = position.saturating_add(maximum as usize).min(bytes.len());
        let mut result = Vec::new();
        result
            .try_reserve_exact(end - *position + 1)
            .map_err(|_| FileSystemError::QuotaExceeded)?;
        result.push(0);
        result.extend_from_slice(&bytes[*position..end]);
        *position = end;
        Ok(result)
    }
    fn open_directory_transfer(&mut self, path: VirtualPath) -> Result<i32, FileSystemError> {
        let names = self.filesystem.list(&self.initial_file_capability, &path)?;
        let resource = FileTransfer::Directory {
            path,
            names,
            position: 0,
        };
        self.transfer_capacity(resource.retained_bytes(), true)?;
        self.active_frame_mut().file_transfers.insert(resource)
    }
    fn next_directory_entry(&mut self, id: i32) -> Result<Vec<u16>, FileSystemError> {
        let bound = self.maximum_text_code_units;
        let FileTransfer::Directory {
            names, position, ..
        } = self.active_frame_mut().file_transfers.get_mut(id)?
        else {
            return Err(FileSystemError::StaleHandle);
        };
        let Some(name) = names.get(*position) else {
            return Ok(vec![0]);
        };
        if name.encode_utf16().count() + 1 > bound {
            return Err(FileSystemError::QuotaExceeded);
        }
        let result = std::iter::once(0).chain(name.encode_utf16()).collect();
        *position += 1;
        Ok(result)
    }
    fn open_move_transfer(&mut self, path: VirtualPath) -> Result<i32, FileSystemError> {
        if !self
            .initial_file_capability
            .permits(&path, FileRights::RENAME | FileRights::INSPECT)
        {
            return Err(FileSystemError::PermissionDenied);
        }
        let generation = self
            .filesystem
            .stat(&self.initial_file_capability, &path)?
            .generation;
        self.transfer_capacity(0, true)?;
        self.active_frame_mut()
            .file_transfers
            .insert(FileTransfer::Move { path, generation })
    }
    fn move_transfer(
        &mut self,
        id: i32,
        destination: VirtualPath,
        replace: bool,
    ) -> Result<(), FileSystemError> {
        let FileTransfer::Move { path, generation } = self.active_frame().file_transfers.get(id)?
        else {
            return Err(FileSystemError::StaleHandle);
        };
        if self
            .filesystem
            .stat(&self.initial_file_capability, path)?
            .generation
            != *generation
        {
            return Err(FileSystemError::Busy);
        }
        let path = path.clone();
        self.filesystem
            .rename(&self.initial_file_capability, &path, &destination, replace)
    }
    pub(super) fn handle_filesystem_v2_request(
        &mut self,
        task: TaskId,
        id: RequestId,
        operation: FileSystemV2Operation,
    ) -> Result<ComputerAdvanceOutcome, ComputerError> {
        use FileSystemV2Operation::*;
        match operation {
            Stat(units) => {
                let metadata = self
                    .parse_path(&units)
                    .and_then(|p| self.filesystem.stat(&self.initial_file_capability, &p));
                let bytes = match metadata {
                    Ok(NodeMetadata {
                        kind,
                        logical_size,
                        generation,
                        executable,
                    }) => {
                        let mut b = vec![
                            0,
                            if kind == NodeKind::File { 1 } else { 2 },
                            u8::from(executable),
                        ];
                        b.extend_from_slice(&logical_size.to_le_bytes());
                        b.extend_from_slice(&generation.to_le_bytes());
                        b
                    }
                    Err(e) => vec![(-filesystem_error_code(e)) as u8],
                };
                self.resume_filesystem(
                    task,
                    id,
                    HostResponse::Success(HostValueInput::ByteArray(&bytes)),
                )?;
            }
            Read(handle, maximum) => {
                let bytes = self
                    .read_transfer(handle, maximum)
                    .unwrap_or_else(|e| vec![(-filesystem_error_code(e)) as u8]);
                self.resume_filesystem(
                    task,
                    id,
                    HostResponse::Success(HostValueInput::ByteArray(&bytes)),
                )?;
            }
            NextEntry(handle) => {
                let units = self
                    .next_directory_entry(handle)
                    .unwrap_or_else(|e| vec![(-filesystem_error_code(e)) as u16]);
                self.resume_filesystem(
                    task,
                    id,
                    HostResponse::Success(HostValueInput::String(&units)),
                )?;
            }
            operation => {
                let result: Result<i32, FileSystemError> = match operation {
                    Validate(p) => self.parse_path(&p).map(|_| 0),
                    OpenRead(p) => self.parse_path(&p).and_then(|p| self.open_read_transfer(p)),
                    OpenWrite(p) => self
                        .parse_path(&p)
                        .and_then(|p| self.open_write_transfer(p)),
                    OpenDirectory(p) => self
                        .parse_path(&p)
                        .and_then(|p| self.open_directory_transfer(p)),
                    OpenMove(p) => self.parse_path(&p).and_then(|p| self.open_move_transfer(p)),
                    Size(h) => self
                        .active_frame()
                        .file_transfers
                        .get(h)
                        .and_then(|r| match r {
                            FileTransfer::Read { bytes, .. } => i32::try_from(bytes.len())
                                .map_err(|_| FileSystemError::QuotaExceeded),
                            _ => Err(FileSystemError::StaleHandle),
                        }),
                    Write(h, b) => self.append_transfer(h, &b).map(|_| 0),
                    Commit(h) => self.commit_transfer(h).map(|_| 0),
                    Close(h) => self.active_frame_mut().file_transfers.close(h).map(|_| 0),
                    CreateDirectory(p) => self
                        .parse_path(&p)
                        .and_then(|p| {
                            self.filesystem
                                .create_directory(&self.initial_file_capability, &p)
                        })
                        .map(|_| 0),
                    Remove(p) => self
                        .parse_path(&p)
                        .and_then(|p| self.filesystem.remove(&self.initial_file_capability, &p))
                        .map(|_| 0),
                    Move(h, p, replace) => self
                        .parse_path(&p)
                        .and_then(|p| self.move_transfer(h, p, replace))
                        .map(|_| 0),
                    Stat(_) | Read(..) | NextEntry(_) => unreachable!(),
                };
                self.resume_filesystem(
                    task,
                    id,
                    HostResponse::Success(HostValueInput::I32(
                        result.unwrap_or_else(filesystem_error_code),
                    )),
                )?;
            }
        }
        Ok(ComputerAdvanceOutcome::SliceExhausted)
    }
}

impl Checkpoint for FileTransfers {
    fn write(&self, writer: &mut Writer) -> crate::execution::checkpoint::Result<()> {
        self.next_id.write(writer)?;
        self.resources.len().write(writer)?;
        for (id, resource) in &self.resources {
            id.write(writer)?;
            resource.path().write(writer)?;
            match resource {
                FileTransfer::Read {
                    bytes, position, ..
                } => {
                    0_u8.write(writer)?;
                    bytes.len().write(writer)?;
                    writer.put(bytes)?;
                    position.write(writer)?;
                }
                FileTransfer::Write {
                    revision,
                    bytes,
                    poisoned,
                    ..
                } => {
                    1_u8.write(writer)?;
                    revision.write(writer)?;
                    bytes.write(writer)?;
                    poisoned.write(writer)?;
                }
                FileTransfer::Directory {
                    names, position, ..
                } => {
                    2_u8.write(writer)?;
                    names.write(writer)?;
                    position.write(writer)?;
                }
                FileTransfer::Move { generation, .. } => {
                    3_u8.write(writer)?;
                    generation.write(writer)?;
                }
            }
        }
        Ok(())
    }
    fn read(reader: &mut Reader<'_>) -> crate::execution::checkpoint::Result<Self> {
        Self::read_with_limits(reader, &FileSystemLimits::default())
    }
}

fn read_bounded_bytes(
    reader: &mut Reader<'_>,
    maximum: usize,
) -> crate::execution::checkpoint::Result<Vec<u8>> {
    let length = usize::read(reader)?;
    if length > maximum {
        return Err(CheckpointError::Limit);
    }
    let input = reader.take(length)?;
    reader.allocate::<u8>(length)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| CheckpointError::Allocation)?;
    bytes.extend_from_slice(input);
    Ok(bytes)
}

impl FileTransfers {
    pub(super) fn read_with_limits(
        reader: &mut Reader<'_>,
        limits: &FileSystemLimits,
    ) -> crate::execution::checkpoint::Result<Self> {
        let next_id = i32::read(reader)?;
        let count = usize::read(reader)?;
        if count > limits.maximum_open_handles as usize || next_id <= 0 {
            return Err(CheckpointError::Limit);
        }
        reader.allocate::<(i32, FileTransfer)>(count)?;
        let mut resources = Vec::new();
        resources
            .try_reserve_exact(count)
            .map_err(|_| CheckpointError::Allocation)?;
        let mut previous = 0;
        let mut used = 0_u64;
        for _ in 0..count {
            let id = i32::read(reader)?;
            if id <= previous || id >= next_id {
                return Err(CheckpointError::InvalidState);
            }
            previous = id;
            let encoded_path = read_bounded_bytes(reader, limits.maximum_path_bytes)?;
            let path = VirtualPath::parse_utf8(
                std::str::from_utf8(&encoded_path).map_err(|_| CheckpointError::InvalidState)?,
                limits,
            )
            .map_err(|_| CheckpointError::InvalidState)?;
            let maximum_bytes = usize::try_from(
                limits
                    .maximum_file_bytes
                    .min(limits.maximum_logical_bytes - used),
            )
            .map_err(|_| CheckpointError::Limit)?;
            let resource = match u8::read(reader)? {
                0 => {
                    let bytes = read_bounded_bytes(reader, maximum_bytes)?;
                    let position = usize::read(reader)?;
                    if position > bytes.len() {
                        return Err(CheckpointError::InvalidState);
                    }
                    FileTransfer::Read {
                        path,
                        bytes: bytes.into(),
                        position,
                    }
                }
                1 => FileTransfer::Write {
                    path,
                    revision: Checkpoint::read(reader)?,
                    bytes: read_bounded_bytes(reader, maximum_bytes)?,
                    poisoned: bool::read(reader)?,
                },
                2 => {
                    let count = usize::read(reader)?;
                    if count > limits.maximum_directory_entries as usize {
                        return Err(CheckpointError::Limit);
                    }
                    let storage = count
                        .checked_mul(std::mem::size_of::<Box<str>>())
                        .ok_or(CheckpointError::Limit)? as u64;
                    if used
                        .checked_add(storage)
                        .is_none_or(|n| n > limits.maximum_logical_bytes)
                    {
                        return Err(CheckpointError::Limit);
                    }
                    reader.allocate::<Box<str>>(count)?;
                    let mut names = Vec::new();
                    names
                        .try_reserve_exact(count)
                        .map_err(|_| CheckpointError::Allocation)?;
                    let mut directory_bytes = storage;
                    for _ in 0..count {
                        let remaining = limits.maximum_logical_bytes - used - directory_bytes;
                        let bytes = read_bounded_bytes(
                            reader,
                            limits.maximum_component_bytes.min(remaining as usize),
                        )?;
                        directory_bytes += bytes.len() as u64;
                        names.push(
                            String::from_utf8(bytes)
                                .map_err(|_| CheckpointError::InvalidState)?
                                .into_boxed_str(),
                        );
                    }
                    let position = usize::read(reader)?;
                    if position > names.len() || names.windows(2).any(|n| n[0] >= n[1]) {
                        return Err(CheckpointError::InvalidState);
                    }
                    FileTransfer::Directory {
                        path,
                        names,
                        position,
                    }
                }
                3 => FileTransfer::Move {
                    path,
                    generation: u64::read(reader)?,
                },
                _ => return Err(CheckpointError::InvalidState),
            };
            used = used
                .checked_add(resource.retained_bytes())
                .ok_or(CheckpointError::Limit)?;
            if used > limits.maximum_logical_bytes {
                return Err(CheckpointError::Limit);
            }
            resources.push((id, resource));
        }
        Ok(Self { next_id, resources })
    }
}

impl FileTransfers {
    pub(super) fn validate(
        &self,
        limits: &FileSystemLimits,
        capability: &FileCapability,
    ) -> crate::execution::checkpoint::Result<()> {
        if self.resources.len() > limits.maximum_open_handles as usize
            || self.retained_bytes() > limits.maximum_logical_bytes
        {
            return Err(CheckpointError::Limit);
        }
        for (_, resource) in &self.resources {
            VirtualPath::parse_utf8(&resource.path().to_string(), limits)
                .map_err(|_| CheckpointError::InvalidState)?;
            let rights = match resource {
                FileTransfer::Read { bytes, .. } => {
                    if bytes.len() as u64 > limits.maximum_file_bytes {
                        return Err(CheckpointError::Limit);
                    }
                    FileRights::READ
                }
                FileTransfer::Write {
                    bytes,
                    revision,
                    path,
                    ..
                } => {
                    if path.components().next() != Some("home")
                        || matches!(revision, ExecutableRevision::Present(0))
                    {
                        return Err(CheckpointError::InvalidState);
                    }
                    if bytes.len() as u64 > limits.maximum_file_bytes {
                        return Err(CheckpointError::Limit);
                    }
                    if *revision == ExecutableRevision::Absent {
                        FileRights::WRITE | FileRights::CREATE
                    } else {
                        FileRights::WRITE
                    }
                }
                FileTransfer::Directory { path, names, .. } => {
                    if names.len() > limits.maximum_directory_entries as usize {
                        return Err(CheckpointError::Limit);
                    }
                    for name in names {
                        VirtualPath::parse_utf8(
                            &format!(
                                "{}{name}",
                                if path.to_string() == "/" {
                                    "/".to_owned()
                                } else {
                                    format!("{path}/")
                                }
                            ),
                            limits,
                        )
                        .map_err(|_| CheckpointError::InvalidState)?;
                        if name.is_empty() || name.contains('/') {
                            return Err(CheckpointError::InvalidState);
                        }
                    }
                    FileRights::LIST
                }
                FileTransfer::Move { .. } => FileRights::RENAME | FileRights::INSPECT,
            };
            if !capability.permits(resource.path(), rights) {
                return Err(CheckpointError::InvalidState);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::fixtures;
    use crate::filesystem::ComputerId;

    fn path(s: &str) -> VirtualPath {
        VirtualPath::parse_utf8(s, &FileSystemLimits::default()).unwrap()
    }
    fn computer(limits: FileSystemLimits) -> ComputerMachine {
        ComputerMachine::start_in_filesystem(
            fixtures::filesystem_conformance_artifact(),
            crate::computer::tests::profile(),
            &[],
            &[],
            ComputerFileSystem::with_limits(limits),
            FileCapability::new(path("/home"), FileRights::OWNER),
        )
        .unwrap()
    }
    fn put(c: &mut ComputerMachine, name: &str, bytes: &[u8]) {
        c.filesystem
            .write_file(&c.initial_file_capability, &path(name), bytes, false)
            .unwrap();
    }
    fn bytes(c: &ComputerMachine, name: &str) -> Vec<u8> {
        c.filesystem
            .read_snapshot(&c.initial_file_capability, &path(name))
            .unwrap()
            .to_vec()
    }
    #[test]
    fn root_completion_discards_drafts_and_releases_snapshots() {
        let mut c = computer(FileSystemLimits::default());
        put(&mut c, "/home/note", b"old");
        c.open_read_transfer(path("/home/note")).unwrap();
        let draft = c.open_write_transfer(path("/home/note")).unwrap();
        c.append_transfer(draft, b"draft").unwrap();
        loop {
            match c.advance(10000, 10000, 16).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::Halted(_) => break,
                other => panic!("unexpected completion {other:?}"),
            }
        }
        assert_eq!(0, c.active_frame().file_transfers.resources_count());
        assert_eq!(b"old", bytes(&c, "/home/note").as_slice());
    }

    #[test]
    fn child_exit_discards_its_draft_and_keeps_suspended_parent_resources() {
        let limits = FileSystemLimits::default();
        let owner = FileCapability::new(path("/home"), FileRights::OWNER);
        let mut fs = ComputerFileSystem::with_limits(limits);
        let child = fixtures::two_block_artifact(1, 1);
        let encoded = crate::test_encode::encode_artifact(child.decoded()).unwrap();
        fs.write_file(&owner, &path("/home/child"), &encoded, true)
            .unwrap();
        fs.write_file(&owner, &path("/home/note"), b"original", false)
            .unwrap();
        let parent = fixtures::process_v2_run_artifact(
            &"/home/child".encode_utf16().collect::<Vec<_>>(),
            &[0, 0],
        );
        let mut c = ComputerMachine::start_in_filesystem(
            parent,
            crate::computer::tests::profile(),
            &[],
            &[],
            fs,
            owner,
        )
        .unwrap();
        let read = c.open_read_transfer(path("/home/note")).unwrap();
        loop {
            match c.advance(10000, 10000, 16).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::ProcessEntered(_) => break,
                other => panic!("unexpected entry {other:?}"),
            }
        }
        assert_eq!(Err(FileSystemError::StaleHandle), c.read_transfer(read, 1));
        let draft = c.open_write_transfer(path("/home/note")).unwrap();
        c.append_transfer(draft, b"unpublished").unwrap();
        loop {
            match c.advance(10000, 10000, 16).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::ProcessExited(_) => break,
                other => panic!("unexpected exit {other:?}"),
            }
        }
        assert_eq!(1, c.active_frame().file_transfers.resources_count());
        assert_eq!(b"original", bytes(&c, "/home/note").as_slice());
        assert_eq!([0, b'o'], c.read_transfer(read, 1).unwrap().as_slice());
    }

    #[test]
    fn transfer_chunks_respect_smaller_filesystem_io_limits() {
        let mut limits = FileSystemLimits::default();
        limits.maximum_io_bytes = 2;
        let mut c = computer(limits);
        put(&mut c, "/home/note", b"ok");
        let read = c.open_read_transfer(path("/home/note")).unwrap();
        assert_eq!(
            Err(FileSystemError::QuotaExceeded),
            c.read_transfer(read, 3)
        );
        assert_eq!(
            [0, b'o', b'k'],
            c.read_transfer(read, 2).unwrap().as_slice()
        );
        let draft = c.open_write_transfer(path("/home/note")).unwrap();
        assert_eq!(
            Err(FileSystemError::QuotaExceeded),
            c.append_transfer(draft, b"bad")
        );
        assert_eq!(
            Err(FileSystemError::QuotaExceeded),
            c.commit_transfer(draft)
        );
        assert_eq!(b"ok", bytes(&c, "/home/note").as_slice());
    }

    #[test]
    fn checkpoint_decoder_bounds_file_lengths_before_allocation() {
        let limits = FileSystemLimits::testing();
        let mut writer = Writer::new(4096);
        2_i32.write(&mut writer).unwrap();
        1_usize.write(&mut writer).unwrap();
        1_i32.write(&mut writer).unwrap();
        path("/home/note").write(&mut writer).unwrap();
        0_u8.write(&mut writer).unwrap();
        (limits.maximum_file_bytes as usize + 1)
            .write(&mut writer)
            .unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 4096, 4096).unwrap();
        assert!(matches!(
            FileTransfers::read_with_limits(&mut reader, &limits),
            Err(CheckpointError::Limit)
        ));
    }

    #[test]
    fn chunked_draft_is_invisible_until_commit_and_snapshot_keeps_old_bytes() {
        let mut c = computer(FileSystemLimits::default());
        put(&mut c, "/home/note", b"old");
        let reader = c.open_read_transfer(path("/home/note")).unwrap();
        let draft = c.open_write_transfer(path("/home/note")).unwrap();
        let content: Vec<u8> = (0..20000).map(|n| (n % 256) as u8).collect();
        for part in content.chunks(CHUNK_BYTES) {
            c.append_transfer(draft, part).unwrap();
            assert_eq!(b"old", bytes(&c, "/home/note").as_slice());
        }
        c.commit_transfer(draft).unwrap();
        assert_eq!(content, bytes(&c, "/home/note"));
        assert_eq!(
            [0, b'o', b'l', b'd'],
            c.read_transfer(reader, 1024).unwrap().as_slice()
        );
        assert_eq!([0], c.read_transfer(reader, 1).unwrap().as_slice());
        assert_eq!(Err(FileSystemError::StaleHandle), c.commit_transfer(draft));
    }
    #[test]
    fn aborted_conflicting_and_oversized_drafts_preserve_published_files() {
        let mut limits = FileSystemLimits::default();
        limits.maximum_file_bytes = 5;
        let mut c = computer(limits);
        put(&mut c, "/home/note", b"old");
        let draft = c.open_write_transfer(path("/home/note")).unwrap();
        c.append_transfer(draft, b"first").unwrap();
        assert_eq!(
            Err(FileSystemError::QuotaExceeded),
            c.append_transfer(draft, b"!")
        );
        c.active_frame_mut().file_transfers.close(draft).unwrap();
        assert_eq!(b"old", bytes(&c, "/home/note").as_slice());
        let draft = c.open_write_transfer(path("/home/note")).unwrap();
        c.append_transfer(draft, b"draft").unwrap();
        put(&mut c, "/home/note", b"other");
        assert_eq!(Err(FileSystemError::Busy), c.commit_transfer(draft));
        assert_eq!(b"other", bytes(&c, "/home/note").as_slice());
        c.active_frame_mut().file_transfers.close(draft).unwrap();
        let absent = c.open_write_transfer(path("/home/absent")).unwrap();
        c.append_transfer(absent, b"x").unwrap();
        c.active_frame_mut().file_transfers.close(absent).unwrap();
        assert_eq!(
            Err(FileSystemError::NotFound),
            c.filesystem
                .stat(&c.initial_file_capability, &path("/home/absent"))
        );
    }
    #[test]
    fn directory_snapshot_is_sorted_and_ids_never_alias_after_close() {
        let mut c = computer(FileSystemLimits::default());
        put(&mut c, "/home/z", b"z");
        put(&mut c, "/home/a", b"a");
        let directory = c.open_directory_transfer(path("/home")).unwrap();
        put(&mut c, "/home/b", b"b");
        assert_eq!(
            vec![0, b'a' as u16],
            c.next_directory_entry(directory).unwrap()
        );
        c.filesystem
            .remove(&c.initial_file_capability, &path("/home/z"))
            .unwrap();
        assert_eq!(
            vec![0, b'z' as u16],
            c.next_directory_entry(directory).unwrap()
        );
        assert_eq!(vec![0], c.next_directory_entry(directory).unwrap());
        c.active_frame_mut()
            .file_transfers
            .close(directory)
            .unwrap();
        let new = c.open_read_transfer(path("/home/a")).unwrap();
        assert!(new > directory);
        assert_eq!(
            Err(FileSystemError::StaleHandle),
            c.next_directory_entry(directory)
        );
        assert_eq!(
            Err(FileSystemError::StaleHandle),
            c.next_directory_entry(new)
        );
    }
    #[test]
    fn aggregate_transfer_storage_and_handle_limits_release_on_close() {
        let mut limits = FileSystemLimits::default();
        limits.maximum_logical_bytes = 8;
        limits.maximum_open_handles = 2;
        let mut c = computer(limits);
        put(&mut c, "/home/note", b"four");
        let first = c.open_read_transfer(path("/home/note")).unwrap();
        let second = c.open_read_transfer(path("/home/note")).unwrap();
        assert_eq!(
            Err(FileSystemError::QuotaExceeded),
            c.open_write_transfer(path("/home/new"))
        );
        c.active_frame_mut().file_transfers.close(second).unwrap();
        let draft = c.open_write_transfer(path("/home/new")).unwrap();
        c.append_transfer(draft, b"four").unwrap();
        assert_eq!(
            Err(FileSystemError::QuotaExceeded),
            c.append_transfer(draft, b"x")
        );
        c.active_frame_mut().file_transfers.close(first).unwrap();
        assert_eq!(
            Err(FileSystemError::QuotaExceeded),
            c.append_transfer(draft, b"x")
        );
        assert_eq!(
            Err(FileSystemError::QuotaExceeded),
            c.commit_transfer(draft)
        );
        c.active_frame_mut().file_transfers.close(draft).unwrap();
        assert_eq!(0, c.active_frame().file_transfers.retained_bytes());
    }
    #[test]
    fn computer_checkpoint_restores_snapshot_positions_and_unpublished_draft() {
        let id = ComputerId::from_bytes([4; 16]);
        let mut c = computer(FileSystemLimits::default());
        put(&mut c, "/home/note", b"original");
        let reader = c.open_read_transfer(path("/home/note")).unwrap();
        assert_eq!(
            [0, b'o', b'r'],
            c.read_transfer(reader, 2).unwrap().as_slice()
        );
        let draft = c.open_write_transfer(path("/home/note")).unwrap();
        c.append_transfer(draft, b"first ").unwrap();
        let directory = c.open_directory_transfer(path("/home")).unwrap();
        let limits = ComputerCheckpointLimits::default();
        let encoded = c.checkpoint(id, b"", limits).unwrap();
        let (mut restored, _) = ComputerMachine::restore_checkpoint(
            ComputerRestoreEnvironment {
                id,
                profile: c.profile.clone(),
                process_limits: c.process_limits,
                addon_bindings: &[],
                filesystem: ComputerFileSystem::with_limits(*c.filesystem.limits()),
                initial_file_capability: c.initial_file_capability.clone(),
            },
            &encoded,
            limits,
        )
        .unwrap();
        assert_eq!(b"original", bytes(&restored, "/home/note").as_slice());
        restored.append_transfer(draft, b"second").unwrap();
        restored.commit_transfer(draft).unwrap();
        assert_eq!(b"first second", bytes(&restored, "/home/note").as_slice());
        assert_eq!(
            [0, b'i', b'g', b'i', b'n', b'a', b'l'],
            restored.read_transfer(reader, 4095).unwrap().as_slice()
        );
        assert_eq!(
            vec![0, b'n' as u16, b'o' as u16, b't' as u16, b'e' as u16],
            restored.next_directory_entry(directory).unwrap()
        );
    }
    #[test]
    fn move_source_guard_rejects_path_reuse_and_checks_destination_collision() {
        let mut c = computer(FileSystemLimits::default());
        put(&mut c, "/home/source", b"old");
        let source = c.open_move_transfer(path("/home/source")).unwrap();
        put(&mut c, "/home/source", b"new");
        assert_eq!(
            Err(FileSystemError::Busy),
            c.move_transfer(source, path("/home/destination"), false)
        );
        c.active_frame_mut().file_transfers.close(source).unwrap();
        let source = c.open_move_transfer(path("/home/source")).unwrap();
        put(&mut c, "/home/destination", b"existing");
        assert_eq!(
            Err(FileSystemError::AlreadyExists),
            c.move_transfer(source, path("/home/destination"), false)
        );
        c.move_transfer(source, path("/home/destination"), true)
            .unwrap();
        assert_eq!(b"new", bytes(&c, "/home/destination").as_slice());
    }
}
