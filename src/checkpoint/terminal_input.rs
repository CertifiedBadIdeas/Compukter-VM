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
    checkpoint_enum, checkpoint_struct, Checkpoint, CheckpointError, Reader, Result, Writer,
};
checkpoint_struct!(TerminalInputLimits {
    maximum_events,
    maximum_text_code_points
});
checkpoint_struct!(TerminalKeyEvent {
    key,
    action,
    modifiers
});
checkpoint_enum!(TerminalKeyAction { 0 => Press; 1 => Repeat; });
checkpoint_enum!(TerminalInputEvent { 0 => Key(v0); 1 => Text(v0); });
checkpoint_struct!(TerminalInputQueue { limits, events });
impl Checkpoint for TerminalKey {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.code().write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Self::try_from(u16::read(reader)?).map_err(|_| CheckpointError::InvalidState)
    }
}
impl Checkpoint for TerminalModifiers {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.bits().write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Self::new(u8::read(reader)?).map_err(|_| CheckpointError::InvalidState)
    }
}
impl TerminalInputQueue {
    pub(crate) fn validate_checkpoint(&self, expected: &Self) -> Result<()> {
        if self.limits != expected.limits || self.events.len() > self.limits.maximum_events
   || self.events.iter().any(|event| matches!(event, TerminalInputEvent::Text(text) if text.chars().count() > self.limits.maximum_text_code_points)) { return Err(CheckpointError::InvalidState); }
        Ok(())
    }
}
