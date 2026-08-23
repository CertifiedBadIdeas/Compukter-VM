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

use std::collections::VecDeque;

use super::{TerminalCell, TerminalPosition};

const MAXIMUM_PENDING_CHANGES: usize = 256;
const MAXIMUM_ACCUMULATED_CHANGES: usize = 4_096;
const MAXIMUM_ACCUMULATED_CELLS: usize = 8_192;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalChange {
    Patch {
        start: u16,
        cells: Box<[TerminalCell]>,
    },
    Fill {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        cell: TerminalCell,
    },
    Scroll {
        rows: u16,
        fill: TerminalCell,
    },
    Cursor {
        position: TerminalPosition,
        visible: bool,
    },
    Reset,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalDelta {
    base_revision: u64,
    target_revision: u64,
    changes: Box<[TerminalChange]>,
}

impl TerminalDelta {
    pub const fn base_revision(&self) -> u64 {
        self.base_revision
    }

    pub const fn target_revision(&self) -> u64 {
        self.target_revision
    }

    pub fn changes(&self) -> &[TerminalChange] {
        &self.changes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSnapshot {
    revision: u64,
    cells: Box<[TerminalCell]>,
    cursor: TerminalPosition,
    cursor_visible: bool,
}

impl TerminalSnapshot {
    pub(super) fn new(
        revision: u64,
        cells: Box<[TerminalCell]>,
        cursor: TerminalPosition,
        cursor_visible: bool,
    ) -> Self {
        Self {
            revision,
            cells,
            cursor,
            cursor_visible,
        }
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn cells(&self) -> &[TerminalCell] {
        &self.cells
    }

    pub const fn cursor_position(&self) -> TerminalPosition {
        self.cursor
    }

    pub const fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalCommit {
    Unchanged { revision: u64 },
    Committed(TerminalDelta),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalUpdate {
    Unchanged { revision: u64 },
    Delta(TerminalDelta),
    Full(TerminalSnapshot),
}

#[derive(Debug)]
pub(super) struct ReplicationState {
    revision: u64,
    journal_capacity: usize,
    pending: Vec<TerminalChange>,
    journal: VecDeque<TerminalDelta>,
    requires_full_replacement: bool,
}

impl ReplicationState {
    pub(super) fn new(journal_capacity: usize) -> Self {
        Self {
            revision: 0,
            journal_capacity,
            pending: Vec::new(),
            journal: VecDeque::new(),
            requires_full_replacement: false,
        }
    }

    pub(super) const fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn record(&mut self, change: TerminalChange) {
        if self.requires_full_replacement {
            return;
        }
        if let TerminalChange::Patch { start, cells } = &change {
            if let Some(TerminalChange::Patch {
                start: previous_start,
                cells: previous_cells,
            }) = self.pending.last_mut()
            {
                let previous_end = *previous_start as usize + previous_cells.len();
                if previous_end == *start as usize {
                    let mut joined = Vec::with_capacity(previous_cells.len() + cells.len());
                    joined.extend_from_slice(previous_cells);
                    joined.extend_from_slice(cells);
                    *previous_cells = joined.into_boxed_slice();
                    return;
                }
            }
        }
        self.pending.push(change);
        if self.pending.len() > MAXIMUM_PENDING_CHANGES {
            self.pending.clear();
            self.requires_full_replacement = true;
        }
    }

    pub(super) fn record_reset(&mut self) {
        self.pending.clear();
        self.requires_full_replacement = false;
        self.pending.push(TerminalChange::Reset);
    }

    pub(super) const fn requires_full_replacement(&self) -> bool {
        self.requires_full_replacement
    }

    pub(super) fn commit(
        &mut self,
        full_cells: Option<Box<[TerminalCell]>>,
        cursor: TerminalPosition,
        cursor_visible: bool,
    ) -> TerminalCommit {
        if self.requires_full_replacement {
            self.pending = vec![
                TerminalChange::Reset,
                TerminalChange::Patch {
                    start: 0,
                    cells: full_cells.expect("compacted commit requires full terminal cells"),
                },
                TerminalChange::Cursor {
                    position: cursor,
                    visible: cursor_visible,
                },
            ];
            self.requires_full_replacement = false;
        } else if self.pending.is_empty() {
            return TerminalCommit::Unchanged {
                revision: self.revision,
            };
        }
        let base_revision = self.revision;
        self.revision = self
            .revision
            .checked_add(1)
            .expect("terminal revision exhausted");
        let delta = TerminalDelta {
            base_revision,
            target_revision: self.revision,
            changes: std::mem::take(&mut self.pending).into_boxed_slice(),
        };
        self.journal.push_back(delta.clone());
        while self.journal.len() > self.journal_capacity {
            self.journal.pop_front();
        }
        TerminalCommit::Committed(delta)
    }

    pub(super) fn delta_since(&self, base_revision: u64) -> Option<TerminalUpdate> {
        if base_revision == self.revision {
            return Some(TerminalUpdate::Unchanged {
                revision: self.revision,
            });
        }
        if base_revision > self.revision {
            return None;
        }
        let mut expected = base_revision;
        let mut changes = Vec::new();
        let mut encoded_cells = 0;
        for delta in &self.journal {
            if delta.target_revision <= expected {
                continue;
            }
            if delta.base_revision != expected {
                return None;
            }
            if changes.len() + delta.changes.len() > MAXIMUM_ACCUMULATED_CHANGES {
                return None;
            }
            encoded_cells += delta.changes.iter().map(encoded_cell_count).sum::<usize>();
            if encoded_cells > MAXIMUM_ACCUMULATED_CELLS {
                return None;
            }
            changes.extend_from_slice(&delta.changes);
            expected = delta.target_revision;
            if expected == self.revision {
                return Some(TerminalUpdate::Delta(TerminalDelta {
                    base_revision,
                    target_revision: self.revision,
                    changes: changes.into_boxed_slice(),
                }));
            }
        }
        None
    }
}

fn encoded_cell_count(change: &TerminalChange) -> usize {
    match change {
        TerminalChange::Patch { cells, .. } => cells.len(),
        TerminalChange::Fill { .. } | TerminalChange::Scroll { .. } => 1,
        TerminalChange::Cursor { .. } | TerminalChange::Reset => 0,
    }
}
