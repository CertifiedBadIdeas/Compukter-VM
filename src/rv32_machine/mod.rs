/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

mod address_space;
mod csr;
#[cfg(target_arch = "x86_64")]
mod dbt;
mod elf;
mod hart;
mod machine;
mod platform;

pub use address_space::{Rv32AddressSpace, Rv32AddressSpaceError};
pub use elf::{
    Rv32ElfError, Rv32ElfErrorKind, Rv32ElfLoader, Rv32LoadedImage, Rv32PagePermissions,
};
pub use machine::{
    Rv32DbtCodeAlignment, Rv32ExecutionBackendConfig, Rv32Machine, Rv32MachineBuildError,
    Rv32MachineConfig, Rv32MachineExecutionError, Rv32MachineOutcome, Rv32TranslationLookupUnit,
    Rv32TranslationStats, DEFAULT_DBT_CACHE_SETS, DEFAULT_DBT_CODE_ALIGNMENT,
    DEFAULT_DBT_CODE_BYTES, DEFAULT_DBT_MAX_INSTRUCTIONS, DEFAULT_DBT_SCRATCH_BYTES,
};
pub use platform::{CONTROL_BASE, DEBUG_BASE, STATUS_BOOTING, STATUS_HALTED, STATUS_PANIC};

#[cfg(feature = "dbt-code-audit")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rv32DbtCodeSnapshot {
    pub generation: u64,
    pub used_bytes: Vec<u8>,
    pub support_code: Vec<Rv32DbtSupportCodeRange>,
    pub blocks: Vec<Rv32DbtCodeBlock>,
}

#[cfg(feature = "dbt-code-audit")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rv32DbtSupportCodeKind {
    CompletedExitStub,
}

#[cfg(feature = "dbt-code-audit")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rv32DbtSupportCodeRange {
    pub kind: Rv32DbtSupportCodeKind,
    pub offset: u32,
    pub length: u32,
}

#[cfg(feature = "dbt-code-audit")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rv32DbtCodeBlock {
    pub guest_pc: u32,
    pub generation: u64,
    pub offset: u32,
    pub length: u32,
    pub chain_entry_offset: u32,
    pub guest_instruction_count: u32,
    pub edges: Vec<Rv32DbtCodeEdge>,
}

#[cfg(feature = "dbt-code-audit")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rv32DbtCodeEdge {
    pub target_pc: u32,
    pub displacement_offset: u32,
    pub reset_target_offset: u32,
    pub linked: bool,
}

#[cfg(feature = "dbt-code-audit")]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Rv32DbtCodeSnapshotError {
    message: String,
}

#[cfg(feature = "dbt-code-audit")]
impl Rv32DbtCodeSnapshotError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rv32DbtStats {
    pub translations: u64,
    pub publications: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub metadata_evictions: u64,
    pub overlap_invalidations: u64,
    pub context_initializations: u64,
    pub native_dispatches: u64,
    pub typed_slow_exits: u64,
    pub chain_transitions: Option<u64>,
    pub budget_overshoot: u64,
    pub max_budget_overshoot: u32,
    pub links_established: u64,
    pub links_reset: u64,
    pub lowered_load_sites: u64,
    pub lowered_store_sites: u64,
    pub decoded_slots_built: u64,
    pub emitted_bytes: u64,
    pub reserved_bytes: usize,
    pub metadata_bytes: usize,
    #[cfg(feature = "dbt-translation-timing")]
    pub decode_nanos: u64,
    #[cfg(feature = "dbt-translation-timing")]
    pub lower_nanos: u64,
    #[cfg(feature = "dbt-translation-timing")]
    pub publish_nanos: u64,
    #[cfg(feature = "dbt-translation-timing")]
    pub timed_translations: u64,
}
