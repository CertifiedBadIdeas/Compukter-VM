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

use super::{FileSystemError, FileSystemLimits};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MutationCost {
    pub logical_bytes_added: u64,
    pub nodes_added: u32,
    pub journal_bytes: u64,
    pub queue_records: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct QuotaReservation(MutationCost);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QuotaLedger {
    maximum_logical_bytes: u64,
    maximum_nodes: u32,
    logical_bytes: u64,
    nodes: u32,
}

impl QuotaLedger {
    pub fn new(limits: &FileSystemLimits, initial_nodes: u32) -> Self {
        Self {
            maximum_logical_bytes: limits.maximum_logical_bytes,
            maximum_nodes: limits.maximum_nodes,
            logical_bytes: 0,
            nodes: initial_nodes,
        }
    }

    pub fn reserve(&self, cost: MutationCost) -> Result<QuotaReservation, FileSystemError> {
        let logical_bytes = self
            .logical_bytes
            .checked_add(cost.logical_bytes_added)
            .ok_or(FileSystemError::QuotaExceeded)?;
        let nodes = self
            .nodes
            .checked_add(cost.nodes_added)
            .ok_or(FileSystemError::QuotaExceeded)?;
        if logical_bytes > self.maximum_logical_bytes || nodes > self.maximum_nodes {
            return Err(FileSystemError::QuotaExceeded);
        }
        Ok(QuotaReservation(cost))
    }

    pub fn commit(&mut self, reservation: QuotaReservation) {
        self.logical_bytes += reservation.0.logical_bytes_added;
        self.nodes += reservation.0.nodes_added;
    }

    pub fn release(&mut self, logical_bytes: u64, nodes: u32) {
        self.logical_bytes -= logical_bytes;
        self.nodes -= nodes;
    }

    pub fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }
}
