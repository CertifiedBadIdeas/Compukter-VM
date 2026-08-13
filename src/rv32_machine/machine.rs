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

#[cfg(target_arch = "x86_64")]
use super::dbt::{PreparedDbtBlock, Rv32DbtExecution, Rv32DbtPolicy};
use super::hart::{Rv32HartStep, Rv32MachineHart};
use super::platform::{self, ControlDevice, DebugDevice};
use super::{Rv32AddressSpace, Rv32AddressSpaceError, Rv32DbtStats, Rv32ElfError, Rv32ElfLoader};
#[cfg(feature = "dbt-code-audit")]
use super::{Rv32DbtCodeSnapshot, Rv32DbtCodeSnapshotError};
use crate::bus::{MachineBus, MmioDeviceId};
use crate::memory::{MemoryBus, MemoryFault};
#[cfg(target_arch = "x86_64")]
use crate::rv32_dbt::abi::{DbtContext, DbtExitRecord, DbtExitTag};
#[cfg(target_arch = "x86_64")]
use crate::rv32_dbt::block::DbtBlockMode;
#[cfg(target_arch = "x86_64")]
use crate::rv32_dbt::{DbtFault, DbtFaultKind};
#[cfg(target_arch = "x86_64")]
use crate::rv32im::{decode_product_word, fill_decoded_block, DecodedInstruction, Load, Store};
use crate::rv32im::{
    ends_basic_block, BoundedCachedRv32imProgram, BoundedDecodedBlockCache, PredecodedRv32imImage,
    Rv32ResolvedInstruction, Rv32imCacheStats,
};
use std::ops::Range;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rv32DbtCodeAlignment {
    BlockBase(usize),
    ChainEntry(usize),
}

