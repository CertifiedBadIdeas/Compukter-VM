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

use super::input::{
    TerminalInputError, TerminalInputEvent, TerminalInputLimits, TerminalInputQueue,
    TerminalKeyEvent,
};
use super::replication::{
    ReplicationState, TerminalChange, TerminalCommit, TerminalSnapshot, TerminalUpdate,
};

pub const TERMINAL_WIDTH: u16 = 51;
pub const TERMINAL_HEIGHT: u16 = 19;
pub const TERMINAL_PALETTE_SIZE: u8 = 16;

const CELL_COUNT: usize = TERMINAL_WIDTH as usize * TERMINAL_HEIGHT as usize;
const DEFAULT_FOREGROUND: u8 = 15;
const DEFAULT_BACKGROUND: u8 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalError {
    InvalidPosition,
    InvalidRectangle,
    InvalidUnicodeScalar,
    InvalidPaletteIndex,
    PatchOutOfBounds,
    ScrollOutOfBounds,
    InvalidConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalConfig {
    pub input: TerminalInputLimits,
    pub journal_revisions: usize,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            input: TerminalInputLimits::default(),
            journal_revisions: 64,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalCell {
    code_point: u32,
    foreground: u8,
    background: u8,
}

impl TerminalCell {
    pub fn new(code_point: u32, foreground: u8, background: u8) -> Result<Self, TerminalError> {
        if char::from_u32(code_point).is_none() {
            return Err(TerminalError::InvalidUnicodeScalar);
        }
        validate_palette(foreground)?;
        validate_palette(background)?;
        Ok(Self {
            code_point,
            foreground,
            background,
        })
    }

    pub const fn code_point(self) -> u32 {
        self.code_point
    }

    pub const fn foreground(self) -> u8 {
        self.foreground
    }

    pub const fn background(self) -> u8 {
        self.background
    }

    fn blank(foreground: u8, background: u8) -> Self {
        Self {
            code_point: ' ' as u32,
            foreground,
            background,
        }
    }
}

impl Default for TerminalCell {
    fn default() -> Self {
        Self::blank(DEFAULT_FOREGROUND, DEFAULT_BACKGROUND)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalPosition {
    x: u16,
    y: u16,
}

impl TerminalPosition {
    pub fn new(x: u16, y: u16) -> Result<Self, TerminalError> {
        if x >= TERMINAL_WIDTH || y >= TERMINAL_HEIGHT {
            return Err(TerminalError::InvalidPosition);
        }
        Ok(Self { x, y })
    }

    pub const fn x(self) -> u16 {
        self.x
    }

    pub const fn y(self) -> u16 {
        self.y
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalRectangle {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

impl TerminalRectangle {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Result<Self, TerminalError> {
        if width == 0
            || height == 0
            || x >= TERMINAL_WIDTH
            || y >= TERMINAL_HEIGHT
            || x.checked_add(width).is_none_or(|end| end > TERMINAL_WIDTH)
            || y.checked_add(height)
                .is_none_or(|end| end > TERMINAL_HEIGHT)
        {
            return Err(TerminalError::InvalidRectangle);
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    pub const fn x(self) -> u16 {
        self.x
    }

    pub const fn y(self) -> u16 {
        self.y
    }

    pub const fn width(self) -> u16 {
        self.width
    }

    pub const fn height(self) -> u16 {
        self.height
    }
}

#[derive(Debug)]
pub struct TerminalDevice {
    cells: [TerminalCell; CELL_COUNT],
    row_head: usize,
    cursor: TerminalPosition,
    cursor_visible: bool,
    foreground: u8,
    background: u8,
    input: TerminalInputQueue,
    replication: ReplicationState,
}

impl Default for TerminalDevice {
    fn default() -> Self {
        Self::with_config(TerminalConfig::default()).expect("default terminal config is valid")
    }
}

impl TerminalDevice {
    pub fn with_config(config: TerminalConfig) -> Result<Self, TerminalError> {
        if config.journal_revisions == 0 {
            return Err(TerminalError::InvalidConfig);
        }
        Ok(Self {
            cells: [TerminalCell::default(); CELL_COUNT],
            row_head: 0,
            cursor: TerminalPosition { x: 0, y: 0 },
            cursor_visible: true,
            foreground: DEFAULT_FOREGROUND,
            background: DEFAULT_BACKGROUND,
            input: TerminalInputQueue::new(config.input),
            replication: ReplicationState::new(config.journal_revisions),
        })
    }
}

impl TerminalDevice {
    pub const fn dimensions(&self) -> (u16, u16) {
        (TERMINAL_WIDTH, TERMINAL_HEIGHT)
    }

    pub const fn revision(&self) -> u64 {
        self.replication.revision()
    }

    pub fn cell(&self, x: u16, y: u16) -> Result<TerminalCell, TerminalError> {
        let position = TerminalPosition::new(x, y)?;
        Ok(self.cells[self.physical_index(position)])
    }

    pub const fn cursor_position(&self) -> TerminalPosition {
        self.cursor
    }

    pub const fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    pub fn logical_cells(&self) -> impl ExactSizeIterator<Item = TerminalCell> + '_ {
        (0..CELL_COUNT).map(move |logical| {
            let position = TerminalPosition {
                x: (logical % TERMINAL_WIDTH as usize) as u16,
                y: (logical / TERMINAL_WIDTH as usize) as u16,
            };
            self.cells[self.physical_index(position)]
        })
    }

    pub fn set_cursor(&mut self, position: TerminalPosition) {
        if self.cursor == position {
            return;
        }
        self.cursor = position;
        self.record_cursor();
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        if self.cursor_visible == visible {
            return;
        }
        self.cursor_visible = visible;
        self.record_cursor();
    }

    pub fn set_colors(&mut self, foreground: u8, background: u8) -> Result<(), TerminalError> {
        validate_palette(foreground)?;
        validate_palette(background)?;
        self.foreground = foreground;
        self.background = background;
        Ok(())
    }

    pub fn patch(
        &mut self,
        start: TerminalPosition,
        cells: &[TerminalCell],
    ) -> Result<(), TerminalError> {
        let logical_start = start.y as usize * TERMINAL_WIDTH as usize + start.x as usize;
        if logical_start
            .checked_add(cells.len())
            .is_none_or(|end| end > CELL_COUNT)
        {
            return Err(TerminalError::PatchOutOfBounds);
        }
        for (offset, cell) in cells.iter().copied().enumerate() {
            let logical = logical_start + offset;
            let position = TerminalPosition {
                x: (logical % TERMINAL_WIDTH as usize) as u16,
                y: (logical / TERMINAL_WIDTH as usize) as u16,
            };
            let index = self.physical_index(position);
            self.cells[index] = cell;
        }
        if !cells.is_empty() {
            self.replication.record(TerminalChange::Patch {
                start: logical_start as u16,
                cells: cells.into(),
            });
        }
        Ok(())
    }

    pub fn fill(
        &mut self,
        rectangle: TerminalRectangle,
        cell: TerminalCell,
    ) -> Result<(), TerminalError> {
        for y in rectangle.y..rectangle.y + rectangle.height {
            for x in rectangle.x..rectangle.x + rectangle.width {
                let index = self.physical_index(TerminalPosition { x, y });
                self.cells[index] = cell;
            }
        }
        self.replication.record(TerminalChange::Fill {
            x: rectangle.x,
            y: rectangle.y,
            width: rectangle.width,
            height: rectangle.height,
            cell,
        });
        Ok(())
    }

    pub fn scroll(&mut self, rows: u16) -> Result<(), TerminalError> {
        if rows > TERMINAL_HEIGHT {
            return Err(TerminalError::ScrollOutOfBounds);
        }
        let fill = TerminalCell::default();
        self.scroll_with(rows, fill);
        if rows != 0 {
            self.replication
                .record(TerminalChange::Scroll { rows, fill });
        }
        Ok(())
    }

    pub fn write_utf16(&mut self, units: &[u16]) -> Result<(), TerminalError> {
        let initial_cursor = self.cursor;
        for decoded in char::decode_utf16(units.iter().copied()) {
            match decoded.unwrap_or(char::REPLACEMENT_CHARACTER) {
                '\n' => self.newline(),
                '\r' => self.cursor.x = 0,
                scalar => self.write_scalar(scalar),
            }
        }
        if self.cursor != initial_cursor {
            self.record_cursor();
        }
        Ok(())
    }

    pub fn erase_previous(&mut self) {
        if self.cursor.x > 0 {
            self.cursor.x -= 1;
        } else if self.cursor.y > 0 {
            self.cursor.x = TERMINAL_WIDTH - 1;
            self.cursor.y -= 1;
        } else {
            return;
        }
        let logical = self.cursor.y as usize * TERMINAL_WIDTH as usize + self.cursor.x as usize;
        let index = self.physical_index(self.cursor);
        let blank = TerminalCell::blank(self.foreground, self.background);
        self.cells[index] = blank;
        self.replication.record(TerminalChange::Patch {
            start: logical as u16,
            cells: vec![blank].into_boxed_slice(),
        });
        self.record_cursor();
    }

    pub fn clear(&mut self) {
        self.cells.fill(TerminalCell::default());
        self.row_head = 0;
        self.cursor = TerminalPosition { x: 0, y: 0 };
        self.cursor_visible = true;
        self.foreground = DEFAULT_FOREGROUND;
        self.background = DEFAULT_BACKGROUND;
        self.replication.record_reset();
    }

    pub fn push_key(&mut self, event: TerminalKeyEvent) -> Result<(), TerminalInputError> {
        self.input.push_key(event)
    }

    pub fn push_text(&mut self, text: &str) -> Result<(), TerminalInputError> {
        self.input.push_text(text)
    }

    pub fn poll_input(&mut self) -> Option<TerminalInputEvent> {
        self.input.poll()
    }

    pub fn commit(&mut self) -> TerminalCommit {
        let full_cells = self
            .replication
            .requires_full_replacement()
            .then(|| self.materialize_logical_cells());
        self.replication
            .commit(full_cells, self.cursor, self.cursor_visible)
    }

    pub fn changes_since(&self, base_revision: u64) -> TerminalUpdate {
        self.replication
            .delta_since(base_revision)
            .unwrap_or_else(|| TerminalUpdate::Full(self.snapshot()))
    }

    fn write_scalar(&mut self, scalar: char) {
        let index = self.physical_index(self.cursor);
        self.cells[index] = TerminalCell::blank(self.foreground, self.background);
        self.cells[index].code_point = scalar as u32;
        let logical = self.cursor.y as usize * TERMINAL_WIDTH as usize + self.cursor.x as usize;
        self.replication.record(TerminalChange::Patch {
            start: logical as u16,
            cells: vec![self.cells[index]].into_boxed_slice(),
        });
        if self.cursor.x + 1 < TERMINAL_WIDTH {
            self.cursor.x += 1;
        } else {
            self.newline();
        }
    }

    fn newline(&mut self) {
        self.cursor.x = 0;
        if self.cursor.y + 1 < TERMINAL_HEIGHT {
            self.cursor.y += 1;
        } else {
            let fill = TerminalCell::blank(self.foreground, self.background);
            self.scroll_with(1, fill);
            self.replication
                .record(TerminalChange::Scroll { rows: 1, fill });
        }
    }

    fn scroll_with(&mut self, rows: u16, fill: TerminalCell) {
        for _ in 0..rows {
            self.row_head = (self.row_head + 1) % TERMINAL_HEIGHT as usize;
            let reclaimed_row =
                (self.row_head + TERMINAL_HEIGHT as usize - 1) % TERMINAL_HEIGHT as usize;
            let start = reclaimed_row * TERMINAL_WIDTH as usize;
            self.cells[start..start + TERMINAL_WIDTH as usize].fill(fill);
        }
    }

    fn physical_index(&self, position: TerminalPosition) -> usize {
        let row = (self.row_head + position.y as usize) % TERMINAL_HEIGHT as usize;
        row * TERMINAL_WIDTH as usize + position.x as usize
    }

    fn record_cursor(&mut self) {
        self.replication.record(TerminalChange::Cursor {
            position: self.cursor,
            visible: self.cursor_visible,
        });
    }

    fn snapshot(&self) -> TerminalSnapshot {
        TerminalSnapshot::new(
            self.replication.revision(),
            self.materialize_logical_cells(),
            self.cursor,
            self.cursor_visible,
        )
    }

    fn materialize_logical_cells(&self) -> Box<[TerminalCell]> {
        self.logical_cells().collect::<Vec<_>>().into_boxed_slice()
    }
}

fn validate_palette(index: u8) -> Result<(), TerminalError> {
    if index >= TERMINAL_PALETTE_SIZE {
        Err(TerminalError::InvalidPaletteIndex)
    } else {
        Ok(())
    }
}
