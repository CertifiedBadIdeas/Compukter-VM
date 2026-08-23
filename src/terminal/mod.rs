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

mod input;
mod replication;
mod state;

pub use input::{
    TerminalInputError, TerminalInputEvent, TerminalInputLimits, TerminalKey, TerminalKeyAction,
    TerminalKeyEvent, TerminalModifiers,
};
pub use replication::{
    TerminalChange, TerminalCommit, TerminalDelta, TerminalSnapshot, TerminalUpdate,
};
pub use state::{
    TerminalCell, TerminalConfig, TerminalDevice, TerminalError, TerminalPosition,
    TerminalRectangle, TERMINAL_HEIGHT, TERMINAL_PALETTE_SIZE, TERMINAL_WIDTH,
};
