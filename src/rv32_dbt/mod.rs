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

#![allow(
    dead_code,
    reason = "the direct DBT contracts are connected to the machine in subsequent issue #17 tasks"
)]

pub(crate) mod abi;
pub(crate) mod block;
pub(crate) mod code_cache;
pub(crate) mod executable;
pub(crate) mod ir;
#[cfg(feature = "dbt-execution-profile")]
pub(crate) mod profile;
#[cfg(target_arch = "x86_64")]
pub(crate) mod x86_64;

use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DbtFaultKind {
    BackendUnavailable,
    Capacity,
    Translation,
    ExecutableMemory,
    InvalidExit,
    AbiInvariant,
}

impl DbtFaultKind {
    const fn name(self) -> &'static str {
        match self {
            Self::BackendUnavailable => "backend unavailable",
            Self::Capacity => "capacity",
            Self::Translation => "translation",
            Self::ExecutableMemory => "executable memory",
            Self::InvalidExit => "invalid exit",
            Self::AbiInvariant => "ABI invariant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DbtFault {
    kind: DbtFaultKind,
    pc: u32,
    instruction_word: Option<u32>,
    message: String,
}

impl DbtFault {
    pub(crate) fn new(
        kind: DbtFaultKind,
        pc: u32,
        instruction_word: Option<u32>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            pc,
            instruction_word,
            message: message.into(),
        }
    }

    pub(crate) fn kind(&self) -> DbtFaultKind {
        self.kind
    }

    pub(crate) fn pc(&self) -> u32 {
        self.pc
    }

    pub(crate) fn instruction_word(&self) -> Option<u32> {
        self.instruction_word
    }
}

impl Display for DbtFault {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "RV32 DBT {} fault at PC {:#010x}",
            self.kind.name(),
            self.pc
        )?;
        if let Some(word) = self.instruction_word {
            write!(formatter, " for instruction {word:#010x}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for DbtFault {}

#[cfg(test)]
mod tests {
    use super::{DbtFault, DbtFaultKind};

    #[test]
    fn dbt_fault_keeps_machine_local_diagnostics() {
        let fault = DbtFault::new(
            DbtFaultKind::Translation,
            0x1200,
            Some(0xffff_ffff),
            "invalid lowering",
        );

        assert_eq!(fault.kind(), DbtFaultKind::Translation);
        assert_eq!(fault.pc(), 0x1200);
        assert_eq!(fault.instruction_word(), Some(0xffff_ffff));
        assert_eq!(fault.to_string(), "RV32 DBT translation fault at PC 0x00001200 for instruction 0xffffffff: invalid lowering");
    }
}
