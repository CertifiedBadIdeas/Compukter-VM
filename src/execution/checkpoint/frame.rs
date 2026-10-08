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

checkpoint_struct!(ComponentLayout { offset, atom });

checkpoint_struct!(FrameLayout {
    byte_len,
    alignment,
    values
});

checkpoint_struct!(ValueLayout { components });

checkpoint_struct!(FrameReservation { base, byte_len });

checkpoint_struct!(FrameArena { bytes, free_head } defaults { #[cfg(test)] active_bytes: 0, #[cfg(test)] peak_active_bytes: 0, #[cfg(test)] initialized: vec![true; <Box<[u8]> as AsRef<[u8]>>::as_ref(&bytes).len()].into_boxed_slice() });

checkpoint_struct!(StaticArena {
    arena,
    layout,
    reservation
});

checkpoint_enum!(PhysicalAtom { 0 => I32; 1 => I64; 2 => F32; 3 => F64; 4 => Ref32; });

impl FrameArena {
    pub(crate) fn validate_checkpoint(
        &mut self,
        capacity: usize,
        live: &[FrameReservation],
    ) -> Result<()> {
        if self.bytes.len() != capacity {
            return Err(CheckpointError::InvalidState);
        }
        let mut ranges = Vec::new();
        let mut active_bytes = 0_u32;
        for frame in live {
            if frame.byte_len == 0 {
                continue;
            }
            let end = frame
                .base
                .checked_add(frame.byte_len)
                .ok_or(CheckpointError::InvalidState)?;
            if !frame.base.is_multiple_of(8)
                || !frame.byte_len.is_multiple_of(8)
                || end as usize > capacity
            {
                return Err(CheckpointError::InvalidState);
            }
            active_bytes = active_bytes
                .checked_add(frame.byte_len)
                .ok_or(CheckpointError::InvalidState)?;
            ranges.push((frame.base, end));
        }
        let mut cursor = self.free_head;
        let mut previous = None;
        while let Some(base) = cursor {
            if !base.is_multiple_of(8) || previous.is_some_and(|previous| base <= previous) {
                return Err(CheckpointError::InvalidState);
            }
            let (next, length) = self
                .read_free(base)
                .map_err(|_| CheckpointError::InvalidState)?;
            let end = base
                .checked_add(length)
                .ok_or(CheckpointError::InvalidState)?;
            if length < 8 || !length.is_multiple_of(8) || end as usize > capacity {
                return Err(CheckpointError::InvalidState);
            }
            ranges.push((base, end));
            previous = Some(base);
            cursor = next;
        }
        ranges.sort_unstable();
        let mut end = 0;
        for (base, next) in ranges {
            if base != end {
                return Err(CheckpointError::InvalidState);
            }
            end = next;
        }
        if end as usize != capacity {
            return Err(CheckpointError::InvalidState);
        }
        #[cfg(test)]
        {
            self.active_bytes = active_bytes;
            self.peak_active_bytes = active_bytes;
        }
        Ok(())
    }
}

impl StaticArena {
    pub(crate) fn validate_checkpoint(&mut self, expected: &FrameLayout) -> Result<()> {
        if self.layout != *expected
            || self.reservation.base != 0
            || self.reservation.byte_len
                != expected
                    .byte_len
                    .checked_next_multiple_of(8)
                    .ok_or(CheckpointError::InvalidState)?
        {
            return Err(CheckpointError::InvalidState);
        }
        self.arena
            .validate_checkpoint(self.reservation.byte_len as usize, &[self.reservation])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::checkpoint::{Checkpoint, Reader, Writer};

    fn layout() -> FrameLayout {
        FrameLayout {
            byte_len: 8,
            alignment: 8,
            values: vec![ValueLayout {
                components: vec![ComponentLayout {
                    offset: 0,
                    atom: PhysicalAtom::I64,
                }]
                .into_boxed_slice(),
            }]
            .into_boxed_slice(),
        }
    }

    #[test]
    fn checkpoint_retains_frame_values_and_fragmented_free_regions() {
        let layout = layout();
        let mut original = FrameArena::new(64).unwrap();
        let first = original.push(&layout).unwrap();
        let hole = original.push(&layout).unwrap();
        let third = original.push(&layout).unwrap();
        original
            .write_i64(first.base, &layout, 0, 0, i64::MIN)
            .unwrap();
        original.pop(hole).unwrap();
        let mut writer = Writer::new(4096);
        original.write(&mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 4096, 4096).unwrap();
        let mut restored = FrameArena::read(&mut reader).unwrap();
        reader.finish().unwrap();
        restored.validate_checkpoint(64, &[first, third]).unwrap();
        assert_eq!(
            i64::MIN,
            restored.read_i64(first.base, &layout, 0, 0).unwrap()
        );
        assert_eq!(
            original.push(&layout).unwrap(),
            restored.push(&layout).unwrap()
        );
        restored.pop(first).unwrap();
        restored.pop(third).unwrap();
    }

    #[test]
    fn checkpoint_rejects_overlapping_frames_and_incomplete_partition() {
        let mut arena = FrameArena::new(64).unwrap();
        let frame = arena.push(&layout()).unwrap();
        assert_eq!(
            Err(CheckpointError::InvalidState),
            arena.validate_checkpoint(64, &[frame, frame])
        );
        assert_eq!(
            Err(CheckpointError::InvalidState),
            arena.validate_checkpoint(64, &[])
        );
        arena.validate_checkpoint(64, &[frame]).unwrap();
        arena.write_free(8, Some(8), 56).unwrap();
        assert_eq!(
            Err(CheckpointError::InvalidState),
            arena.validate_checkpoint(64, &[frame])
        );
    }

    #[test]
    fn checkpoint_validates_empty_and_populated_statics_against_image_layout() {
        for layout in [
            layout(),
            FrameLayout {
                byte_len: 0,
                alignment: 1,
                values: Box::new([]),
            },
        ] {
            let mut statics = StaticArena::new(layout.clone()).unwrap();
            statics.validate_checkpoint(&layout).unwrap();
            statics.reservation.base = 8;
            assert_eq!(
                Err(CheckpointError::InvalidState),
                statics.validate_checkpoint(&layout)
            );
        }
    }
}
