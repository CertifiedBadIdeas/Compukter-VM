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
    AdmissionError, AdvanceOutcome, CapabilityBinding, EntryValue, ExecutionProfile, GuestTrap,
    HostFailure, HostRequestView, HostResponse, HostValueInput, HostValueType, HostValueView,
    ManagedAllocationFailure, OperationSchema, QuotaExhaustion, RequestId, ResumeError, RunError,
    Session, TerminalDevice, VerifiedArtifact, VmFault,
};

const TERMINAL_NAMESPACE: &str = "compukter";
const TERMINAL_NAME: &str = "terminal";
const TERMINAL_ABI_MAJOR: u16 = 1;

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
    NoPendingCompatibilityLine,
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
    WaitingForLine,
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
    pending_compatibility_line: Option<RequestId>,
}

impl ComputerMachine {
    pub fn start(
        artifact: VerifiedArtifact,
        profile: ExecutionProfile,
        addon_bindings: &[CapabilityBinding<'_>],
        arguments: &[EntryValue],
    ) -> Result<Self, ComputerStartError> {
        let string_argument = [HostValueType::String];
        let terminal_operations = [
            OperationSchema::asynchronous(&string_argument, HostValueType::Unit),
            OperationSchema::asynchronous(&string_argument, HostValueType::Unit),
            OperationSchema::asynchronous(&[], HostValueType::String),
        ];
        let terminal_binding = CapabilityBinding::new(
            TERMINAL_NAMESPACE,
            TERMINAL_NAME,
            TERMINAL_ABI_MAJOR,
            0,
            &terminal_operations,
        );
        let mut bindings = Vec::with_capacity(addon_bindings.len() + 1);
        bindings.extend_from_slice(addon_bindings);
        bindings.push(terminal_binding);
        let mut session =
            Session::admit(artifact, profile, &bindings).map_err(ComputerStartError::Admission)?;
        session
            .start(arguments)
            .map_err(ComputerStartError::Start)?;
        Ok(Self {
            session,
            terminal: TerminalDevice::default(),
            pending_compatibility_line: None,
        })
    }

    pub const fn terminal(&self) -> &TerminalDevice {
        &self.terminal
    }

    pub fn terminal_mut(&mut self) -> &mut TerminalDevice {
        &mut self.terminal
    }

    pub fn advance(
        &mut self,
        guest_budget: u32,
        maintenance_budget: u32,
    ) -> Result<ComputerAdvanceOutcome, ComputerError> {
        if self.pending_compatibility_line.is_some() {
            return Ok(ComputerAdvanceOutcome::WaitingForLine);
        }
        let internal = {
            let outcome = self
                .session
                .advance(guest_budget, maintenance_budget)
                .map_err(ComputerError::Run)?;
            match outcome {
                AdvanceOutcome::HostRequest(request) if is_terminal(request) => {
                    Some(copy_terminal_request(request)?)
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
        match internal.expect("terminal request branch always publishes an action") {
            TerminalRequest::Write { id, units, newline } => {
                self.terminal
                    .write_utf16(&units)
                    .map_err(|_| ComputerError::InvalidTerminalRequest)?;
                if newline {
                    self.terminal
                        .write_utf16(&['\n' as u16])
                        .map_err(|_| ComputerError::InvalidTerminalRequest)?;
                }
                self.session
                    .resume(id, HostResponse::Success(HostValueInput::Unit))
                    .map_err(ComputerError::Resume)?;
                Ok(ComputerAdvanceOutcome::SliceExhausted)
            }
            TerminalRequest::ReadLine { id } => {
                self.pending_compatibility_line = Some(id);
                Ok(ComputerAdvanceOutcome::WaitingForLine)
            }
        }
    }

    pub fn provide_compatibility_line(&mut self, units: &[u16]) -> Result<(), ComputerError> {
        let request = self
            .pending_compatibility_line
            .ok_or(ComputerError::NoPendingCompatibilityLine)?;
        self.session
            .resume(
                request,
                HostResponse::Success(HostValueInput::String(units)),
            )
            .map_err(ComputerError::Resume)?;
        self.pending_compatibility_line = None;
        Ok(())
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
    Write {
        id: RequestId,
        units: Vec<u16>,
        newline: bool,
    },
    ReadLine {
        id: RequestId,
    },
}

fn is_terminal(request: HostRequestView<'_>) -> bool {
    request.namespace() == TERMINAL_NAMESPACE
        && request.name() == TERMINAL_NAME
        && request.abi_major() == TERMINAL_ABI_MAJOR
}

fn copy_terminal_request(request: HostRequestView<'_>) -> Result<TerminalRequest, ComputerError> {
    match request.operation() {
        operation @ 0..=1 => {
            if request.arguments().len() != 1 {
                return Err(ComputerError::InvalidTerminalRequest);
            }
            let Some(HostValueView::String(units)) = request.arguments().get(0) else {
                return Err(ComputerError::InvalidTerminalRequest);
            };
            Ok(TerminalRequest::Write {
                id: request.id(),
                units: units.to_vec(),
                newline: operation == 1,
            })
        }
        2 if request.arguments().is_empty() => Ok(TerminalRequest::ReadLine { id: request.id() }),
        _ => Err(ComputerError::InvalidTerminalRequest),
    }
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
