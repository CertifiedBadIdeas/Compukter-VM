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

use crate::{TaskId, TerminalDevice, TerminalError, TerminalKey};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InputOwner {
    frame: usize,
    task: TaskId,
}

impl InputOwner {
    pub(crate) const fn new(frame: usize, task: TaskId) -> Self {
        Self { frame, task }
    }

    pub(crate) const fn frame(self) -> usize {
        self.frame
    }

    pub(crate) const fn task(self) -> TaskId {
        self.task
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputOwnershipError {
    RawBusy,
    CanonicalBusy,
    WrongOwner,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalLineSubmissionError {
    NoPendingRead,
    InputBusy,
    PartialInput,
    UnsupportedCodeUnit,
    LineTooLong,
    Terminal,
    Resume,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StandardStreamError {
    InvalidLimits,
    LineTooLong,
    LineReady,
    OutputTooLarge,
    NoCanonicalRead,
    Terminal,
}

impl From<TerminalError> for StandardStreamError {
    fn from(_: TerminalError) -> Self {
        Self::Terminal
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputMode {
    Raw,
    Canonical,
}

#[derive(Debug, Default)]
struct CanonicalInput {
    editing: Vec<u16>,
    ready: Option<Box<[u16]>>,
}

impl CanonicalInput {
    fn clear(&mut self) {
        self.editing.clear();
        self.ready = None;
    }
}

#[derive(Debug, Default)]
struct TerminalOutput;

#[derive(Debug)]
pub(crate) struct StandardStreams {
    input: CanonicalInput,
    output: TerminalOutput,
    error: TerminalOutput,
    owner: Option<(InputMode, InputOwner)>,
    maximum_line_code_units: usize,
    maximum_output_code_units: usize,
}

impl StandardStreams {
    pub(crate) fn new(
        maximum_line_code_units: usize,
        maximum_output_code_units: usize,
    ) -> Result<Self, StandardStreamError> {
        if maximum_line_code_units == 0 || maximum_output_code_units == 0 {
            return Err(StandardStreamError::InvalidLimits);
        }
        Ok(Self {
            input: CanonicalInput::default(),
            output: TerminalOutput,
            error: TerminalOutput,
            owner: None,
            maximum_line_code_units,
            maximum_output_code_units,
        })
    }

    pub(crate) fn begin_read(&mut self, owner: InputOwner) -> Result<(), InputOwnershipError> {
        self.acquire(InputMode::Canonical, owner)
    }

    pub(crate) fn submit_complete_line(
        &mut self,
        owner: InputOwner,
        units: &[u16],
        terminal: &mut TerminalDevice,
    ) -> Result<Box<[u16]>, CanonicalLineSubmissionError> {
        if self.owner != Some((InputMode::Canonical, owner)) {
            return Err(CanonicalLineSubmissionError::InputBusy);
        }
        if !self.input.editing.is_empty() || self.input.ready.is_some() {
            return Err(CanonicalLineSubmissionError::PartialInput);
        }
        if !units.iter().copied().all(is_canonical_text_unit) {
            return Err(CanonicalLineSubmissionError::UnsupportedCodeUnit);
        }
        if units.len() > self.maximum_line_code_units {
            return Err(CanonicalLineSubmissionError::LineTooLong);
        }
        terminal
            .write_utf16(units)
            .map_err(|_| CanonicalLineSubmissionError::Terminal)?;
        terminal
            .write_utf16(&['\n' as u16])
            .map_err(|_| CanonicalLineSubmissionError::Terminal)?;
        self.owner = None;
        Ok(units.to_vec().into_boxed_slice())
    }

    pub(crate) fn begin_raw_wait(&mut self, owner: InputOwner) -> Result<(), InputOwnershipError> {
        self.acquire(InputMode::Raw, owner)
    }

    pub(crate) fn finish_raw(&mut self, owner: InputOwner) -> Result<(), InputOwnershipError> {
        if self.owner != Some((InputMode::Raw, owner)) {
            return Err(InputOwnershipError::WrongOwner);
        }
        self.owner = None;
        Ok(())
    }

    pub(crate) fn ensure_raw_owner(&self, owner: InputOwner) -> Result<(), InputOwnershipError> {
        if self.owner == Some((InputMode::Raw, owner)) {
            Ok(())
        } else {
            Err(InputOwnershipError::WrongOwner)
        }
    }

    pub(crate) fn cancel(&mut self, owner: InputOwner) {
        if self.owner.is_some_and(|(_, current)| current == owner) {
            self.owner = None;
            self.input.clear();
        }
    }

    pub(crate) fn cancel_frame(&mut self, frame: usize) {
        if self.owner.is_some_and(|(_, owner)| owner.frame() == frame) {
            self.owner = None;
            self.input.clear();
        }
    }

    pub(crate) fn accept_text(
        &mut self,
        units: &[u16],
        terminal: &mut TerminalDevice,
    ) -> Result<(), StandardStreamError> {
        self.require_canonical()?;
        if self.input.ready.is_some() {
            return Err(StandardStreamError::LineReady);
        }
        let filtered;
        let units = if units.iter().copied().all(is_canonical_text_unit) {
            units
        } else {
            filtered = units
                .iter()
                .copied()
                .filter(|unit| is_canonical_text_unit(*unit))
                .collect::<Vec<_>>();
            filtered.as_slice()
        };
        if self
            .input
            .editing
            .len()
            .checked_add(units.len())
            .is_none_or(|length| length > self.maximum_line_code_units)
        {
            return Err(StandardStreamError::LineTooLong);
        }
        terminal.write_utf16(units)?;
        self.input.editing.extend_from_slice(units);
        Ok(())
    }

    pub(crate) fn accept_key(
        &mut self,
        key: TerminalKey,
        terminal: &mut TerminalDevice,
    ) -> Result<(), StandardStreamError> {
        self.require_canonical()?;
        if self.input.ready.is_some() {
            return Err(StandardStreamError::LineReady);
        }
        match key {
            TerminalKey::Backspace => {
                let Some(last) = self.input.editing.pop() else {
                    return Ok(());
                };
                if (0xDC00..=0xDFFF).contains(&last)
                    && self
                        .input
                        .editing
                        .last()
                        .is_some_and(|previous| (0xD800..=0xDBFF).contains(previous))
                {
                    self.input.editing.pop();
                }
                terminal.erase_previous();
            }
            TerminalKey::Enter => {
                terminal.write_utf16(&['\n' as u16])?;
                self.input.ready = Some(std::mem::take(&mut self.input.editing).into_boxed_slice());
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn take_line(&mut self, owner: InputOwner) -> Option<Box<[u16]>> {
        if self.owner != Some((InputMode::Canonical, owner)) {
            return None;
        }
        let line = self.input.ready.take()?;
        self.owner = None;
        Some(line)
    }

    pub(crate) fn write_stdout(
        &mut self,
        units: &[u16],
        terminal: &mut TerminalDevice,
    ) -> Result<(), StandardStreamError> {
        Self::write_endpoint(
            &mut self.output,
            self.maximum_output_code_units,
            units,
            terminal,
        )
    }

    pub(crate) fn write_stderr(
        &mut self,
        units: &[u16],
        terminal: &mut TerminalDevice,
    ) -> Result<(), StandardStreamError> {
        Self::write_endpoint(
            &mut self.error,
            self.maximum_output_code_units,
            units,
            terminal,
        )
    }

    fn acquire(&mut self, mode: InputMode, owner: InputOwner) -> Result<(), InputOwnershipError> {
        if self.owner == Some((mode, owner)) {
            return Ok(());
        }
        if let Some((current, _)) = self.owner {
            return Err(match current {
                InputMode::Raw => InputOwnershipError::RawBusy,
                InputMode::Canonical => InputOwnershipError::CanonicalBusy,
            });
        }
        self.owner = Some((mode, owner));
        Ok(())
    }

    fn require_canonical(&self) -> Result<(), StandardStreamError> {
        if matches!(self.owner, Some((InputMode::Canonical, _))) {
            Ok(())
        } else {
            Err(StandardStreamError::NoCanonicalRead)
        }
    }

    fn write_endpoint(
        _endpoint: &mut TerminalOutput,
        maximum_output_code_units: usize,
        units: &[u16],
        terminal: &mut TerminalDevice,
    ) -> Result<(), StandardStreamError> {
        if units.len() > maximum_output_code_units {
            return Err(StandardStreamError::OutputTooLarge);
        }
        terminal.write_utf16(units)?;
        Ok(())
    }
}

fn is_canonical_text_unit(unit: u16) -> bool {
    unit >= 0x20 && unit != 0x7f
}

#[cfg(test)]
mod tests {
    use super::{
        CanonicalLineSubmissionError, InputOwner, InputOwnershipError, StandardStreamError,
        StandardStreams,
    };
    use crate::{TaskId, TerminalDevice, TerminalKey, TerminalPosition};

    const ROOT: InputOwner = InputOwner::new(1, TaskId::ROOT);

    #[test]
    fn canonical_input_echoes_edits_and_returns_a_line_without_lf() {
        let mut terminal = TerminalDevice::default();
        let mut streams = StandardStreams::new(8, 8).unwrap();

        streams.begin_read(ROOT).unwrap();
        streams
            .accept_text(&[0x0041, 0xD83D, 0xDE00], &mut terminal)
            .unwrap();
        streams
            .accept_key(TerminalKey::Backspace, &mut terminal)
            .unwrap();
        streams
            .accept_key(TerminalKey::Enter, &mut terminal)
            .unwrap();

        assert_eq!(
            Some(vec![0x0041].into_boxed_slice()),
            streams.take_line(ROOT)
        );
        assert_eq!('A' as u32, terminal.cell(0, 0).unwrap().code_point());
        assert_eq!(' ' as u32, terminal.cell(1, 0).unwrap().code_point());
        assert_eq!(
            TerminalPosition::new(0, 1).unwrap(),
            terminal.cursor_position()
        );
    }

    #[test]
    fn canonical_input_rejects_an_oversized_text_event_before_echo() {
        let mut terminal = TerminalDevice::default();
        let mut streams = StandardStreams::new(2, 8).unwrap();
        streams.begin_read(ROOT).unwrap();

        assert_eq!(
            StandardStreamError::LineTooLong,
            streams
                .accept_text(&['a' as u16, 'b' as u16, 'c' as u16], &mut terminal)
                .unwrap_err(),
        );
        assert_eq!(' ' as u32, terminal.cell(0, 0).unwrap().code_point());
        assert_eq!(None, streams.take_line(ROOT));
    }

    #[test]
    fn canonical_input_ignores_control_text_without_echoing_or_storing_it() {
        let mut terminal = TerminalDevice::default();
        let mut streams = StandardStreams::new(8, 8).unwrap();
        streams.begin_read(ROOT).unwrap();

        streams
            .accept_text(
                &['a' as u16, 0x0000, '\r' as u16, '\n' as u16, 0x007f],
                &mut terminal,
            )
            .unwrap();
        streams
            .accept_key(TerminalKey::Enter, &mut terminal)
            .unwrap();

        assert_eq!(
            Some(vec!['a' as u16].into_boxed_slice()),
            streams.take_line(ROOT),
        );
        assert_eq!('a' as u32, terminal.cell(0, 0).unwrap().code_point());
        assert_eq!(
            TerminalPosition::new(0, 1).unwrap(),
            terminal.cursor_position(),
        );
    }

    #[test]
    fn stdout_and_stderr_share_terminal_order_but_enforce_payload_bounds() {
        let mut terminal = TerminalDevice::default();
        let mut streams = StandardStreams::new(8, 2).unwrap();

        streams.write_stdout(&['A' as u16], &mut terminal).unwrap();
        streams.write_stderr(&['B' as u16], &mut terminal).unwrap();
        assert_eq!('A' as u32, terminal.cell(0, 0).unwrap().code_point());
        assert_eq!('B' as u32, terminal.cell(1, 0).unwrap().code_point());

        assert_eq!(
            StandardStreamError::OutputTooLarge,
            streams
                .write_stdout(&['C' as u16, 'D' as u16, 'E' as u16], &mut terminal)
                .unwrap_err(),
        );
        assert_eq!(' ' as u32, terminal.cell(2, 0).unwrap().code_point());
    }

    #[test]
    fn raw_and_canonical_ownership_conflict_then_switch_sequentially() {
        let other = InputOwner::new(1, TaskId::new(2).unwrap());
        let mut streams = StandardStreams::new(8, 8).unwrap();

        streams.begin_read(ROOT).unwrap();
        assert_eq!(
            InputOwnershipError::CanonicalBusy,
            streams.begin_raw_wait(other).unwrap_err(),
        );
        streams.cancel(ROOT);

        streams.begin_raw_wait(other).unwrap();
        assert_eq!(
            InputOwnershipError::WrongOwner,
            streams.ensure_raw_owner(ROOT).unwrap_err(),
        );
        streams.ensure_raw_owner(other).unwrap();
        assert_eq!(
            InputOwnershipError::RawBusy,
            streams.begin_read(ROOT).unwrap_err(),
        );
        assert_eq!(
            InputOwnershipError::WrongOwner,
            streams.finish_raw(ROOT).unwrap_err(),
        );
        streams.finish_raw(other).unwrap();
        streams.begin_read(ROOT).unwrap();
    }

    #[test]
    fn complete_line_submission_requires_empty_canonical_input() {
        let mut streams = StandardStreams::new(32, 32).unwrap();
        let mut terminal = TerminalDevice::default();
        streams.begin_read(ROOT).unwrap();
        streams.accept_text(&['a' as u16], &mut terminal).unwrap();

        assert_eq!(
            CanonicalLineSubmissionError::PartialInput,
            streams
                .submit_complete_line(ROOT, &['x' as u16], &mut terminal)
                .unwrap_err(),
        );
    }

    #[test]
    fn complete_line_submission_returns_the_exact_line() {
        let mut streams = StandardStreams::new(32, 32).unwrap();
        let mut terminal = TerminalDevice::default();
        streams.begin_read(ROOT).unwrap();

        let line = streams
            .submit_complete_line(ROOT, &['r' as u16, 'u' as u16, 'n' as u16], &mut terminal)
            .unwrap();

        assert_eq!(line.as_ref(), &['r' as u16, 'u' as u16, 'n' as u16]);
        assert!(streams.take_line(ROOT).is_none());
    }

    #[test]
    fn complete_line_submission_rejects_invalid_owner_units_and_length_without_echo() {
        let mut streams = StandardStreams::new(2, 32).unwrap();
        let mut terminal = TerminalDevice::default();
        let other = InputOwner::new(1, TaskId::new(2).unwrap());
        streams.begin_read(ROOT).unwrap();
        let revision = terminal.revision();

        assert_eq!(
            CanonicalLineSubmissionError::InputBusy,
            streams
                .submit_complete_line(other, &['x' as u16], &mut terminal)
                .unwrap_err(),
        );
        assert_eq!(
            CanonicalLineSubmissionError::UnsupportedCodeUnit,
            streams
                .submit_complete_line(ROOT, &['\n' as u16], &mut terminal)
                .unwrap_err(),
        );
        assert_eq!(
            CanonicalLineSubmissionError::LineTooLong,
            streams
                .submit_complete_line(ROOT, &['a' as u16, 'b' as u16, 'c' as u16], &mut terminal,)
                .unwrap_err(),
        );
        assert_eq!(revision, terminal.revision());
    }

    #[test]
    fn invalid_zero_limits_are_rejected() {
        assert_eq!(
            StandardStreamError::InvalidLimits,
            StandardStreams::new(0, 1).unwrap_err()
        );
        assert_eq!(
            StandardStreamError::InvalidLimits,
            StandardStreams::new(1, 0).unwrap_err()
        );
    }
}
