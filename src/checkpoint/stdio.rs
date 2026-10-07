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
use crate::execution::checkpoint::{checkpoint_enum, checkpoint_struct, CheckpointError, Result};
checkpoint_struct!(InputOwner { frame, task });
checkpoint_enum!(InputMode { 0 => Raw; 1 => Canonical; });
checkpoint_struct!(CanonicalInput { editing, ready });
checkpoint_struct!(StandardStreams { input, owner, maximum_line_code_units, maximum_output_code_units } defaults { output: TerminalOutput, error: TerminalOutput });
impl StandardStreams {
    pub(crate) fn validate_checkpoint(&self, expected: &Self) -> Result<()> {
        if self.maximum_line_code_units != expected.maximum_line_code_units
            || self.maximum_output_code_units != expected.maximum_output_code_units
            || self.input.editing.len() > self.maximum_line_code_units
            || self
                .input
                .ready
                .as_ref()
                .is_some_and(|line| line.len() > self.maximum_line_code_units)
            || self
                .input
                .editing
                .iter()
                .any(|unit| !is_canonical_text_unit(*unit))
            || self
                .input
                .ready
                .as_ref()
                .is_some_and(|line| line.iter().any(|unit| !is_canonical_text_unit(*unit)))
            || self.owner.is_some_and(|(_, owner)| owner.task.get() == 0)
            || (!self.input.editing.is_empty() || self.input.ready.is_some())
                && !matches!(self.owner, Some((InputMode::Canonical, _)))
            || self.input.ready.is_some() && !self.input.editing.is_empty()
        {
            return Err(CheckpointError::InvalidState);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::checkpoint::{Checkpoint, Reader, Writer};
    #[test]
    fn checkpoint_retains_partial_line_and_exclusive_input_owner() {
        let owner = InputOwner::new(1, TaskId::new(2).unwrap());
        let mut original = StandardStreams::new(64, 64).unwrap();
        let mut terminal = TerminalDevice::default();
        original.begin_read(owner).unwrap();
        original.accept_text(&[0x41, 0x42], &mut terminal).unwrap();
        let mut writer = Writer::new(4096);
        original.write(&mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 4096, 4096).unwrap();
        let mut restored = StandardStreams::read(&mut reader).unwrap();
        reader.finish().unwrap();
        restored
            .validate_checkpoint(&StandardStreams::new(64, 64).unwrap())
            .unwrap();
        assert_eq!(
            Err(InputOwnershipError::CanonicalBusy),
            restored.begin_raw_wait(InputOwner::new(0, TaskId::ROOT))
        );
        restored
            .accept_key(TerminalKey::Backspace, &mut terminal)
            .unwrap();
        restored.accept_text(&[0x43], &mut terminal).unwrap();
        restored
            .accept_key(TerminalKey::Enter, &mut terminal)
            .unwrap();
        assert_eq!(
            Some(vec![0x41, 0x43].into_boxed_slice()),
            restored.take_line(owner)
        );
        assert_eq!(None, restored.take_line(owner));
        restored
            .begin_raw_wait(InputOwner::new(0, TaskId::ROOT))
            .unwrap();
    }
}