impl Rv32DbtCodeAlignment {
    pub const fn bytes(self) -> usize {
        match self {
            Self::BlockBase(bytes) | Self::ChainEntry(bytes) => bytes,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rv32ExecutionBackendConfig {
    Cached {
        sets: usize,
    },
    Predecoded,
    BlockCached {
        sets: usize,
        max_instructions: usize,
    },
    DirectDbt {
        max_instructions: usize,
        scratch_bytes: usize,
    },
    CachedDbt {
        sets: usize,
        max_instructions: usize,
        scratch_bytes: usize,
        cache_bytes: usize,
        code_alignment: Rv32DbtCodeAlignment,
    },
}

pub const DEFAULT_DBT_CACHE_SETS: usize = 256;
pub const DEFAULT_DBT_MAX_INSTRUCTIONS: usize = 8;
pub const DEFAULT_DBT_SCRATCH_BYTES: usize = 8 * 1024;
pub const DEFAULT_DBT_CODE_BYTES: usize = 128 * 1024;
pub const DEFAULT_DBT_CODE_ALIGNMENT: Rv32DbtCodeAlignment = Rv32DbtCodeAlignment::BlockBase(32);

impl Default for Rv32ExecutionBackendConfig {
    fn default() -> Self {
        Self::CachedDbt {
            sets: DEFAULT_DBT_CACHE_SETS,
            max_instructions: DEFAULT_DBT_MAX_INSTRUCTIONS,
            scratch_bytes: DEFAULT_DBT_SCRATCH_BYTES,
            cache_bytes: DEFAULT_DBT_CODE_BYTES,
            code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rv32TranslationLookupUnit {
    Instruction,
    Block,
}

impl Rv32TranslationLookupUnit {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Instruction => "instruction",
            Self::Block => "block",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rv32TranslationStats {
    pub lookup_unit: Rv32TranslationLookupUnit,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub blocks_built: u64,
    pub decoded_slots_built: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rv32MachineConfig {
    pub ram_size: usize,
    pub debug_limit: usize,
    pub execution: Rv32ExecutionBackendConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rv32MachineOutcome {
    BudgetExhausted {
        retired_delta: u64,
        retired_total: u64,
    },
    Halted {
        exit_code: i32,
        retired_delta: u64,
        retired_total: u64,
    },
    Panicked {
        panic_code: i32,
        retired_delta: u64,
        retired_total: u64,
    },
}

#[derive(Debug, Error)]
pub enum Rv32MachineBuildError {
    #[error("invalid RV32 machine configuration: {0}")]
    Config(String),
    #[error(transparent)]
    Elf(#[from] Rv32ElfError),
    #[error(transparent)]
    AddressSpace(#[from] Rv32AddressSpaceError),
    #[error("RV32 machine memory/device construction failed: {0}")]
    Memory(#[from] MemoryFault),
    #[error("RV32 execution backend construction failed: {0}")]
    Backend(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "RV32 execution failed at PC {pc:#010x} after {retired_total} retired instructions: {message}"
)]
pub struct Rv32MachineExecutionError {
    pc: u32,
    retired_total: u64,
    message: String,
}

impl Rv32MachineExecutionError {
    pub fn pc(&self) -> u32 {
        self.pc
    }

    pub fn retired_total(&self) -> u64 {
        self.retired_total
    }
}

enum Rv32ExecutionBackend {
    Cached(BoundedCachedRv32imProgram),
    Predecoded(PredecodedRv32imImage),
    BlockCached(BoundedDecodedBlockCache),
    #[cfg(target_arch = "x86_64")]
    Dbt(Rv32DbtExecution),
}

enum Rv32DbtBackendBuild {
    Direct,
    Cached {
        sets: usize,
        cache_bytes: usize,
        code_alignment: Rv32DbtCodeAlignment,
    },
}

#[cfg(target_arch = "x86_64")]
fn build_dbt_backend(
    build: Rv32DbtBackendBuild,
    max_instructions: usize,
    scratch_bytes: usize,
    ram_len: u32,
) -> Result<Rv32ExecutionBackend, Rv32MachineBuildError> {
    let policy = match build {
        Rv32DbtBackendBuild::Direct => Rv32DbtPolicy::Direct,
        Rv32DbtBackendBuild::Cached {
            sets,
            cache_bytes,
            code_alignment,
        } => Rv32DbtPolicy::Cached {
            sets,
            cache_bytes,
            code_alignment,
        },
    };
    Rv32DbtExecution::new(policy, max_instructions, scratch_bytes, ram_len)
        .map(Rv32ExecutionBackend::Dbt)
        .map_err(|error| Rv32MachineBuildError::Backend(error.to_string()))
}

#[cfg(not(target_arch = "x86_64"))]
fn build_dbt_backend(
    _build: Rv32DbtBackendBuild,
    _max_instructions: usize,
    _scratch_bytes: usize,
    _ram_len: u32,
) -> Result<Rv32ExecutionBackend, Rv32MachineBuildError> {
    Err(Rv32MachineBuildError::Backend(
        "RV32 direct DBT is unavailable on non-x86_64 hosts".to_string(),
    ))
}

pub struct Rv32Machine {
    hart: Rv32MachineHart,
    address_space: Rv32AddressSpace,
    execution: Rv32ExecutionBackend,
    executable_ranges: Vec<Range<u32>>,
    control_device: MmioDeviceId,
    debug_device: MmioDeviceId,
}

impl Rv32Machine {
    pub fn from_elf(elf: &[u8], config: Rv32MachineConfig) -> Result<Self, Rv32MachineBuildError> {
        validate_config(config)?;
        let image = Rv32ElfLoader::load(elf, config.ram_size)?;
        let ram_len = u32::try_from(config.ram_size).map_err(|_| {
            Rv32MachineBuildError::Config("RAM size exceeds the RV32 address space".to_string())
        })?;
        let execution = match config.execution {
            Rv32ExecutionBackendConfig::Cached { sets } => Rv32ExecutionBackend::Cached(
                BoundedCachedRv32imProgram::new(sets).map_err(Rv32MachineBuildError::Backend)?,
            ),
            Rv32ExecutionBackendConfig::Predecoded => Rv32ExecutionBackend::Predecoded(
                PredecodedRv32imImage::new(image.ram(), image.executable_ranges())
                    .map_err(Rv32MachineBuildError::Backend)?,
            ),
            Rv32ExecutionBackendConfig::BlockCached {
                sets,
                max_instructions,
            } => Rv32ExecutionBackend::BlockCached(
                BoundedDecodedBlockCache::new(sets, max_instructions)
                    .map_err(Rv32MachineBuildError::Backend)?,
            ),
            Rv32ExecutionBackendConfig::DirectDbt {
                max_instructions,
                scratch_bytes,
            } => build_dbt_backend(
                Rv32DbtBackendBuild::Direct,
                max_instructions,
                scratch_bytes,
                ram_len,
            )?,
            Rv32ExecutionBackendConfig::CachedDbt {
                sets,
                max_instructions,
                scratch_bytes,
                cache_bytes,
                code_alignment,
            } => build_dbt_backend(
                Rv32DbtBackendBuild::Cached {
                    sets,
                    cache_bytes,
                    code_alignment,
                },
                max_instructions,
                scratch_bytes,
                ram_len,
            )?,
        };
        let (entry_point, ram, page_permissions, executable_ranges) = image.into_parts();
        let mut bus = MachineBus::new(ram.len())?;
        for (address, byte) in ram.into_iter().enumerate() {
            bus.memory_mut().store_u8(address as u32, byte)?;
        }

        let mut control = ControlDevice::new();
        control.status = platform::STATUS_BOOTING;
        let control_device = bus.map_mmio(platform::CONTROL_BASE, Box::new(control))?;
        let debug_device = bus.map_mmio(
            platform::DEBUG_BASE,
            Box::new(DebugDevice::with_limit(config.debug_limit)),
        )?;
        let address_space = Rv32AddressSpace::from_parts(bus, page_permissions)?;

        Ok(Self {
            hart: Rv32MachineHart::new(entry_point),
            address_space,
            execution,
            executable_ranges,
            control_device,
            debug_device,
        })
    }

    pub fn run(
        &mut self,
        instruction_budget: u64,
    ) -> Result<Rv32MachineOutcome, Rv32MachineExecutionError> {
        match self.execution {
            Rv32ExecutionBackend::BlockCached(_) => self.run_block_cached(instruction_budget),
            #[cfg(target_arch = "x86_64")]
            Rv32ExecutionBackend::Dbt(_) => self.run_dbt(instruction_budget),
            Rv32ExecutionBackend::Cached(_) | Rv32ExecutionBackend::Predecoded(_) => {
                self.run_single_instruction(instruction_budget)
            }
        }
    }

    fn run_single_instruction(
        &mut self,
        instruction_budget: u64,
    ) -> Result<Rv32MachineOutcome, Rv32MachineExecutionError> {
        let retired_before = self.hart.retired_instructions();
        if let Some(outcome) = self.terminal_outcome(retired_before) {
            return Ok(outcome);
        }
        for _ in 0..instruction_budget {
            let instruction_pc = self.hart.pc();
            if !instruction_pc.is_multiple_of(4) {
                self.hart
                    .take_instruction_address_misaligned(instruction_pc);
            } else if !self.is_executable_pc(instruction_pc) {
                self.hart.take_instruction_access_fault(instruction_pc);
            } else {
                let resolved = match &mut self.execution {
                    Rv32ExecutionBackend::Cached(cache) => {
                        match cache.resolve(instruction_pc, &self.address_space) {
                            Ok(resolved) => resolved,
                            Err(error) => {
                                self.hart.take_instruction_access_fault(
                                    error.address().unwrap_or(instruction_pc),
                                );
                                if let Some(outcome) = self.terminal_outcome(retired_before) {
                                    return Ok(outcome);
                                }
                                continue;
                            }
                        }
                    }
                    Rv32ExecutionBackend::Predecoded(image) => image
                        .resolve(instruction_pc)
                        .map_err(|message| self.execution_error(instruction_pc, message))?,
                    Rv32ExecutionBackend::BlockCached(_) => {
                        unreachable!("block backend uses the block execution loop")
                    }
                    #[cfg(target_arch = "x86_64")]
                    Rv32ExecutionBackend::Dbt(_) => {
                        unreachable!("DBT backend uses the explicit DBT execution loop")
                    }
                };
                match resolved {
                    Rv32ResolvedInstruction::Valid { word, instruction } => {
                        self.hart.execute_resolved(
                            &mut self.address_space,
                            instruction_pc,
                            word,
                            instruction,
                        );
                    }
                    Rv32ResolvedInstruction::Invalid { word } => {
                        self.hart.take_illegal_instruction(word);
                    }
                }
            }
            if let Some(outcome) = self.terminal_outcome(retired_before) {
                return Ok(outcome);
            }
        }
        Ok(Rv32MachineOutcome::BudgetExhausted {
            retired_delta: self
                .hart
                .retired_instructions()
                .saturating_sub(retired_before),
            retired_total: self.hart.retired_instructions(),
        })
    }

    fn run_block_cached(
        &mut self,
        instruction_budget: u64,
    ) -> Result<Rv32MachineOutcome, Rv32MachineExecutionError> {
        let retired_before = self.hart.retired_instructions();
        if let Some(outcome) = terminal_outcome(
            &self.hart,
            &self.address_space,
            self.control_device,
            retired_before,
        ) {
            return Ok(outcome);
        }
        let mut attempted = 0;
        while attempted < instruction_budget {
            let instruction_pc = self.hart.pc();
            if !instruction_pc.is_multiple_of(4) {
                attempted += 1;
                self.hart
                    .take_instruction_address_misaligned(instruction_pc);
            } else if self.executable_range_end(instruction_pc).is_none() {
                attempted += 1;
                self.hart.take_instruction_access_fault(instruction_pc);
            } else {
                let executable_end = self
                    .executable_range_end(instruction_pc)
                    .expect("executable PC has an owning ELF range");
                let Rv32ExecutionBackend::BlockCached(cache) = &mut self.execution else {
                    unreachable!("block execution loop requires the block backend")
                };
                let block = match cache.resolve(instruction_pc, executable_end, &self.address_space)
                {
                    Ok(block) => block,
                    Err(error) => {
                        attempted += 1;
                        self.hart.take_instruction_access_fault(
                            error.address().unwrap_or(instruction_pc),
                        );
                        if let Some(outcome) = terminal_outcome(
                            &self.hart,
                            &self.address_space,
                            self.control_device,
                            retired_before,
                        ) {
                            return Ok(outcome);
                        }
                        continue;
                    }
                };
                for (slot_index, slot) in block.iter().copied().enumerate() {
                    if attempted >= instruction_budget {
                        break;
                    }
                    let slot_pc = instruction_pc.wrapping_add((slot_index as u32) * 4);
                    if self.hart.pc() != slot_pc {
                        break;
                    }
                    attempted += 1;
                    let step = execute_slot(&mut self.hart, &mut self.address_space, slot_pc, slot);
                    if let Some(outcome) = terminal_outcome(
                        &self.hart,
                        &self.address_space,
                        self.control_device,
                        retired_before,
                    ) {
                        return Ok(outcome);
                    }
                    if step == Rv32HartStep::TrapTaken
                        || ends_basic_block(slot)
                        || self.hart.pc() != slot_pc.wrapping_add(4)
                    {
                        break;
                    }
                }
            }
            if let Some(outcome) = terminal_outcome(
                &self.hart,
                &self.address_space,
                self.control_device,
                retired_before,
            ) {
                return Ok(outcome);
            }
        }
        Ok(Rv32MachineOutcome::BudgetExhausted {
            retired_delta: self
                .hart
                .retired_instructions()
                .saturating_sub(retired_before),
            retired_total: self.hart.retired_instructions(),
        })
    }

    #[cfg(target_arch = "x86_64")]
    fn run_dbt(
        &mut self,
        instruction_budget: u64,
    ) -> Result<Rv32MachineOutcome, Rv32MachineExecutionError> {
        let retired_before = self.hart.retired_instructions();
        if let Some(outcome) = self.terminal_outcome(retired_before) {
            return Ok(outcome);
        }
        let mut context =
            create_dbt_context(&mut self.hart, &mut self.address_space).map_err(|error| {
                Rv32MachineExecutionError {
                    pc: self.hart.pc(),
                    retired_total: self.hart.retired_instructions(),
                    message: error.to_string(),
                }
            })?;
        let Rv32ExecutionBackend::Dbt(execution) = &mut self.execution else {
            unreachable!("DBT loop requires the DBT backend")
        };
        execution.record_context_initialization();
        let mut attempted = 0_u64;
        while attempted < instruction_budget {
            let instruction_pc = self.hart.pc();
            if !instruction_pc.is_multiple_of(4) {
                attempted += 1;
                self.hart
                    .take_instruction_address_misaligned(instruction_pc);
            } else if self.executable_range_end(instruction_pc).is_none() {
                attempted += 1;
                self.hart.take_instruction_access_fault(instruction_pc);
            } else {
                let executable_end = self
                    .executable_range_end(instruction_pc)
                    .expect("executable PC has an owning ELF range");
                let remaining = instruction_budget - attempted;
                let cached = {
                    let Rv32ExecutionBackend::Dbt(execution) = &mut self.execution else {
                        unreachable!("DBT loop requires the DBT backend")
                    };
                    execution.lookup(instruction_pc, execution.fast_mode())
                };
                let prepared = if cached.is_some_and(|prepared| {
                    prepared.is_cached() || u64::from(prepared.instruction_count()) <= remaining
                }) {
                    cached.unwrap()
                } else {
                    #[cfg(feature = "dbt-translation-timing")]
                    let decode_started = {
                        let Rv32ExecutionBackend::Dbt(execution) = &self.execution else {
                            unreachable!("DBT loop requires the DBT backend")
                        };
                        execution
                            .translation_timing_enabled()
                            .then(std::time::Instant::now)
                    };
                    let fill_result = {
                        let Rv32ExecutionBackend::Dbt(execution) = &mut self.execution else {
                            unreachable!("DBT loop requires the DBT backend")
                        };
                        fill_decoded_block(
                            instruction_pc,
                            executable_end,
                            execution.max_instructions(),
                            &self.address_space,
                            execution.decoded_slots_mut(),
                        )
                    };
                    #[cfg(feature = "dbt-translation-timing")]
                    {
                        let Rv32ExecutionBackend::Dbt(execution) = &mut self.execution else {
                            unreachable!("DBT loop requires the DBT backend")
                        };
                        if let Some(decode_started) = decode_started {
                            execution.record_decode_nanos(decode_started.elapsed().as_nanos());
                        }
                    }
                    if let Err(error) = fill_result {
                        attempted += 1;
                        self.hart.take_instruction_access_fault(
                            error.address().unwrap_or(instruction_pc),
                        );
                        if let Some(outcome) = self.terminal_outcome(retired_before) {
                            return Ok(outcome);
                        }
                        continue;
                    }
                    let mode = {
                        let Rv32ExecutionBackend::Dbt(execution) = &self.execution else {
                            unreachable!("DBT loop requires the DBT backend")
                        };
                        if execution.fast_mode() == DbtBlockMode::ChainableThroughput
                            || execution.decoded_slots().len() as u64 <= remaining
                        {
                            execution.fast_mode()
                        } else {
                            DbtBlockMode::Bounded {
                                max_attempts: remaining as u32,
                            }
                        }
                    };
                    let Rv32ExecutionBackend::Dbt(execution) = &mut self.execution else {
                        unreachable!("DBT loop requires the DBT backend")
                    };
                    execution.translate(instruction_pc, mode).map_err(|error| {
                        Rv32MachineExecutionError {
                            pc: instruction_pc,
                            retired_total: self.hart.retired_instructions(),
                            message: error.to_string(),
                        }
                    })?
                };

                let (tag, exit, reservation_valid, reservation_address) = {
                    refresh_dbt_context(&mut self.hart, &mut context, remaining);
                    let Rv32ExecutionBackend::Dbt(execution) = &mut self.execution else {
                        unreachable!("DBT loop requires the DBT backend")
                    };
                    execute_prepared_dbt(execution, prepared, &mut context).map_err(|error| {
                        Rv32MachineExecutionError {
                            pc: instruction_pc,
                            retired_total: self.hart.retired_instructions(),
                            message: error.to_string(),
                        }
                    })?
                };
                let overshoot = u64::from(exit.attempted).saturating_sub(remaining);
                let max_overshoot = if prepared.is_cached() {
                    let Rv32ExecutionBackend::Dbt(execution) = &self.execution else {
                        unreachable!("DBT loop requires the DBT backend")
                    };
                    execution.max_instructions().saturating_sub(1) as u64
                } else {
                    0
                };
                if exit.attempted == 0
                    || (!prepared.is_cached() && exit.attempted > prepared.instruction_count())
                    || overshoot > max_overshoot
                    || exit.next_pc != self.hart.pc()
                {
                    return Err(self.execution_error(
                        instruction_pc,
                        format!(
                            "invalid DBT exit {:?}: attempted {}, block {}, remaining {}, next PC {:#010x}, hart PC {:#010x}",
                            tag,
                            exit.attempted,
                            prepared.instruction_count(),
                            remaining,
                            exit.next_pc,
                            self.hart.pc(),
                        ),
                    ));
                }
                if overshoot != 0 {
                    let Rv32ExecutionBackend::Dbt(execution) = &mut self.execution else {
                        unreachable!("DBT loop requires the DBT backend")
                    };
                    execution.record_budget_overshoot(overshoot as u32);
                }

                attempted = attempted.saturating_add(u64::from(exit.attempted));
                match tag {
                    DbtExitTag::Completed | DbtExitTag::BudgetExhausted => {
                        if tag == DbtExitTag::BudgetExhausted
                            && u64::from(exit.attempted) != remaining
                        {
                            return Err(self.execution_error(
                                instruction_pc,
                                "DBT budget exit did not consume the exact remaining budget"
                                    .to_string(),
                            ));
                        }
                        self.hart
                            .commit_dbt_prefix(
                                exit.attempted,
                                reservation_valid,
                                reservation_address,
                            )
                            .map_err(|message| {
                                self.execution_error(instruction_pc, message.to_string())
                            })?;
                    }
                    DbtExitTag::SlowInstruction | DbtExitTag::MemoryAccess => {
                        let prefix = exit.attempted - 1;
                        if exit.instruction_pc != self.hart.pc() {
                            return Err(self.execution_error(
                                instruction_pc,
                                format!(
                                    "DBT typed exit PC {:#010x} disagrees with hart PC {:#010x}",
                                    exit.instruction_pc,
                                    self.hart.pc(),
                                ),
                            ));
                        }
                        self.hart
                            .commit_dbt_prefix(prefix, reservation_valid, reservation_address)
                            .map_err(|message| {
                                self.execution_error(instruction_pc, message.to_string())
                            })?;
                        let word =
                            self.address_space
                                .load_i32(exit.instruction_pc)
                                .map_err(|error| {
                                    self.execution_error(instruction_pc, error.to_string())
                                })? as u32;
                        if word != exit.instruction_word {
                            return Err(self.execution_error(
                                instruction_pc,
                                format!(
                                    "DBT typed exit word {:#010x} disagrees with memory word {word:#010x}",
                                    exit.instruction_word,
                                ),
                            ));
                        }
                        let resolved = match decode_product_word(word) {
                            Ok(instruction) => {
                                if tag == DbtExitTag::MemoryAccess {
                                    validate_memory_exit(&self.hart, instruction, exit).map_err(
                                        |message| {
                                            self.execution_error(
                                                instruction_pc,
                                                message.to_string(),
                                            )
                                        },
                                    )?;
                                } else if exit.address != 0 || exit.access_size != 0 {
                                    return Err(self.execution_error(
                                        instruction_pc,
                                        "DBT slow-instruction exit carried memory metadata"
                                            .to_string(),
                                    ));
                                }
                                Rv32ResolvedInstruction::Valid { word, instruction }
                            }
                            Err(_) => Rv32ResolvedInstruction::Invalid { word },
                        };
                        let invalidate_code = matches!(
                            resolved,
                            Rv32ResolvedInstruction::Valid {
                                instruction: DecodedInstruction::FenceI,
                                ..
                            }
                        );
                        execute_slot(
                            &mut self.hart,
                            &mut self.address_space,
                            exit.instruction_pc,
                            resolved,
                        );
                        if invalidate_code {
                            let Rv32ExecutionBackend::Dbt(execution) = &mut self.execution else {
                                unreachable!("DBT loop requires the DBT backend")
                            };
                            execution.invalidate_all();
                        }
                    }
                }
            }
            if let Some(outcome) = self.terminal_outcome(retired_before) {
                return Ok(outcome);
            }
        }
        Ok(Rv32MachineOutcome::BudgetExhausted {
            retired_delta: self
                .hart
                .retired_instructions()
                .saturating_sub(retired_before),
            retired_total: self.hart.retired_instructions(),
        })
    }

    pub fn dbt_stats(&self) -> Option<Rv32DbtStats> {
        #[cfg(target_arch = "x86_64")]
        if let Rv32ExecutionBackend::Dbt(execution) = &self.execution {
            return Some(execution.stats());
        }
        None
    }

    #[cfg(feature = "dbt-code-audit")]
    pub fn dbt_code_snapshot(
        &self,
    ) -> Result<Option<Rv32DbtCodeSnapshot>, Rv32DbtCodeSnapshotError> {
        #[cfg(target_arch = "x86_64")]
        if let Rv32ExecutionBackend::Dbt(execution) = &self.execution {
            return execution.code_snapshot();
        }
        Ok(None)
    }

    #[cfg(feature = "dbt-translation-timing")]
    pub fn enable_dbt_translation_timing(&mut self) {
        #[cfg(target_arch = "x86_64")]
        if let Rv32ExecutionBackend::Dbt(execution) = &mut self.execution {
            execution.enable_translation_timing();
        }
    }

    pub fn debug_bytes(&self) -> &[u8] {
        self.address_space
            .bus()
            .device::<DebugDevice>(self.debug_device)
            .expect("RV32 machine debug device invariant")
            .bytes()
    }

    pub fn control_status(&self) -> i32 {
        self.control().status
    }

    pub fn retired_instructions(&self) -> u64 {
        self.hart.retired_instructions()
    }

    pub fn pc(&self) -> u32 {
        self.hart.pc()
    }

    pub fn cache_stats(&self) -> Option<Rv32imCacheStats> {
        match &self.execution {
            Rv32ExecutionBackend::Cached(cache) => Some(cache.stats()),
            Rv32ExecutionBackend::Predecoded(_) | Rv32ExecutionBackend::BlockCached(_) => None,
            #[cfg(target_arch = "x86_64")]
            Rv32ExecutionBackend::Dbt(_) => None,
        }
    }

    pub fn translation_stats(&self) -> Option<Rv32TranslationStats> {
        match &self.execution {
            Rv32ExecutionBackend::Cached(cache) => {
                let stats = cache.stats();
                Some(Rv32TranslationStats {
                    lookup_unit: Rv32TranslationLookupUnit::Instruction,
                    hits: stats.hits,
                    misses: stats.misses,
                    evictions: stats.evictions,
                    blocks_built: 0,
                    decoded_slots_built: 0,
                })
            }
            Rv32ExecutionBackend::Predecoded(_) => None,
            Rv32ExecutionBackend::BlockCached(cache) => {
                let stats = cache.stats();
                Some(Rv32TranslationStats {
                    lookup_unit: Rv32TranslationLookupUnit::Block,
                    hits: stats.hits,
                    misses: stats.misses,
                    evictions: stats.evictions,
                    blocks_built: stats.blocks_built,
                    decoded_slots_built: stats.decoded_slots_built,
                })
            }
            #[cfg(target_arch = "x86_64")]
            Rv32ExecutionBackend::Dbt(execution) => {
                let stats = execution.stats();
                Some(Rv32TranslationStats {
                    lookup_unit: Rv32TranslationLookupUnit::Block,
                    hits: stats.hits,
                    misses: stats.misses,
                    evictions: stats.evictions,
                    blocks_built: stats.translations,
                    decoded_slots_built: stats.decoded_slots_built,
                })
            }
        }
    }

    pub fn executable_bytes(&self) -> usize {
        self.executable_ranges.iter().map(Range::len).sum()
    }

    pub fn translation_bytes(&self) -> usize {
        match &self.execution {
            Rv32ExecutionBackend::Cached(cache) => cache.retained_bytes(),
            Rv32ExecutionBackend::Predecoded(image) => image.retained_bytes(),
            Rv32ExecutionBackend::BlockCached(cache) => cache.retained_bytes(),
            #[cfg(target_arch = "x86_64")]
            Rv32ExecutionBackend::Dbt(execution) => execution.retained_bytes(),
        }
    }

    fn executable_range_end(&self, pc: u32) -> Option<u32> {
        let range_end = self
            .executable_ranges
            .iter()
            .find(|range| range.contains(&pc))
            .map(|range| range.end)?;
        let page_executable = self.address_space.page_permissions(pc).executable();
        page_executable.then_some(range_end)
    }

    fn is_executable_pc(&self, pc: u32) -> bool {
        let in_range = self
            .executable_ranges
            .iter()
            .any(|range| range.contains(&pc));
        let page_executable = self.address_space.page_permissions(pc).executable();
        in_range && page_executable
    }

    fn terminal_outcome(&self, retired_before: u64) -> Option<Rv32MachineOutcome> {
        let retired_total = self.hart.retired_instructions();
        let retired_delta = retired_total.saturating_sub(retired_before);
        let control = self.control();
        match control.status {
            platform::STATUS_HALTED => Some(Rv32MachineOutcome::Halted {
                exit_code: control.exit_code,
                retired_delta,
                retired_total,
            }),
            platform::STATUS_PANIC => Some(Rv32MachineOutcome::Panicked {
                panic_code: control.panic_code,
                retired_delta,
                retired_total,
            }),
            _ => None,
        }
    }

    fn control(&self) -> &ControlDevice {
        self.address_space
            .bus()
            .device::<ControlDevice>(self.control_device)
            .expect("RV32 machine control device invariant")
    }

    fn execution_error(&self, pc: u32, message: String) -> Rv32MachineExecutionError {
        Rv32MachineExecutionError {
            pc,
            retired_total: self.hart.retired_instructions(),
            message,
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn execute_prepared_dbt(
    execution: &mut Rv32DbtExecution,
    prepared: PreparedDbtBlock,
    context: &mut DbtContext,
) -> Result<(DbtExitTag, DbtExitRecord, u32, u32), DbtFault> {
    let tag = unsafe { execution.execute(prepared, context) }?;
    Ok((
        tag,
        context.exit,
        context.reservation_valid,
        context.reservation_address,
    ))
}

#[cfg(target_arch = "x86_64")]
fn create_dbt_context(
    hart: &mut Rv32MachineHart,
    address_space: &mut Rv32AddressSpace,
) -> Result<DbtContext, DbtFault> {
    let (state, reservation_valid, reservation_address) = hart.dbt_state();
    let view = address_space.direct_ram_view();
    let ram_len = u32::try_from(view.len()).map_err(|_| {
        DbtFault::new(
            DbtFaultKind::AbiInvariant,
            0,
            None,
            "RV32 RAM length exceeds the DBT ABI",
        )
    })?;
    let page_count = u32::try_from(view.page_count()).map_err(|_| {
        DbtFault::new(
            DbtFaultKind::AbiInvariant,
            0,
            None,
            "RV32 permission page count exceeds the DBT ABI",
        )
    })?;
    Ok(DbtContext {
        state,
        ram_base: view.base(),
        ram_len,
        page_permissions: view.page_permissions(),
        page_count,
        remaining_budget: 0,
        reservation_valid,
        reservation_address,
        chain_transitions: 0,
        exit: DbtExitRecord::default(),
    })
}

#[cfg(target_arch = "x86_64")]
fn refresh_dbt_context(
    hart: &mut Rv32MachineHart,
    context: &mut DbtContext,
    remaining_budget: u64,
) {
    let (state, reservation_valid, reservation_address) = hart.dbt_state();
    debug_assert_eq!(context.state, state);
    context.remaining_budget = remaining_budget.min(u64::from(u32::MAX)) as u32;
    context.reservation_valid = reservation_valid;
    context.reservation_address = reservation_address;
    context.chain_transitions = 0;
    context.exit = DbtExitRecord::default();
}

#[cfg(target_arch = "x86_64")]
fn validate_memory_exit(
    hart: &Rv32MachineHart,
    instruction: DecodedInstruction,
    exit: DbtExitRecord,
) -> Result<(), String> {
    let (base, immediate, width) = match instruction {
        DecodedInstruction::Load {
            kind,
            rs1,
            immediate,
            ..
        } => {
            let width = match kind {
                Load::Byte | Load::ByteU => 1,
                Load::Half | Load::HalfU => 2,
                Load::Word => 4,
            };
            (hart.register(rs1), immediate, width)
        }
        DecodedInstruction::Store {
            kind,
            rs1,
            immediate,
            ..
        } => {
            let width = match kind {
                Store::Byte => 1,
                Store::Half => 2,
                Store::Word => 4,
            };
            (hart.register(rs1), immediate, width)
        }
        _ => {
            return Err(
                "DBT memory exit does not identify an RV32 load or store instruction".to_string(),
            )
        }
    };
    let address = base.wrapping_add(immediate as u32);
    if exit.address != address || exit.access_size != width {
        return Err(format!(
            "DBT memory exit reported address {:#010x}/size {}, expected {address:#010x}/size {width}",
            exit.address, exit.access_size,
        ));
    }
    Ok(())
}

fn execute_slot(
    hart: &mut Rv32MachineHart,
    address_space: &mut Rv32AddressSpace,
    instruction_pc: u32,
    resolved: Rv32ResolvedInstruction,
) -> Rv32HartStep {
    match resolved {
        Rv32ResolvedInstruction::Valid { word, instruction } => {
            hart.execute_resolved(address_space, instruction_pc, word, instruction)
        }
        Rv32ResolvedInstruction::Invalid { word } => hart.take_illegal_instruction(word),
    }
}

fn terminal_outcome(
    hart: &Rv32MachineHart,
    address_space: &Rv32AddressSpace,
    control_device: MmioDeviceId,
    retired_before: u64,
) -> Option<Rv32MachineOutcome> {
    let retired_total = hart.retired_instructions();
    let retired_delta = retired_total.saturating_sub(retired_before);
    let control = address_space
        .bus()
        .device::<ControlDevice>(control_device)
        .expect("RV32 machine control device invariant");
    match control.status {
        platform::STATUS_HALTED => Some(Rv32MachineOutcome::Halted {
            exit_code: control.exit_code,
            retired_delta,
            retired_total,
        }),
        platform::STATUS_PANIC => Some(Rv32MachineOutcome::Panicked {
            panic_code: control.panic_code,
            retired_delta,
            retired_total,
        }),
        _ => None,
    }
}

fn validate_config(config: Rv32MachineConfig) -> Result<(), Rv32MachineBuildError> {
    if config.ram_size == 0 {
        return Err(Rv32MachineBuildError::Config(
            "RAM size must be positive".to_string(),
        ));
    }
    if config.ram_size > platform::CONTROL_BASE as usize {
        return Err(Rv32MachineBuildError::Config(format!(
            "RAM size {} overlaps control MMIO at {:#010x}",
            config.ram_size,
            platform::CONTROL_BASE,
        )));
    }
    Ok(())
}
