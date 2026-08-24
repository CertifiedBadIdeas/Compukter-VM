/*
 * The Compukters Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

use crate::{
    AdmissionError, AdvanceOutcome, CapabilityBinding, ComputerFileSystem, EntryValue,
    ExecutionProfile, FileCapability, FileRights, FileSystemError, FileSystemLimits, GuestTrap,
    HostFailure, HostFailureKind, HostRequestView, HostResponse, HostValueInput, HostValueType,
    HostValueView, ManagedAllocationFailure, NodeKind, OpenMode, OperationSchema, QuotaExhaustion,
    RequestId, ResumeError, RunError, Session, TerminalDevice, TerminalInputEvent,
    TerminalKeyAction, VerifiedArtifact, VirtualPath, VmFault,
};

const TERMINAL_NAMESPACE: &str = "compukter";
const TERMINAL_NAME: &str = "terminal";
const RAW_TERMINAL_ABI_MAJOR: u16 = 2;
const FILESYSTEM_NAME: &str = "filesystem";
const FILESYSTEM_ABI_MAJOR: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComputerStartError {
    Admission(AdmissionError),
    Start(RunError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComputerError {
    Run(RunError),
    Resume(ResumeError),
    InvalidRequestId,
    InvalidTerminalRequest,
    InvalidFileSystemRequest,
    ActiveTerminalEvent,
    NoActiveTerminalEvent,
    WrongTerminalEventKind,
}

#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComputerTerminalEventKind {
    Text = 1,
    Key = 2,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ComputerValue {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Bool(bool),
    Char(u16),
    String(Box<[u16]>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ComputerHostRequest {
    pub id: u64,
    pub namespace: Box<str>,
    pub name: Box<str>,
    pub abi_major: u16,
    pub abi_minor: u16,
    pub operation: u32,
    pub arguments: Box<[ComputerValue]>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ComputerAdvanceOutcome {
    SliceExhausted,
    WaitingForTerminalEvent,
    HostRequest(ComputerHostRequest),
    AllocationExhausted(ManagedAllocationFailure),
    QuotaExhausted(QuotaExhaustion),
    Halted(Option<ComputerValue>),
    Crashed(GuestTrap),
    Faulted(VmFault),
    HostFailed(HostFailure),
}

#[derive(Debug)]
pub struct ComputerMachine {
    session: Session,
    terminal: TerminalDevice,
    pending_terminal_event: Option<RequestId>,
    active_terminal_event: Option<TerminalInputEvent>,
    filesystem: ComputerFileSystem,
    initial_file_capability: FileCapability,
    maximum_text_code_units: usize,
}

impl ComputerMachine {
    pub fn start(
        artifact: VerifiedArtifact,
        profile: ExecutionProfile,
        addon_bindings: &[CapabilityBinding<'_>],
        arguments: &[EntryValue],
    ) -> Result<Self, ComputerStartError> {
        let limits = FileSystemLimits::default();
        let initial_capability = FileCapability::new(
            VirtualPath::parse_utf8("/home", &limits).expect("fixed ephemeral filesystem path"),
            FileRights::OWNER,
        );
        Self::start_in_filesystem(
            artifact,
            profile,
            addon_bindings,
            arguments,
            ComputerFileSystem::with_limits(limits),
            initial_capability,
        )
    }

    pub fn start_in_filesystem(
        artifact: VerifiedArtifact,
        profile: ExecutionProfile,
        addon_bindings: &[CapabilityBinding<'_>],
        arguments: &[EntryValue],
        filesystem: ComputerFileSystem,
        initial_capability: FileCapability,
    ) -> Result<Self, ComputerStartError> {
        let maximum_text_code_units = profile.maximum_inbound_utf16_code_units as usize;
        let string_argument = [HostValueType::String];
        let two_string_arguments = [HostValueType::String, HostValueType::String];
        let raw_terminal_operations = [
            OperationSchema::synchronous(&string_argument, HostValueType::Unit),
            OperationSchema::synchronous(&[], HostValueType::Unit),
            OperationSchema::synchronous(&[], HostValueType::Unit),
            OperationSchema::asynchronous(&[], HostValueType::I32),
            OperationSchema::synchronous(&[], HostValueType::String),
            OperationSchema::synchronous(&[], HostValueType::I32),
            OperationSchema::synchronous(&[], HostValueType::I32),
            OperationSchema::synchronous(&[], HostValueType::I32),
            OperationSchema::synchronous(&[], HostValueType::Unit),
        ];
        let raw_terminal_binding = CapabilityBinding::new(
            TERMINAL_NAMESPACE,
            TERMINAL_NAME,
            RAW_TERMINAL_ABI_MAJOR,
            0,
            &raw_terminal_operations,
        );
        let filesystem_operations = [
            OperationSchema::synchronous(&string_argument, HostValueType::I32),
            OperationSchema::synchronous(&string_argument, HostValueType::String),
            OperationSchema::synchronous(&string_argument, HostValueType::String),
            OperationSchema::synchronous(&two_string_arguments, HostValueType::I32),
            OperationSchema::synchronous(&string_argument, HostValueType::I32),
            OperationSchema::synchronous(&string_argument, HostValueType::I32),
            OperationSchema::synchronous(&two_string_arguments, HostValueType::I32),
        ];
        let filesystem_binding = CapabilityBinding::new(
            TERMINAL_NAMESPACE,
            FILESYSTEM_NAME,
            FILESYSTEM_ABI_MAJOR,
            0,
            &filesystem_operations,
        );
        let mut bindings = Vec::with_capacity(addon_bindings.len() + 2);
        bindings.extend_from_slice(addon_bindings);
        bindings.push(raw_terminal_binding);
        bindings.push(filesystem_binding);
        let mut session =
            Session::admit(artifact, profile, &bindings).map_err(ComputerStartError::Admission)?;
        session
            .start(arguments)
            .map_err(ComputerStartError::Start)?;
        Ok(Self {
            session,
            terminal: TerminalDevice::default(),
            pending_terminal_event: None,
            active_terminal_event: None,
            filesystem,
            initial_file_capability: initial_capability,
            maximum_text_code_units,
        })
    }

    pub const fn terminal(&self) -> &TerminalDevice {
        &self.terminal
    }

    pub fn filesystem_generation(&self) -> u64 {
        self.filesystem.generation()
    }

    pub fn terminal_mut(&mut self) -> &mut TerminalDevice {
        &mut self.terminal
    }

    pub fn terminal_await_event(
        &mut self,
    ) -> Result<Option<ComputerTerminalEventKind>, ComputerError> {
        if self.active_terminal_event.is_some() {
            return Err(ComputerError::ActiveTerminalEvent);
        }
        let Some(event) = self.terminal.poll_input() else {
            return Ok(None);
        };
        let kind = match event {
            TerminalInputEvent::Text(_) => ComputerTerminalEventKind::Text,
            TerminalInputEvent::Key(_) => ComputerTerminalEventKind::Key,
        };
        self.active_terminal_event = Some(event);
        Ok(Some(kind))
    }

    pub fn terminal_event_text(&self) -> Result<&str, ComputerError> {
        match self.active_terminal_event.as_ref() {
            Some(TerminalInputEvent::Text(text)) => Ok(text),
            Some(TerminalInputEvent::Key(_)) => Err(ComputerError::WrongTerminalEventKind),
            None => Err(ComputerError::NoActiveTerminalEvent),
        }
    }

    pub fn terminal_event_key(&self) -> Result<u16, ComputerError> {
        match self.active_terminal_event.as_ref() {
            Some(TerminalInputEvent::Key(event)) => Ok(event.key().code()),
            Some(TerminalInputEvent::Text(_)) => Err(ComputerError::WrongTerminalEventKind),
            None => Err(ComputerError::NoActiveTerminalEvent),
        }
    }

    pub fn terminal_event_action(&self) -> Result<i32, ComputerError> {
        match self.active_terminal_event.as_ref() {
            Some(TerminalInputEvent::Key(event)) => Ok(match event.action() {
                TerminalKeyAction::Press => 1,
                TerminalKeyAction::Repeat => 2,
            }),
            Some(TerminalInputEvent::Text(_)) => Err(ComputerError::WrongTerminalEventKind),
            None => Err(ComputerError::NoActiveTerminalEvent),
        }
    }

    pub fn terminal_event_modifiers(&self) -> Result<u8, ComputerError> {
        match self.active_terminal_event.as_ref() {
            Some(TerminalInputEvent::Key(event)) => Ok(event.modifiers().bits()),
            Some(TerminalInputEvent::Text(_)) => Err(ComputerError::WrongTerminalEventKind),
            None => Err(ComputerError::NoActiveTerminalEvent),
        }
    }

    pub fn terminal_finish_event(&mut self) -> Result<(), ComputerError> {
        self.active_terminal_event
            .take()
            .map(|_| ())
            .ok_or(ComputerError::NoActiveTerminalEvent)
    }

    pub fn advance(
        &mut self,
        guest_budget: u32,
        maintenance_budget: u32,
    ) -> Result<ComputerAdvanceOutcome, ComputerError> {
        if let Some(request) = self.pending_terminal_event {
            let Some(kind) = self.terminal_await_event()? else {
                return Ok(ComputerAdvanceOutcome::WaitingForTerminalEvent);
            };
            self.session
                .resume_internal(
                    request,
                    HostResponse::Success(HostValueInput::I32(kind as i32)),
                )
                .map_err(ComputerError::Resume)?;
            self.pending_terminal_event = None;
            return Ok(ComputerAdvanceOutcome::SliceExhausted);
        }
        let internal = {
            let outcome = self
                .session
                .advance(guest_budget, maintenance_budget)
                .map_err(ComputerError::Run)?;
            match outcome {
                AdvanceOutcome::HostRequest(request) if is_raw_terminal(request) => {
                    Some(copy_raw_terminal_request(request)?)
                }
                AdvanceOutcome::HostRequest(request) if is_filesystem(request) => {
                    Some(copy_filesystem_request(request)?)
                }
                AdvanceOutcome::SliceExhausted => {
                    return Ok(ComputerAdvanceOutcome::SliceExhausted)
                }
                AdvanceOutcome::HostRequest(request) => {
                    return Ok(ComputerAdvanceOutcome::HostRequest(copy_host_request(
                        request,
                    )))
                }
                AdvanceOutcome::AllocationExhausted(value) => {
                    return Ok(ComputerAdvanceOutcome::AllocationExhausted(value))
                }
                AdvanceOutcome::QuotaExhausted(value) => {
                    return Ok(ComputerAdvanceOutcome::QuotaExhausted(value))
                }
                AdvanceOutcome::Halted(value) => {
                    return Ok(ComputerAdvanceOutcome::Halted(value.map(copy_value)))
                }
                AdvanceOutcome::Crashed(value) => {
                    return Ok(ComputerAdvanceOutcome::Crashed(value))
                }
                AdvanceOutcome::Faulted(value) => {
                    return Ok(ComputerAdvanceOutcome::Faulted(value))
                }
                AdvanceOutcome::HostFailed(value) => {
                    return Ok(ComputerAdvanceOutcome::HostFailed(value))
                }
            }
        };
        match internal.expect("internal request branch always publishes an action") {
            TerminalRequest::Raw { id, operation } => {
                self.handle_raw_terminal_request(id, operation)
            }
            TerminalRequest::FileSystem { id, operation } => {
                self.handle_filesystem_request(id, operation)
            }
        }
    }

    fn handle_raw_terminal_request(
        &mut self,
        id: RequestId,
        operation: RawTerminalOperation,
    ) -> Result<ComputerAdvanceOutcome, ComputerError> {
        let response = match operation {
            RawTerminalOperation::Write(units) => {
                self.terminal
                    .write_utf16(&units)
                    .map_err(|_| ComputerError::InvalidTerminalRequest)?;
                HostValueInput::Unit
            }
            RawTerminalOperation::ErasePrevious => {
                self.terminal.erase_previous();
                HostValueInput::Unit
            }
            RawTerminalOperation::Clear => {
                self.terminal.clear();
                HostValueInput::Unit
            }
            RawTerminalOperation::AwaitEvent => {
                let Some(kind) = self.terminal_await_event()? else {
                    self.pending_terminal_event = Some(id);
                    return Ok(ComputerAdvanceOutcome::WaitingForTerminalEvent);
                };
                HostValueInput::I32(kind as i32)
            }
            RawTerminalOperation::EventText => {
                let units = self
                    .terminal_event_text()?
                    .encode_utf16()
                    .collect::<Vec<_>>();
                self.session
                    .resume_internal(id, HostResponse::Success(HostValueInput::String(&units)))
                    .map_err(ComputerError::Resume)?;
                return Ok(ComputerAdvanceOutcome::SliceExhausted);
            }
            RawTerminalOperation::EventKey => {
                HostValueInput::I32(i32::from(self.terminal_event_key()?))
            }
            RawTerminalOperation::EventAction => HostValueInput::I32(self.terminal_event_action()?),
            RawTerminalOperation::EventModifiers => {
                HostValueInput::I32(i32::from(self.terminal_event_modifiers()?))
            }
            RawTerminalOperation::FinishEvent => {
                self.terminal_finish_event()?;
                HostValueInput::Unit
            }
        };
        self.session
            .resume_internal(id, HostResponse::Success(response))
            .map_err(ComputerError::Resume)?;
        Ok(ComputerAdvanceOutcome::SliceExhausted)
    }

    fn handle_filesystem_request(
        &mut self,
        id: RequestId,
        operation: FileSystemOperation,
    ) -> Result<ComputerAdvanceOutcome, ComputerError> {
        match operation {
            FileSystemOperation::Stat(path) => {
                let result = self
                    .parse_path(&path)
                    .and_then(|path| self.filesystem.stat(&self.initial_file_capability, &path))
                    .map(|metadata| match metadata.kind {
                        NodeKind::File => 1,
                        NodeKind::Directory => 2,
                    })
                    .unwrap_or_else(filesystem_error_code);
                self.resume_filesystem(id, HostResponse::Success(HostValueInput::I32(result)))?;
            }
            FileSystemOperation::List(path) => {
                let result = self
                    .parse_path(&path)
                    .and_then(|path| self.filesystem.list(&self.initial_file_capability, &path));
                match result {
                    Ok(names) => {
                        let units = names
                            .iter()
                            .enumerate()
                            .flat_map(|(index, name)| {
                                (index != 0)
                                    .then_some(0)
                                    .into_iter()
                                    .chain(name.encode_utf16())
                            })
                            .collect::<Vec<_>>();
                        if units.len() > self.maximum_text_code_units {
                            self.resume_filesystem(
                                id,
                                filesystem_failure(FileSystemError::QuotaExceeded),
                            )?;
                        } else {
                            self.resume_filesystem(
                                id,
                                HostResponse::Success(HostValueInput::String(&units)),
                            )?;
                        }
                    }
                    Err(error) => self.resume_filesystem(id, filesystem_failure(error))?,
                }
            }
            FileSystemOperation::ReadText(path) => {
                match self
                    .parse_path(&path)
                    .and_then(|path| self.read_file(&path))
                {
                    Ok(bytes) => match std::str::from_utf8(&bytes) {
                        Ok(text) => {
                            let units = text.encode_utf16().collect::<Vec<_>>();
                            if units.len() > self.maximum_text_code_units {
                                self.resume_filesystem(
                                    id,
                                    filesystem_failure(FileSystemError::QuotaExceeded),
                                )?;
                            } else {
                                self.resume_filesystem(
                                    id,
                                    HostResponse::Success(HostValueInput::String(&units)),
                                )?;
                            }
                        }
                        Err(_) => self.resume_filesystem(
                            id,
                            HostResponse::Failure(HostFailure::new(
                                HostFailureKind::InputOutput,
                                INVALID_UTF8_ERROR_CODE,
                            )),
                        )?,
                    },
                    Err(error) => self.resume_filesystem(id, filesystem_failure(error))?,
                }
            }
            FileSystemOperation::WriteText(path, value) => {
                let result = self.parse_path(&path).and_then(|path| {
                    String::from_utf16(&value)
                        .map_err(|_| FileSystemError::InvalidPath)
                        .and_then(|value| {
                            self.filesystem.write_file(
                                &self.initial_file_capability,
                                &path,
                                value.as_bytes(),
                                false,
                            )
                        })
                });
                self.resume_filesystem(
                    id,
                    HostResponse::Success(HostValueInput::I32(status_code(result))),
                )?;
            }
            FileSystemOperation::CreateDirectory(path) => {
                let result = self.parse_path(&path).and_then(|path| {
                    self.filesystem
                        .create_directory(&self.initial_file_capability, &path)
                });
                self.resume_filesystem(
                    id,
                    HostResponse::Success(HostValueInput::I32(status_code(result))),
                )?;
            }
            FileSystemOperation::Remove(path) => {
                let result = self
                    .parse_path(&path)
                    .and_then(|path| self.filesystem.remove(&self.initial_file_capability, &path));
                self.resume_filesystem(
                    id,
                    HostResponse::Success(HostValueInput::I32(status_code(result))),
                )?;
            }
            FileSystemOperation::Rename(source, destination) => {
                let result = self.parse_path(&source).and_then(|source| {
                    self.parse_path(&destination).and_then(|destination| {
                        self.filesystem.rename(
                            &self.initial_file_capability,
                            &source,
                            &destination,
                            false,
                        )
                    })
                });
                self.resume_filesystem(
                    id,
                    HostResponse::Success(HostValueInput::I32(status_code(result))),
                )?;
            }
        }
        Ok(ComputerAdvanceOutcome::SliceExhausted)
    }

    fn parse_path(&self, units: &[u16]) -> Result<VirtualPath, FileSystemError> {
        VirtualPath::parse_utf16(units, self.filesystem.limits())
    }

    fn read_file(&mut self, path: &VirtualPath) -> Result<Vec<u8>, FileSystemError> {
        let length = self
            .filesystem
            .stat(&self.initial_file_capability, path)?
            .logical_size;
        let maximum_utf8_bytes = self.maximum_text_code_units.saturating_mul(3);
        if length > maximum_utf8_bytes as u64 {
            return Err(FileSystemError::QuotaExceeded);
        }
        let handle = self
            .filesystem
            .open(&self.initial_file_capability, path, OpenMode::Read)?;
        let result = (|| {
            let capacity = usize::try_from(length).map_err(|_| FileSystemError::QuotaExceeded)?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(capacity)
                .map_err(|_| FileSystemError::QuotaExceeded)?;
            let mut offset = 0_u64;
            while offset < length {
                let chunk = self.filesystem.read(handle, offset, usize::MAX)?;
                if chunk.is_empty() {
                    return Err(FileSystemError::StorageFaulted);
                }
                offset = offset
                    .checked_add(chunk.len() as u64)
                    .ok_or(FileSystemError::StorageFaulted)?;
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        })();
        let close = self.filesystem.close(handle);
        match (result, close) {
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            (Ok(bytes), Ok(())) => Ok(bytes),
        }
    }

    fn resume_filesystem(
        &mut self,
        id: RequestId,
        response: HostResponse<'_>,
    ) -> Result<(), ComputerError> {
        self.session
            .resume_internal(id, response)
            .map_err(ComputerError::Resume)
    }

    pub fn resume_host_request(
        &mut self,
        request_id: u64,
        response: HostResponse<'_>,
    ) -> Result<(), ComputerError> {
        let request_id = RequestId::new(request_id).ok_or(ComputerError::InvalidRequestId)?;
        self.session
            .resume(request_id, response)
            .map_err(ComputerError::Resume)
    }
}

enum TerminalRequest {
    Raw {
        id: RequestId,
        operation: RawTerminalOperation,
    },
    FileSystem {
        id: RequestId,
        operation: FileSystemOperation,
    },
}

enum RawTerminalOperation {
    Write(Vec<u16>),
    ErasePrevious,
    Clear,
    AwaitEvent,
    EventText,
    EventKey,
    EventAction,
    EventModifiers,
    FinishEvent,
}

enum FileSystemOperation {
    Stat(Vec<u16>),
    List(Vec<u16>),
    ReadText(Vec<u16>),
    WriteText(Vec<u16>, Vec<u16>),
    CreateDirectory(Vec<u16>),
    Remove(Vec<u16>),
    Rename(Vec<u16>, Vec<u16>),
}

fn is_raw_terminal(request: HostRequestView<'_>) -> bool {
    request.namespace() == TERMINAL_NAMESPACE
        && request.name() == TERMINAL_NAME
        && request.abi_major() == RAW_TERMINAL_ABI_MAJOR
}

fn is_filesystem(request: HostRequestView<'_>) -> bool {
    request.namespace() == TERMINAL_NAMESPACE
        && request.name() == FILESYSTEM_NAME
        && request.abi_major() == FILESYSTEM_ABI_MAJOR
}

fn copy_filesystem_request(request: HostRequestView<'_>) -> Result<TerminalRequest, ComputerError> {
    if request.asynchronous() {
        return Err(ComputerError::InvalidFileSystemRequest);
    }
    let string = |index| match request.arguments().get(index) {
        Some(HostValueView::String(units)) => Ok(units.to_vec()),
        _ => Err(ComputerError::InvalidFileSystemRequest),
    };
    let operation = match (request.operation(), request.arguments().len()) {
        (0, 1) => FileSystemOperation::Stat(string(0)?),
        (1, 1) => FileSystemOperation::List(string(0)?),
        (2, 1) => FileSystemOperation::ReadText(string(0)?),
        (3, 2) => FileSystemOperation::WriteText(string(0)?, string(1)?),
        (4, 1) => FileSystemOperation::CreateDirectory(string(0)?),
        (5, 1) => FileSystemOperation::Remove(string(0)?),
        (6, 2) => FileSystemOperation::Rename(string(0)?, string(1)?),
        _ => return Err(ComputerError::InvalidFileSystemRequest),
    };
    Ok(TerminalRequest::FileSystem {
        id: request.id(),
        operation,
    })
}

fn status_code(result: Result<(), FileSystemError>) -> i32 {
    result.map_or_else(filesystem_error_code, |()| 0)
}

fn filesystem_error_code(error: FileSystemError) -> i32 {
    -(match error {
        FileSystemError::InvalidPath => 1,
        FileSystemError::NotFound => 2,
        FileSystemError::AlreadyExists => 3,
        FileSystemError::NotDirectory => 4,
        FileSystemError::IsDirectory => 5,
        FileSystemError::NotEmpty => 6,
        FileSystemError::ReadOnly => 7,
        FileSystemError::PermissionDenied => 8,
        FileSystemError::StaleHandle => 9,
        FileSystemError::QuotaExceeded => 10,
        FileSystemError::Busy => 11,
        FileSystemError::StorageFaulted => 12,
        FileSystemError::Closed => 13,
    })
}

fn filesystem_failure(error: FileSystemError) -> HostResponse<'static> {
    HostResponse::Failure(HostFailure::new(
        HostFailureKind::InputOutput,
        filesystem_error_code(error).unsigned_abs(),
    ))
}

const INVALID_UTF8_ERROR_CODE: u32 = 14;

fn copy_raw_terminal_request(
    request: HostRequestView<'_>,
) -> Result<TerminalRequest, ComputerError> {
    let operation = match request.operation() {
        0 if !request.asynchronous() && request.arguments().len() == 1 => {
            let Some(HostValueView::String(units)) = request.arguments().get(0) else {
                return Err(ComputerError::InvalidTerminalRequest);
            };
            RawTerminalOperation::Write(units.to_vec())
        }
        1 if !request.asynchronous() && request.arguments().is_empty() => {
            RawTerminalOperation::ErasePrevious
        }
        2 if !request.asynchronous() && request.arguments().is_empty() => {
            RawTerminalOperation::Clear
        }
        3 if request.asynchronous() && request.arguments().is_empty() => {
            RawTerminalOperation::AwaitEvent
        }
        4 if !request.asynchronous() && request.arguments().is_empty() => {
            RawTerminalOperation::EventText
        }
        5 if !request.asynchronous() && request.arguments().is_empty() => {
            RawTerminalOperation::EventKey
        }
        6 if !request.asynchronous() && request.arguments().is_empty() => {
            RawTerminalOperation::EventAction
        }
        7 if !request.asynchronous() && request.arguments().is_empty() => {
            RawTerminalOperation::EventModifiers
        }
        8 if !request.asynchronous() && request.arguments().is_empty() => {
            RawTerminalOperation::FinishEvent
        }
        _ => return Err(ComputerError::InvalidTerminalRequest),
    };
    Ok(TerminalRequest::Raw {
        id: request.id(),
        operation,
    })
}

fn copy_host_request(request: HostRequestView<'_>) -> ComputerHostRequest {
    let arguments = (0..request.arguments().len())
        .filter_map(|index| request.arguments().get(index).map(copy_value))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    ComputerHostRequest {
        id: request.id().get(),
        namespace: request.namespace().into(),
        name: request.name().into(),
        abi_major: request.abi_major(),
        abi_minor: request.abi_minor(),
        operation: request.operation(),
        arguments,
    }
}

fn copy_value(value: HostValueView<'_>) -> ComputerValue {
    match value {
        HostValueView::I32(value) => ComputerValue::I32(value),
        HostValueView::I64(value) => ComputerValue::I64(value),
        HostValueView::F32(value) => ComputerValue::F32(value),
        HostValueView::F64(value) => ComputerValue::F64(value),
        HostValueView::Bool(value) => ComputerValue::Bool(value),
        HostValueView::Char(value) => ComputerValue::Char(value),
        HostValueView::String(value) => ComputerValue::String(value.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::{
        ComputerId, FileCapability, FileRights, FileSystemError, FileSystemLimits, RomImage,
        TerminalKey, TerminalKeyEvent, TerminalModifiers, VirtualPath, WorldFileSystemStore,
    };

    #[test]
    fn raw_terminal_capability_waits_and_consumes_typed_events() {
        let artifact = crate::execution::fixtures::raw_terminal_conformance_artifact(
            &['>' as u16, ' ' as u16],
            1,
        );
        let mut constrained_profile = profile();
        constrained_profile.maximum_host_requests = 1;
        constrained_profile.maximum_accepted_responses = 1;
        let mut computer = ComputerMachine::start(artifact, constrained_profile, &[], &[]).unwrap();

        while !matches!(
            computer.advance(64, 64).unwrap(),
            ComputerAdvanceOutcome::WaitingForTerminalEvent
        ) {}
        assert_eq!(
            '>' as u32,
            computer.terminal().cell(0, 0).unwrap().code_point()
        );

        computer.terminal_mut().push_text("λ").unwrap();
        while !matches!(
            computer.advance(64, 64).unwrap(),
            ComputerAdvanceOutcome::WaitingForTerminalEvent
        ) {}

        computer
            .terminal_mut()
            .push_key(TerminalKeyEvent::new(
                TerminalKey::Enter,
                TerminalKeyAction::Press,
                TerminalModifiers::new(TerminalModifiers::CONTROL).unwrap(),
            ))
            .unwrap();
        let halted = loop {
            match computer.advance(64, 64).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::Halted(value) => break value,
                other => panic!("unexpected raw terminal outcome: {other:?}"),
            }
        };
        assert_eq!(
            Some(ComputerValue::I32(i32::from(TerminalKey::Enter.code()))),
            halted
        );
        assert_eq!(
            ComputerError::NoActiveTerminalEvent,
            computer.terminal_finish_event().unwrap_err()
        );
    }

    #[test]
    fn filesystem_capability_persists_text_and_isolates_computer_identities() {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir()
            .join("compukters-computer-filesystem-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let limits = FileSystemLimits::testing();
        let store = WorldFileSystemStore::open(&root, limits).unwrap();
        let first_id = ComputerId::from_bytes([1; 16]);
        let second_id = ComputerId::from_bytes([2; 16]);
        let owner = FileCapability::new(path("/home", &limits), FileRights::OWNER);
        let filesystem = store
            .open_computer(first_id, Arc::new(empty_rom(&limits)))
            .unwrap();
        let artifact = crate::execution::fixtures::filesystem_conformance_artifact();
        let mut computer = ComputerMachine::start_in_filesystem(
            artifact,
            profile(),
            &[],
            &[],
            filesystem,
            owner.clone(),
        )
        .unwrap();

        let result = loop {
            match computer.advance(128, 128).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(value))) => break value,
                other => panic!("unexpected filesystem conformance outcome: {other:?}"),
            }
        };
        assert_eq!(filesystem_status(), result);
        let generation = computer.filesystem.generation();
        drop(computer);
        store.flush(first_id, generation).unwrap();

        let first = store
            .open_computer(first_id, Arc::new(empty_rom(&limits)))
            .unwrap();
        assert_eq!(
            b"fun main() = 42\n",
            first
                .read_file_for_test(&path("/home/project/main.kt", &limits))
                .unwrap()
                .as_slice(),
        );
        let second = store
            .open_computer(second_id, Arc::new(empty_rom(&limits)))
            .unwrap();
        assert_eq!(
            Err(FileSystemError::NotFound),
            second.stat(&owner, &path("/home/project/main.kt", &limits))
        );
        store.close().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn filesystem_text_response_is_bounded_before_guest_materialization() {
        let mut constrained = profile();
        constrained.maximum_inbound_utf16_code_units = 2;
        let mut computer = ComputerMachine::start(
            crate::execution::fixtures::filesystem_conformance_artifact(),
            constrained,
            &[],
            &[],
        )
        .unwrap();

        loop {
            match computer.advance(128, 128).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::HostFailed(failure) => {
                    assert_eq!(HostFailureKind::InputOutput, failure.kind());
                    assert_eq!(10, failure.code());
                    break;
                }
                other => panic!("unexpected bounded filesystem outcome: {other:?}"),
            }
        }
    }

    fn path(value: &str, limits: &FileSystemLimits) -> VirtualPath {
        VirtualPath::parse_utf8(value, limits).unwrap()
    }

    fn empty_rom(limits: &FileSystemLimits) -> RomImage {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"CPKTROM\0");
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        let digest = Sha256::digest(&bytes);
        bytes.extend_from_slice(&digest);
        RomImage::admit(bytes.into(), limits).unwrap()
    }

    fn filesystem_status() -> i32 {
        "fun main() = 42\n"
            .encode_utf16()
            .fold(0_i32, |hash, unit| {
                hash.wrapping_mul(31).wrapping_add(i32::from(unit))
            })
            .wrapping_add("main.kt".encode_utf16().fold(0_i32, |hash, unit| {
                hash.wrapping_mul(31).wrapping_add(i32::from(unit))
            }))
    }

    fn profile() -> ExecutionProfile {
        ExecutionProfile {
            heap_bytes: 1024 * 1024,
            frame_storage_bytes: 1024 * 1024,
            maximum_call_depth: 64,
            maximum_coroutines: 64,
            maximum_host_requests: 64,
            maximum_events: 64,
            maximum_slice_budget: u32::MAX,
            compiler_abi: [0; 32],
            standard_library_abi: [0; 32],
            maximum_host_arguments: 16,
            maximum_outbound_utf16_code_units: 4096,
            maximum_inbound_utf16_code_units: 4096,
            maximum_accepted_responses: 64,
        }
    }
}
