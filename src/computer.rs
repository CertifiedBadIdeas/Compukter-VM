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

use crate::process::{OwnedCapabilityBinding, MAXIMUM_ADDON_CAPABILITIES};
use crate::{
    verify_artifact, AdmissionError, AdvanceOutcome, ArtifactLimits, CapabilityBinding,
    ComputerFileSystem, EntryValue, ExecutionProfile, FileCapability, FileRights, FileSystemError,
    FileSystemLimits, GuestTrap, HostFailure, HostFailureKind, HostRequestView, HostResponse,
    HostValueInput, HostValueType, HostValueView, ManagedAllocationFailure, NodeKind, OpenMode,
    OperationSchema, ProcessCapabilityMask, ProcessLimits, ProcessResult, QuotaExhaustion,
    RequestId, ResumeError, RunError, Session, TerminalDevice, TerminalInputEvent,
    TerminalKeyAction, VerifiedArtifact, VirtualPath, VmFault,
};

const TERMINAL_NAMESPACE: &str = "compukter";
const TERMINAL_NAME: &str = "terminal";
const RAW_TERMINAL_ABI_MAJOR: u16 = 2;
const FILESYSTEM_NAME: &str = "filesystem";
const FILESYSTEM_ABI_MAJOR: u16 = 1;
const PROCESS_NAME: &str = "process";
const PROCESS_ABI_MAJOR: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComputerStartError {
    Admission(AdmissionError),
    Start(RunError),
    Process(ProcessResult),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComputerError {
    Run(RunError),
    Resume(ResumeError),
    InvalidRequestId,
    InvalidTerminalRequest,
    InvalidFileSystemRequest,
    InvalidProcessRequest,
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
    sessions: Vec<ProcessFrame>,
    terminal: TerminalDevice,
    active_terminal_event: Option<TerminalInputEvent>,
    active_terminal_event_owner: Option<usize>,
    filesystem: ComputerFileSystem,
    initial_file_capability: FileCapability,
    profile: ExecutionProfile,
    addon_bindings: Box<[OwnedCapabilityBinding]>,
    process_limits: ProcessLimits,
    process_starts: u64,
    reserved_heap_bytes: u64,
    reserved_frame_storage_bytes: u64,
    maximum_text_code_units: usize,
}

#[derive(Debug)]
struct ProcessFrame {
    session: Session,
    capabilities: ProcessCapabilityMask,
    pending_terminal_event: Option<RequestId>,
    pending_process: Option<RequestId>,
}

impl ComputerMachine {
    pub fn boot_in_filesystem(
        profile: ExecutionProfile,
        process_limits: ProcessLimits,
        addon_bindings: &[CapabilityBinding<'_>],
        mut filesystem: ComputerFileSystem,
        initial_capability: FileCapability,
    ) -> Result<Self, ComputerStartError> {
        let path = VirtualPath::parse_utf8("/rom/boot", filesystem.limits())
            .expect("the fixed boot path is canonical");
        let bytes = filesystem
            .read_executable(&initial_capability, &path)
            .map_err(|error| ComputerStartError::Process(process_filesystem_result(error)))?;
        let artifact = verify_artifact(bytes.into(), ArtifactLimits::default())
            .map_err(|_| ComputerStartError::Process(ProcessResult::InvalidArtifact))?;
        Self::start_in_filesystem_with_process_limits(
            artifact,
            profile,
            process_limits,
            addon_bindings,
            &[],
            filesystem,
            initial_capability,
        )
    }

    pub fn start(
        artifact: VerifiedArtifact,
        profile: ExecutionProfile,
        addon_bindings: &[CapabilityBinding<'_>],
        arguments: &[EntryValue],
    ) -> Result<Self, ComputerStartError> {
        Self::start_with_process_limits(
            artifact,
            profile,
            ProcessLimits::default(),
            addon_bindings,
            arguments,
        )
    }

    pub fn start_with_process_limits(
        artifact: VerifiedArtifact,
        profile: ExecutionProfile,
        process_limits: ProcessLimits,
        addon_bindings: &[CapabilityBinding<'_>],
        arguments: &[EntryValue],
    ) -> Result<Self, ComputerStartError> {
        let limits = FileSystemLimits::default();
        let initial_capability = FileCapability::new(
            VirtualPath::parse_utf8("/home", &limits).expect("fixed ephemeral filesystem path"),
            FileRights::OWNER,
        );
        Self::start_in_filesystem_with_process_limits(
            artifact,
            profile,
            process_limits,
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
        Self::start_in_filesystem_with_process_limits(
            artifact,
            profile,
            ProcessLimits::default(),
            addon_bindings,
            arguments,
            filesystem,
            initial_capability,
        )
    }

    pub fn start_in_filesystem_with_process_limits(
        artifact: VerifiedArtifact,
        profile: ExecutionProfile,
        process_limits: ProcessLimits,
        addon_bindings: &[CapabilityBinding<'_>],
        arguments: &[EntryValue],
        filesystem: ComputerFileSystem,
        initial_capability: FileCapability,
    ) -> Result<Self, ComputerStartError> {
        let maximum_text_code_units = profile.maximum_inbound_utf16_code_units as usize;
        if addon_bindings.len() > MAXIMUM_ADDON_CAPABILITIES {
            return Err(ComputerStartError::Process(
                ProcessResult::InvalidCapabilities,
            ));
        }
        let owned_addon_bindings = addon_bindings
            .iter()
            .map(OwnedCapabilityBinding::copy_from)
            .collect::<Box<[_]>>();
        let addon_mask = if owned_addon_bindings.is_empty() {
            0
        } else {
            u32::MAX >> (32 - owned_addon_bindings.len()) << 3
        };
        let available_capabilities = ProcessCapabilityMask::STANDARD | addon_mask;
        let reserved_heap_bytes = u64::from(profile.heap_bytes);
        let reserved_frame_storage_bytes = profile.frame_storage_bytes;
        if reserved_heap_bytes > process_limits.maximum_aggregate_heap_bytes
            || reserved_frame_storage_bytes > process_limits.maximum_aggregate_frame_storage_bytes
        {
            return Err(ComputerStartError::Process(ProcessResult::QuotaExhausted));
        }
        let root_capabilities = ProcessCapabilityMask::from_bits(available_capabilities);
        let mut session = admit_session(
            artifact,
            profile.clone(),
            &owned_addon_bindings,
            root_capabilities,
        )
        .map_err(ComputerStartError::Admission)?;
        session
            .start(arguments)
            .map_err(ComputerStartError::Start)?;
        Ok(Self {
            sessions: vec![ProcessFrame {
                session,
                capabilities: root_capabilities,
                pending_terminal_event: None,
                pending_process: None,
            }],
            terminal: TerminalDevice::default(),
            active_terminal_event: None,
            active_terminal_event_owner: None,
            filesystem,
            initial_file_capability: initial_capability,
            profile,
            addon_bindings: owned_addon_bindings,
            process_limits,
            process_starts: 1,
            reserved_heap_bytes,
            reserved_frame_storage_bytes,
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
        self.active_terminal_event_owner = Some(self.sessions.len());
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
        let result = self
            .active_terminal_event
            .take()
            .map(|_| ())
            .ok_or(ComputerError::NoActiveTerminalEvent);
        if result.is_ok() {
            self.active_terminal_event_owner = None;
        }
        result
    }

    pub fn advance(
        &mut self,
        guest_budget: u32,
        maintenance_budget: u32,
    ) -> Result<ComputerAdvanceOutcome, ComputerError> {
        if let Some(request) = self.active_frame().pending_terminal_event {
            let Some(kind) = self.terminal_await_event()? else {
                return Ok(ComputerAdvanceOutcome::WaitingForTerminalEvent);
            };
            self.active_session_mut()
                .resume_internal(
                    request,
                    HostResponse::Success(HostValueInput::I32(kind as i32)),
                )
                .map_err(ComputerError::Resume)?;
            self.active_frame_mut().pending_terminal_event = None;
            return Ok(ComputerAdvanceOutcome::SliceExhausted);
        }
        let internal = {
            let outcome = self
                .active_session_mut()
                .advance(guest_budget, maintenance_budget)
                .map_err(ComputerError::Run)?;
            match outcome {
                AdvanceOutcome::HostRequest(request) if is_raw_terminal(request) => {
                    Some(copy_raw_terminal_request(request)?)
                }
                AdvanceOutcome::HostRequest(request) if is_filesystem(request) => {
                    Some(copy_filesystem_request(request)?)
                }
                AdvanceOutcome::HostRequest(request) if is_process(request) => {
                    Some(copy_process_request(request)?)
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
                    if self.sessions.len() > 1 {
                        return self.finish_child(ProcessResult::AllocationExhausted);
                    }
                    return Ok(ComputerAdvanceOutcome::AllocationExhausted(value));
                }
                AdvanceOutcome::QuotaExhausted(value) => {
                    if self.sessions.len() > 1 {
                        return self.finish_child(ProcessResult::QuotaExhausted);
                    }
                    return Ok(ComputerAdvanceOutcome::QuotaExhausted(value));
                }
                AdvanceOutcome::Halted(value) => {
                    let value = value.map(copy_value);
                    if self.sessions.len() > 1 {
                        return self.finish_child(ProcessResult::Exited);
                    }
                    return Ok(ComputerAdvanceOutcome::Halted(value));
                }
                AdvanceOutcome::Crashed(value) => {
                    if self.sessions.len() > 1 {
                        return self.finish_child(ProcessResult::Trapped);
                    }
                    return Ok(ComputerAdvanceOutcome::Crashed(value));
                }
                AdvanceOutcome::Faulted(value) => {
                    if self.sessions.len() > 1 {
                        return self.finish_child(ProcessResult::Faulted);
                    }
                    return Ok(ComputerAdvanceOutcome::Faulted(value));
                }
                AdvanceOutcome::HostFailed(value) => {
                    if self.sessions.len() > 1 {
                        return self.finish_child(ProcessResult::HostFailed);
                    }
                    return Ok(ComputerAdvanceOutcome::HostFailed(value));
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
            TerminalRequest::Process { id, operation } => {
                self.handle_process_request(id, operation)
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
                    self.active_frame_mut().pending_terminal_event = Some(id);
                    return Ok(ComputerAdvanceOutcome::WaitingForTerminalEvent);
                };
                HostValueInput::I32(kind as i32)
            }
            RawTerminalOperation::EventText => {
                let units = self
                    .terminal_event_text()?
                    .encode_utf16()
                    .collect::<Vec<_>>();
                self.active_session_mut()
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
        self.active_session_mut()
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
            FileSystemOperation::InstallExecutableFromRom(source, destination) => {
                let result = self.parse_path(&source).and_then(|source| {
                    self.parse_path(&destination).and_then(|destination| {
                        self.filesystem.install_executable_from_rom(
                            &self.initial_file_capability,
                            &source,
                            &destination,
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
        self.active_session_mut()
            .resume_internal(id, response)
            .map_err(ComputerError::Resume)
    }

    pub fn resume_host_request(
        &mut self,
        request_id: u64,
        response: HostResponse<'_>,
    ) -> Result<(), ComputerError> {
        let request_id = RequestId::new(request_id).ok_or(ComputerError::InvalidRequestId)?;
        self.active_session_mut()
            .resume(request_id, response)
            .map_err(ComputerError::Resume)
    }

    fn handle_process_request(
        &mut self,
        id: RequestId,
        operation: ProcessOperation,
    ) -> Result<ComputerAdvanceOutcome, ComputerError> {
        let ProcessOperation::Run(path, requested_capabilities) = operation;
        let capabilities = match self
            .active_frame()
            .capabilities
            .delegate(requested_capabilities)
        {
            Ok(capabilities) => capabilities,
            Err(_) => return self.resume_process_result(id, ProcessResult::InvalidCapabilities),
        };
        let path = match self.parse_path(&path) {
            Ok(path) => path,
            Err(_) => return self.resume_process_result(id, ProcessResult::InvalidPath),
        };
        let bytes = match self
            .filesystem
            .read_executable(&self.initial_file_capability, &path)
        {
            Ok(bytes) => bytes,
            Err(error) => return self.resume_process_result(id, process_filesystem_result(error)),
        };
        let artifact = match verify_artifact(bytes.into(), ArtifactLimits::default()) {
            Ok(artifact) => artifact,
            Err(_) => return self.resume_process_result(id, ProcessResult::InvalidArtifact),
        };
        if self.sessions.len() >= self.process_limits.maximum_depth as usize {
            return self.resume_process_result(id, ProcessResult::DepthLimit);
        }
        if self.process_starts >= self.process_limits.maximum_starts {
            return self.resume_process_result(id, ProcessResult::StartLimit);
        }
        let Some(reserved_heap_bytes) = self
            .reserved_heap_bytes
            .checked_add(u64::from(self.profile.heap_bytes))
        else {
            return self.resume_process_result(id, ProcessResult::QuotaExhausted);
        };
        let Some(reserved_frame_storage_bytes) = self
            .reserved_frame_storage_bytes
            .checked_add(self.profile.frame_storage_bytes)
        else {
            return self.resume_process_result(id, ProcessResult::QuotaExhausted);
        };
        if reserved_heap_bytes > self.process_limits.maximum_aggregate_heap_bytes
            || reserved_frame_storage_bytes
                > self.process_limits.maximum_aggregate_frame_storage_bytes
        {
            return self.resume_process_result(id, ProcessResult::QuotaExhausted);
        }
        let mut child = match admit_session(
            artifact,
            self.profile.clone(),
            &self.addon_bindings,
            capabilities,
        ) {
            Ok(session) => session,
            Err(_) => return self.resume_process_result(id, ProcessResult::AdmissionFailed),
        };
        if child.start(&[]).is_err() {
            return self.resume_process_result(id, ProcessResult::StartFailed);
        }
        self.active_frame_mut().pending_process = Some(id);
        self.process_starts += 1;
        self.reserved_heap_bytes = reserved_heap_bytes;
        self.reserved_frame_storage_bytes = reserved_frame_storage_bytes;
        self.sessions.push(ProcessFrame {
            session: child,
            capabilities,
            pending_terminal_event: None,
            pending_process: None,
        });
        Ok(ComputerAdvanceOutcome::SliceExhausted)
    }

    fn finish_child(
        &mut self,
        result: ProcessResult,
    ) -> Result<ComputerAdvanceOutcome, ComputerError> {
        let child_depth = self.sessions.len();
        self.sessions.pop();
        self.reserved_heap_bytes -= u64::from(self.profile.heap_bytes);
        self.reserved_frame_storage_bytes -= self.profile.frame_storage_bytes;
        if self.active_terminal_event_owner == Some(child_depth) {
            self.active_terminal_event = None;
            self.active_terminal_event_owner = None;
        }
        let request = self
            .active_frame_mut()
            .pending_process
            .take()
            .expect("a child session always has a suspended parent request");
        self.active_session_mut()
            .resume_internal(
                request,
                HostResponse::Success(HostValueInput::I32(result.code())),
            )
            .map_err(ComputerError::Resume)?;
        Ok(ComputerAdvanceOutcome::SliceExhausted)
    }

    fn resume_process_result(
        &mut self,
        id: RequestId,
        result: ProcessResult,
    ) -> Result<ComputerAdvanceOutcome, ComputerError> {
        self.active_session_mut()
            .resume_internal(
                id,
                HostResponse::Success(HostValueInput::I32(result.code())),
            )
            .map_err(ComputerError::Resume)?;
        Ok(ComputerAdvanceOutcome::SliceExhausted)
    }

    fn active_frame(&self) -> &ProcessFrame {
        self.sessions
            .last()
            .expect("a computer always has a root process")
    }

    fn active_frame_mut(&mut self) -> &mut ProcessFrame {
        self.sessions
            .last_mut()
            .expect("a computer always has a root process")
    }

    fn active_session_mut(&mut self) -> &mut Session {
        &mut self.active_frame_mut().session
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
    Process {
        id: RequestId,
        operation: ProcessOperation,
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
    InstallExecutableFromRom(Vec<u16>, Vec<u16>),
}

enum ProcessOperation {
    Run(Vec<u16>, i32),
}

fn admit_session(
    artifact: VerifiedArtifact,
    profile: ExecutionProfile,
    addon_bindings: &[OwnedCapabilityBinding],
    capabilities: ProcessCapabilityMask,
) -> Result<Session, AdmissionError> {
    let string_argument = [HostValueType::String];
    let two_string_arguments = [HostValueType::String, HostValueType::String];
    let process_arguments = [HostValueType::String, HostValueType::I32];
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
    let filesystem_operations = [
        OperationSchema::synchronous(&string_argument, HostValueType::I32),
        OperationSchema::synchronous(&string_argument, HostValueType::String),
        OperationSchema::synchronous(&string_argument, HostValueType::String),
        OperationSchema::synchronous(&two_string_arguments, HostValueType::I32),
        OperationSchema::synchronous(&string_argument, HostValueType::I32),
        OperationSchema::synchronous(&string_argument, HostValueType::I32),
        OperationSchema::synchronous(&two_string_arguments, HostValueType::I32),
        OperationSchema::synchronous(&two_string_arguments, HostValueType::I32),
    ];
    let process_operations = [OperationSchema::asynchronous(
        &process_arguments,
        HostValueType::I32,
    )];
    let addon_operations = addon_bindings
        .iter()
        .map(|binding| {
            binding
                .operations()
                .iter()
                .map(|operation| OperationSchema {
                    arguments: operation.arguments(),
                    result: operation.result(),
                    asynchronous: operation.asynchronous(),
                })
                .collect::<Box<[_]>>()
        })
        .collect::<Box<[_]>>();
    let mut bindings = Vec::with_capacity(addon_bindings.len() + 3);
    for (index, binding) in addon_bindings.iter().enumerate() {
        if capabilities.allows(1 << (index + 3)) {
            bindings.push(CapabilityBinding::new(
                binding.namespace(),
                binding.name(),
                binding.abi_major(),
                binding.abi_minor(),
                &addon_operations[index],
            ));
        }
    }
    if capabilities.allows(ProcessCapabilityMask::TERMINAL) {
        bindings.push(CapabilityBinding::new(
            TERMINAL_NAMESPACE,
            TERMINAL_NAME,
            RAW_TERMINAL_ABI_MAJOR,
            0,
            &raw_terminal_operations,
        ));
    }
    if capabilities.allows(ProcessCapabilityMask::FILESYSTEM) {
        bindings.push(CapabilityBinding::new(
            TERMINAL_NAMESPACE,
            FILESYSTEM_NAME,
            FILESYSTEM_ABI_MAJOR,
            0,
            &filesystem_operations,
        ));
    }
    if capabilities.allows(ProcessCapabilityMask::PROCESS) {
        bindings.push(CapabilityBinding::new(
            TERMINAL_NAMESPACE,
            PROCESS_NAME,
            PROCESS_ABI_MAJOR,
            0,
            &process_operations,
        ));
    }
    Session::admit(artifact, profile, &bindings)
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

fn is_process(request: HostRequestView<'_>) -> bool {
    request.namespace() == TERMINAL_NAMESPACE
        && request.name() == PROCESS_NAME
        && request.abi_major() == PROCESS_ABI_MAJOR
}

fn copy_process_request(request: HostRequestView<'_>) -> Result<TerminalRequest, ComputerError> {
    if !request.asynchronous() || request.operation() != 0 || request.arguments().len() != 2 {
        return Err(ComputerError::InvalidProcessRequest);
    }
    let Some(HostValueView::String(path)) = request.arguments().get(0) else {
        return Err(ComputerError::InvalidProcessRequest);
    };
    let Some(HostValueView::I32(capabilities)) = request.arguments().get(1) else {
        return Err(ComputerError::InvalidProcessRequest);
    };
    Ok(TerminalRequest::Process {
        id: request.id(),
        operation: ProcessOperation::Run(path.to_vec(), capabilities),
    })
}

fn process_filesystem_result(error: FileSystemError) -> ProcessResult {
    match error {
        FileSystemError::InvalidPath => ProcessResult::InvalidPath,
        FileSystemError::NotFound => ProcessResult::NotFound,
        FileSystemError::PermissionDenied | FileSystemError::ReadOnly => {
            ProcessResult::PermissionDenied
        }
        FileSystemError::NotExecutable
        | FileSystemError::IsDirectory
        | FileSystemError::NotDirectory => ProcessResult::NotExecutable,
        FileSystemError::QuotaExceeded => ProcessResult::QuotaExhausted,
        FileSystemError::AlreadyExists
        | FileSystemError::NotEmpty
        | FileSystemError::StaleHandle
        | FileSystemError::Busy
        | FileSystemError::StorageFaulted
        | FileSystemError::Closed => ProcessResult::IoFailed,
    }
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
        (7, 2) => FileSystemOperation::InstallExecutableFromRom(string(0)?, string(1)?),
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
        FileSystemError::NotExecutable => 14,
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
        ComputerId, FileCapability, FileRights, FileSystemError, FileSystemLimits, ProcessLimits,
        ProcessResult, RomImage, TerminalKey, TerminalKeyEvent, TerminalModifiers, VirtualPath,
        WorldFileSystemStore,
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
    fn process_run_suspends_parent_until_child_halts() {
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(path("/home", &limits), FileRights::OWNER);
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        let child = crate::execution::fixtures::two_block_artifact(1, 1);
        let child_bytes = crate::test_encode::encode_artifact(child.decoded()).unwrap();
        filesystem
            .write_file(&owner, &path("/home/child", &limits), &child_bytes, true)
            .unwrap();
        let parent = crate::execution::fixtures::process_run_artifact(
            &"/home/child".encode_utf16().collect::<Vec<_>>(),
            0,
        );
        let mut computer =
            ComputerMachine::start_in_filesystem(parent, profile(), &[], &[], filesystem, owner)
                .unwrap();

        assert_eq!(1, computer.sessions.len());
        while computer.sessions.len() == 1 {
            assert_eq!(
                ComputerAdvanceOutcome::SliceExhausted,
                computer.advance(64, 64).unwrap()
            );
        }
        assert_eq!(2, computer.sessions.len());
        let halted = loop {
            match computer.advance(64, 64).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::Halted(value) => break value,
                other => panic!("unexpected process outcome: {other:?}"),
            }
        };
        assert_eq!(
            Some(ComputerValue::I32(ProcessResult::Exited.code())),
            halted
        );
        assert_eq!(1, computer.sessions.len());
    }

    #[test]
    fn filesystem_installs_an_executable_from_rom_without_host_file_access() {
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(path("/home", &limits), FileRights::OWNER);
        let child = crate::execution::fixtures::two_block_artifact(1, 1);
        let child_bytes = crate::test_encode::encode_artifact(child.decoded()).unwrap();
        let filesystem = ComputerFileSystem::with_rom(
            limits,
            rom_with_executable("/rom/hello", &child_bytes, &limits),
        )
        .unwrap();
        let installer = crate::execution::fixtures::process_install_rom_executable_artifact();
        let mut computer =
            ComputerMachine::start_in_filesystem(installer, profile(), &[], &[], filesystem, owner)
                .unwrap();

        let halted = loop {
            match computer.advance(64, 64).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::Halted(value) => break value,
                other => panic!("unexpected ROM installer outcome: {other:?}"),
            }
        };

        assert_eq!(Some(ComputerValue::I32(0)), halted);
        assert_eq!(
            child_bytes,
            computer
                .filesystem
                .read_executable(
                    &computer.initial_file_capability,
                    &path("/home/hello", &limits),
                )
                .unwrap(),
        );
        assert_eq!(
            Err(FileSystemError::AlreadyExists),
            computer.filesystem.install_executable_from_rom(
                &computer.initial_file_capability,
                &path("/rom/hello", &limits),
                &path("/home/hello", &limits),
            ),
        );
        assert_eq!(
            Err(FileSystemError::PermissionDenied),
            computer.filesystem.install_executable_from_rom(
                &computer.initial_file_capability,
                &path("/home/hello", &limits),
                &path("/home/copy", &limits),
            ),
        );
    }

    #[test]
    fn process_result_matrix() {
        let ordinary_child = crate::execution::fixtures::two_block_artifact(1, 1);
        let child_bytes = crate::test_encode::encode_artifact(ordinary_child.decoded()).unwrap();
        let terminal_child = crate::execution::fixtures::raw_terminal_conformance_artifact(&[], 1);
        let terminal_child_bytes =
            crate::test_encode::encode_artifact(terminal_child.decoded()).unwrap();
        let typed_child = crate::execution::fixtures::typed_entry_artifact();
        let typed_child_bytes = crate::test_encode::encode_artifact(typed_child.decoded()).unwrap();
        let trapped_child = crate::execution::fixtures::trap_after_write_artifact(7);
        let trapped_child_bytes =
            crate::test_encode::encode_artifact(trapped_child.decoded()).unwrap();
        let allocating_child = crate::execution::fixtures::array_allocation_artifact(100);
        let allocating_child_bytes =
            crate::test_encode::encode_artifact(allocating_child.decoded()).unwrap();

        assert_eq!(
            ProcessResult::InvalidCapabilities.code(),
            process_result(None, 8, ProcessLimits::default(), profile()),
        );
        assert_eq!(
            ProcessResult::InvalidCapabilities.code(),
            process_result(None, -1, ProcessLimits::default(), profile()),
        );
        assert_eq!(
            ProcessResult::NotFound.code(),
            process_result(None, 0, ProcessLimits::default(), profile()),
        );
        assert_eq!(
            ProcessResult::NotExecutable.code(),
            process_result(
                Some((&child_bytes, false)),
                0,
                ProcessLimits::default(),
                profile(),
            ),
        );
        assert_eq!(
            ProcessResult::InvalidArtifact.code(),
            process_result(
                Some((b"not an artifact", true)),
                0,
                ProcessLimits::default(),
                profile(),
            ),
        );
        assert_eq!(
            ProcessResult::AdmissionFailed.code(),
            process_result(
                Some((&terminal_child_bytes, true)),
                0,
                ProcessLimits::default(),
                profile(),
            ),
        );
        assert_eq!(
            ProcessResult::StartFailed.code(),
            process_result(
                Some((&typed_child_bytes, true)),
                0,
                ProcessLimits::default(),
                profile(),
            ),
        );
        assert_eq!(
            ProcessResult::Trapped.code(),
            process_result(
                Some((&trapped_child_bytes, true)),
                0,
                ProcessLimits::default(),
                profile(),
            ),
        );
        assert_eq!(
            ProcessResult::AllocationExhausted.code(),
            process_result(
                Some((&allocating_child_bytes, true)),
                0,
                ProcessLimits::default(),
                ExecutionProfile {
                    heap_bytes: 64,
                    ..profile()
                },
            ),
        );
        assert_eq!(
            ProcessResult::DepthLimit.code(),
            process_result(
                Some((&child_bytes, true)),
                0,
                ProcessLimits::new(1, 8, 8 << 20, 8 << 20).unwrap(),
                profile(),
            ),
        );
        assert_eq!(
            ProcessResult::StartLimit.code(),
            process_result(
                Some((&child_bytes, true)),
                0,
                ProcessLimits::new(8, 1, 8 << 20, 8 << 20).unwrap(),
                profile(),
            ),
        );
        assert_eq!(
            ProcessResult::QuotaExhausted.code(),
            process_result(
                Some((&child_bytes, true)),
                0,
                ProcessLimits::new(8, 8, 1024 * 1024, 8 << 20).unwrap(),
                profile(),
            ),
        );
        assert_eq!(
            ProcessResult::QuotaExhausted.code(),
            process_result(
                Some((&child_bytes, true)),
                0,
                ProcessLimits::new(8, 8, 8 << 20, 1024 * 1024).unwrap(),
                profile(),
            ),
        );
    }

    #[test]
    fn process_addon_mask_filters_child_and_maps_host_failure() {
        let child = crate::execution::fixtures::capability_artifact(true, true, 1, 0);
        let child_bytes = crate::test_encode::encode_artifact(child.decoded()).unwrap();
        let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let addon = CapabilityBinding::new("app", "entry", 1, 2, &operations);
        let owned = [OwnedCapabilityBinding::copy_from(&addon)];
        admit_session(
            child.clone(),
            profile(),
            &owned,
            ProcessCapabilityMask::from_bits(1 << 3),
        )
        .unwrap();

        assert_eq!(
            ProcessResult::AdmissionFailed.code(),
            process_result_with_addons(&child_bytes, 0, &[addon], None),
        );
        assert_eq!(
            ProcessResult::HostFailed.code(),
            process_result_with_addons(
                &child_bytes,
                1 << 3,
                &[addon],
                Some(HostFailure::new(HostFailureKind::Unavailable, 17)),
            ),
        );
    }

    #[test]
    fn process_maps_child_quota_and_fault_without_terminating_parent() {
        let quota_child = crate::execution::fixtures::two_unit_capability_calls_artifact();
        let quota_bytes = crate::test_encode::encode_artifact(quota_child.decoded()).unwrap();
        let mut constrained = profile();
        constrained.maximum_accepted_responses = 1;
        let mut quota_computer = addon_process_computer(&quota_bytes, constrained);
        let quota_result = loop {
            match quota_computer.advance(64, 64).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::HostRequest(request) => quota_computer
                    .resume_host_request(request.id, HostResponse::Success(HostValueInput::Unit))
                    .unwrap(),
                ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(result))) => break result,
                other => panic!("unexpected quota process outcome: {other:?}"),
            }
        };
        assert_eq!(ProcessResult::QuotaExhausted.code(), quota_result);

        let fault_child = crate::execution::fixtures::capability_artifact(true, true, 1, 0);
        let fault_bytes = crate::test_encode::encode_artifact(fault_child.decoded()).unwrap();
        let mut fault_computer = addon_process_computer(&fault_bytes, profile());
        while fault_computer.sessions.len() == 1 {
            assert_eq!(
                ComputerAdvanceOutcome::SliceExhausted,
                fault_computer.advance(64, 64).unwrap(),
            );
        }
        fault_computer
            .active_session_mut()
            .test_set_next_request_id(u64::MAX);
        let fault_result = loop {
            match fault_computer.advance(64, 64).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(result))) => break result,
                other => panic!("unexpected faulted process outcome: {other:?}"),
            }
        };
        assert_eq!(ProcessResult::Faulted.code(), fault_result);
    }

    fn addon_process_computer(
        child: &[u8],
        execution_profile: ExecutionProfile,
    ) -> ComputerMachine {
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(path("/home", &limits), FileRights::OWNER);
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        filesystem
            .write_file(
                &owner,
                &path("/home/child", filesystem.limits()),
                child,
                true,
            )
            .unwrap();
        let parent = crate::execution::fixtures::process_run_artifact(
            &"/home/child".encode_utf16().collect::<Vec<_>>(),
            1 << 3,
        );
        let operations = [OperationSchema::asynchronous(&[], HostValueType::Unit)];
        let addon = CapabilityBinding::new("app", "entry", 1, 2, &operations);
        ComputerMachine::start_in_filesystem(
            parent,
            execution_profile,
            &[addon],
            &[],
            filesystem,
            owner,
        )
        .unwrap()
    }

    fn process_result_with_addons(
        child: &[u8],
        capabilities: i32,
        addons: &[CapabilityBinding<'_>],
        failure: Option<HostFailure>,
    ) -> i32 {
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(path("/home", &limits), FileRights::OWNER);
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        filesystem
            .write_file(
                &owner,
                &path("/home/child", filesystem.limits()),
                child,
                true,
            )
            .unwrap();
        let parent = crate::execution::fixtures::process_run_artifact(
            &"/home/child".encode_utf16().collect::<Vec<_>>(),
            capabilities,
        );
        let mut computer =
            ComputerMachine::start_in_filesystem(parent, profile(), addons, &[], filesystem, owner)
                .unwrap();
        loop {
            match computer.advance(64, 64).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::HostRequest(request) => {
                    assert_eq!(("app", "entry"), (&*request.namespace, &*request.name));
                    computer.terminal_mut().push_text("x").unwrap();
                    assert_eq!(
                        Some(ComputerTerminalEventKind::Text),
                        computer.terminal_await_event().unwrap(),
                    );
                    computer
                        .resume_host_request(
                            request.id,
                            HostResponse::Failure(failure.expect("host failure is configured")),
                        )
                        .unwrap();
                }
                ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(result))) => {
                    assert_eq!(
                        ComputerError::NoActiveTerminalEvent,
                        computer.terminal_finish_event().unwrap_err(),
                    );
                    return result;
                }
                other => panic!("unexpected addon process outcome: {other:?}"),
            }
        }
    }

    fn process_result(
        child: Option<(&[u8], bool)>,
        capabilities: i32,
        process_limits: ProcessLimits,
        execution_profile: ExecutionProfile,
    ) -> i32 {
        let limits = FileSystemLimits::testing();
        let owner = FileCapability::new(path("/home", &limits), FileRights::OWNER);
        let mut filesystem = ComputerFileSystem::with_limits(limits);
        if let Some((bytes, executable)) = child {
            filesystem
                .write_file(
                    &owner,
                    &path("/home/child", filesystem.limits()),
                    bytes,
                    executable,
                )
                .unwrap();
        }
        let parent = crate::execution::fixtures::process_run_artifact(
            &"/home/child".encode_utf16().collect::<Vec<_>>(),
            capabilities,
        );
        let mut computer = ComputerMachine::start_in_filesystem_with_process_limits(
            parent,
            execution_profile,
            process_limits,
            &[],
            &[],
            filesystem,
            owner,
        )
        .unwrap();
        loop {
            match computer.advance(64, 64).unwrap() {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::Halted(Some(ComputerValue::I32(result))) => return result,
                other => panic!("unexpected process result outcome: {other:?}"),
            }
        }
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

    #[test]
    fn filesystem_game_test_artifacts_are_committed_and_reproducible() {
        for (name, artifact) in filesystem_game_test_artifacts() {
            let generated = crate::test_encode::encode_artifact(artifact.decoded()).unwrap();
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name);
            assert_eq!(std::fs::read(path).unwrap(), generated, "{name} changed");
        }
    }

    #[test]
    #[ignore = "explicitly rewrites committed GameTest artifacts"]
    fn regenerate_filesystem_game_test_artifacts() {
        for (name, artifact) in filesystem_game_test_artifacts() {
            let generated = crate::test_encode::encode_artifact(artifact.decoded()).unwrap();
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name);
            std::fs::write(path, generated).unwrap();
        }
    }

    fn filesystem_game_test_artifacts() -> [(&'static str, VerifiedArtifact); 5] {
        [
            (
                "filesystem-write.cpkt",
                crate::execution::fixtures::filesystem_conformance_artifact(),
            ),
            (
                "filesystem-write-alternate.cpkt",
                crate::execution::fixtures::filesystem_alternate_marker_artifact(),
            ),
            (
                "filesystem-read.cpkt",
                crate::execution::fixtures::filesystem_recovery_reader_artifact(),
            ),
            (
                "process-terminal-child.cpkt",
                crate::execution::fixtures::process_terminal_child_artifact(),
            ),
            (
                "process-install-rom-executable.cpkt",
                crate::execution::fixtures::process_install_rom_executable_artifact(),
            ),
        ]
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

    fn rom_with_executable(path: &str, content: &[u8], limits: &FileSystemLimits) -> RomImage {
        let path = path.as_bytes();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"CPKTROM\0");
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
        bytes.extend_from_slice(path);
        bytes.push(2);
        bytes.push(1);
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&(content.len() as u64).to_le_bytes());
        bytes.extend_from_slice(content);
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
