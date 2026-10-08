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
    checkpoint_struct, Checkpoint, CheckpointError, Reader, Result, Writer,
};
checkpoint_struct!(TerminalDevice {
    cells,
    row_head,
    cursor,
    cursor_visible,
    foreground,
    background,
    input,
    replication
});
impl Checkpoint for TerminalCell {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.code_point.write(writer)?;
        self.foreground.write(writer)?;
        self.background.write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Self::new(u32::read(reader)?, u8::read(reader)?, u8::read(reader)?)
            .map_err(|_| CheckpointError::InvalidState)
    }
}
impl Checkpoint for TerminalPosition {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.x.write(writer)?;
        self.y.write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Self::new(u16::read(reader)?, u16::read(reader)?).map_err(|_| CheckpointError::InvalidState)
    }
}
impl TerminalDevice {
    pub(crate) fn validate_checkpoint(&self, expected: &Self) -> Result<()> {
        if self.row_head >= TERMINAL_HEIGHT as usize
            || validate_palette(self.foreground).is_err()
            || validate_palette(self.background).is_err()
        {
            return Err(CheckpointError::InvalidState);
        }
        self.input.validate_checkpoint(&expected.input)?;
        self.replication.validate_checkpoint(&expected.replication)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkpoint_retains_scrolled_screen_pending_changes_and_input_order() {
        let mut original = TerminalDevice::default();
        for line in 0..25 {
            original
                .write_utf16(&format!("{line}\n").encode_utf16().collect::<Vec<_>>())
                .unwrap();
        }
        original.commit();
        original.write_utf16(&[0x41, 0x42]).unwrap();
        original.push_text("Привет").unwrap();
        original
            .push_key(TerminalKeyEvent::new(
                crate::TerminalKey::Enter,
                crate::TerminalKeyAction::Press,
                crate::TerminalModifiers::default(),
            ))
            .unwrap();
        let mut writer = Writer::new(256 * 1024);
        original.write(&mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 256 * 1024, 256 * 1024).unwrap();
        let mut restored = TerminalDevice::read(&mut reader).unwrap();
        reader.finish().unwrap();
        restored
            .validate_checkpoint(&TerminalDevice::default())
            .unwrap();
        assert_eq!(original.changes_since(0), restored.changes_since(0));
        assert_eq!(original.commit(), restored.commit());
        assert_eq!(original.poll_input(), restored.poll_input());
        assert_eq!(original.poll_input(), restored.poll_input());
        assert_eq!(None, restored.poll_input());
        original.write_utf16(&[0x43]).unwrap();
        restored.write_utf16(&[0x43]).unwrap();
        assert_eq!(original.commit(), restored.commit());
    }
    #[test]
    fn checkpoint_rejects_invalid_terminal_scalars_and_cursor() {
        let mut writer = Writer::new(32);
        0xd800_u32.write(&mut writer).unwrap();
        15_u8.write(&mut writer).unwrap();
        0_u8.write(&mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 32, 0).unwrap();
        assert!(matches!(
            TerminalCell::read(&mut reader),
            Err(CheckpointError::InvalidState)
        ));
        let terminal = TerminalDevice {
            row_head: TERMINAL_HEIGHT as usize,
            ..TerminalDevice::default()
        };
        assert_eq!(
            Err(CheckpointError::InvalidState),
            terminal.validate_checkpoint(&TerminalDevice::default())
        );
    }
}
