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
    Session, TerminalDevice, TerminalInputEvent, TerminalKeyAction, VerifiedArtifact, VmFault,
};

const TERMINAL_NAMESPACE: &str = "compukter";
const TERMINAL_NAME: &str = "terminal";
const TERMINAL_ABI_MAJOR: u16 = 1;
const RAW_TERMINAL_ABI_MAJOR: u16 = 2;

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
    WaitingForLine,
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
    pending_compatibility_line: Option<RequestId>,
    pending_terminal_event: Option<RequestId>,
    active_terminal_event: Option<TerminalInputEvent>,
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
        let terminal_binding = CapabilityBinding::new(
            TERMINAL_NAMESPACE,
            TERMINAL_NAME,
            TERMINAL_ABI_MAJOR,
            0,
            &terminal_operations,
        );
        let raw_terminal_binding = CapabilityBinding::new(
            TERMINAL_NAMESPACE,
            TERMINAL_NAME,
            RAW_TERMINAL_ABI_MAJOR,
            0,
            &raw_terminal_operations,
        );
        let mut bindings = Vec::with_capacity(addon_bindings.len() + 2);
        bindings.extend_from_slice(addon_bindings);
        bindings.push(terminal_binding);
        bindings.push(raw_terminal_binding);
        let mut session =
            Session::admit(artifact, profile, &bindings).map_err(ComputerStartError::Admission)?;
        session
            .start(arguments)
            .map_err(ComputerStartError::Start)?;
        Ok(Self {
            session,
            terminal: TerminalDevice::default(),
            pending_compatibility_line: None,
            pending_terminal_event: None,
            active_terminal_event: None,
        })
    }

    pub const fn terminal(&self) -> &TerminalDevice {
        &self.terminal
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
        if self.pending_compatibility_line.is_some() {
            return Ok(ComputerAdvanceOutcome::WaitingForLine);
        }
        if let Some(request) = self.pending_terminal_event {
            let Some(kind) = self.terminal_await_event()? else {
                return Ok(ComputerAdvanceOutcome::WaitingForTerminalEvent);
            };
            self.session
                .resume(
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
                AdvanceOutcome::HostRequest(request) if is_compatibility_terminal(request) => {
                    Some(copy_terminal_request(request)?)
                }
                AdvanceOutcome::HostRequest(request) if is_raw_terminal(request) => {
                    Some(copy_raw_terminal_request(request)?)
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
            TerminalRequest::Raw { id, operation } => {
                self.handle_raw_terminal_request(id, operation)
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
                    .resume(id, HostResponse::Success(HostValueInput::String(&units)))
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
            .resume(id, HostResponse::Success(response))
            .map_err(ComputerError::Resume)?;
        Ok(ComputerAdvanceOutcome::SliceExhausted)
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
    Raw {
        id: RequestId,
        operation: RawTerminalOperation,
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

fn is_compatibility_terminal(request: HostRequestView<'_>) -> bool {
    request.namespace() == TERMINAL_NAMESPACE
        && request.name() == TERMINAL_NAME
        && request.abi_major() == TERMINAL_ABI_MAJOR
}

fn is_raw_terminal(request: HostRequestView<'_>) -> bool {
    request.namespace() == TERMINAL_NAMESPACE
        && request.name() == TERMINAL_NAME
        && request.abi_major() == RAW_TERMINAL_ABI_MAJOR
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
    use super::*;
    use crate::{ExecutionProfile, TerminalKey, TerminalKeyEvent, TerminalModifiers};

    #[test]
    fn raw_terminal_capability_waits_and_consumes_typed_events() {
        let artifact = crate::execution::fixtures::raw_terminal_conformance_artifact(&[
            '>' as u16, ' ' as u16,
        ]);
        let mut computer = ComputerMachine::start(artifact, profile(), &[], &[]).unwrap();

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
