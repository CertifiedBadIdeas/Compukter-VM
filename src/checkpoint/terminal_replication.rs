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
checkpoint_enum!(TerminalChange {
 0 => Patch { start, cells }; 1 => Fill { x, y, width, height, cell }; 2 => Scroll { rows, fill }; 3 => Cursor { position, visible }; 4 => Reset;
});
checkpoint_struct!(TerminalDelta {
    base_revision,
    target_revision,
    changes
});
checkpoint_struct!(ReplicationState {
    revision,
    journal_capacity,
    pending,
    journal,
    requires_full_replacement
});
impl ReplicationState {
    pub(crate) fn validate_checkpoint(&self, expected: &Self) -> Result<()> {
        use crate::terminal::{TERMINAL_HEIGHT, TERMINAL_WIDTH};
        let valid = |change: &TerminalChange| match change {
            TerminalChange::Patch { start, cells } => {
                *start as usize + cells.len() <= TERMINAL_WIDTH as usize * TERMINAL_HEIGHT as usize
            }
            TerminalChange::Fill {
                x,
                y,
                width,
                height,
                ..
            } => {
                x.checked_add(*width)
                    .is_some_and(|end| end <= TERMINAL_WIDTH)
                    && y.checked_add(*height)
                        .is_some_and(|end| end <= TERMINAL_HEIGHT)
            }
            TerminalChange::Scroll { rows, .. } => *rows <= TERMINAL_HEIGHT,
            TerminalChange::Cursor { .. } | TerminalChange::Reset => true,
        };
        if self.journal_capacity != expected.journal_capacity
            || self.journal.len() > self.journal_capacity
            || self.pending.len() > MAXIMUM_PENDING_CHANGES
            || self.pending.iter().any(|change| !valid(change))
        {
            return Err(CheckpointError::InvalidState);
        }
        let mut previous = None;
        for delta in &self.journal {
            if delta.base_revision.checked_add(1) != Some(delta.target_revision)
                || delta.target_revision > self.revision
                || previous.is_some_and(|previous| delta.base_revision != previous)
                || delta.changes.len() > MAXIMUM_PENDING_CHANGES
                || delta.changes.iter().any(|change| !valid(change))
            {
                return Err(CheckpointError::InvalidState);
            }
            previous = Some(delta.target_revision);
        }
        if previous.is_some_and(|previous| previous != self.revision) {
            return Err(CheckpointError::InvalidState);
        }
        Ok(())
    }
}
