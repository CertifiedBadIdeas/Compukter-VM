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

use super::rv32::rv32_workload;
use super::IsaBenchmarkWorkload;
use crate::memory::MachineMemory;
use crate::rv32_dbt::abi::{DbtContext, DbtEntry, DbtExitRecord, DbtExitTag};
use crate::rv32_dbt::block::{DbtBlockInput, DbtBlockMode};
use crate::rv32_dbt::code_cache::{DbtCacheKey, DirectDbtCodeCache};
use crate::rv32_dbt::executable::ExecutableScratch;
use crate::rv32_dbt::x86_64::lower::DbtTranslationWorkspace;
use crate::rv32im::{
    decode_product_word, fill_decoded_block, DecodedInstruction, Rv32ResolvedInstruction, Rv32imCpu,
};
use std::time::Instant;

const EXECUTABLE_BYTES: usize = 64 * 1024;
const CACHE_SETS: usize = 64;
const MAX_BLOCK_INSTRUCTIONS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectDbtComputeObservation {
    pub checksum: u32,
    pub dispatches: u64,
    pub attempted_instructions: u64,
    pub translation_nanos: u128,
    pub publication_nanos: u128,
    pub execution_nanos: u128,
    pub translated_bytes: u64,
    pub reserved_bytes: usize,
}

pub struct PreparedDirectDbtCompute32 {
    iterations: u32,
    words: Vec<u32>,
    result_register: u8,
    memory: MachineMemory,
    scratch: ExecutableScratch,
    workspace: DbtTranslationWorkspace,
    decoded: Vec<Rv32ResolvedInstruction>,
}

impl PreparedDirectDbtCompute32 {
    pub fn new(iterations: u32) -> Result<Self, String> {
        if iterations == 0 {
            return Err("direct DBT compute32 iterations must be positive".to_string());
        }
        let image = rv32_workload(IsaBenchmarkWorkload::Compute32, iterations)?;
        let code = image
            .words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let memory = MachineMemory::from_sections(code.len(), &code, &[], 0)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            iterations,
            words: image.words,
            result_register: image.result_register,
            memory,
            scratch: ExecutableScratch::new(EXECUTABLE_BYTES).map_err(|error| error.to_string())?,
            workspace: DbtTranslationWorkspace::new(EXECUTABLE_BYTES, MAX_BLOCK_INSTRUCTIONS)
                .map_err(|error| error.to_string())?,
            decoded: Vec::with_capacity(MAX_BLOCK_INSTRUCTIONS),
        })
    }

    pub fn execute(&mut self) -> Result<DirectDbtComputeObservation, String> {
        let executable_end = u32::try_from(self.words.len() * 4)
            .map_err(|_| "compute32 executable exceeds RV32 address space".to_string())?;
        let max_dispatches = u64::from(self.iterations)
            .checked_mul(3)
            .and_then(|count| count.checked_add(16))
            .ok_or_else(|| "compute32 DBT dispatch limit overflow".to_string())?;
        let mut cpu = Rv32imCpu::new(0);
        let mut dispatches = 0_u64;
        let mut attempted_instructions = 0_u64;
        let mut translation_nanos = 0_u128;
        let mut publication_nanos = 0_u128;
        let mut execution_nanos = 0_u128;
        let mut translated_bytes = 0_u64;

        loop {
            if dispatches >= max_dispatches {
                return Err(format!(
                    "compute32 direct DBT exceeded {max_dispatches} dispatches at PC {:#010x}",
                    cpu.pc()
                ));
            }
            fill_decoded_block(
                cpu.pc(),
                executable_end,
                MAX_BLOCK_INSTRUCTIONS,
                &self.memory,
                &mut self.decoded,
            )
            .map_err(|error| error.to_string())?;

            let translation_started = Instant::now();
            let input = DbtBlockInput::new(cpu.pc(), &self.decoded, DbtBlockMode::Fast)?;
            let compiled = self
                .workspace
                .lower(&input)
                .map_err(|error| error.to_string())?;
            translation_nanos += translation_started.elapsed().as_nanos();
            translated_bytes = translated_bytes.saturating_add(compiled.code().len() as u64);

            let publication_started = Instant::now();
            self.scratch
                .publish(compiled.code())
                .map_err(|error| error.to_string())?;
            publication_nanos += publication_started.elapsed().as_nanos();

            let mut context = DbtContext {
                state: cpu.architectural_state_mut(),
                ram_base: self.memory.as_mut_ptr(),
                ram_len: self.memory.len() as u32,
                page_permissions: std::ptr::null(),
                page_count: 0,
                remaining_budget: u32::MAX,
                reservation_valid: 0,
                reservation_address: 0,
                exit: DbtExitRecord::default(),
            };
            let entry: DbtEntry = unsafe {
                std::mem::transmute(
                    self.scratch
                        .entry_address()
                        .ok_or_else(|| "direct DBT scratch is not executable".to_string())?,
                )
            };
            let execution_started = Instant::now();
            let raw_tag = unsafe { entry(&mut context) };
            execution_nanos += execution_started.elapsed().as_nanos();
            let tag = DbtExitTag::try_from(raw_tag)?;
            dispatches += 1;
            attempted_instructions =
                attempted_instructions.saturating_add(u64::from(context.exit.attempted));

            match tag {
                DbtExitTag::Completed => {
                    if context.exit.attempted == 0
                        || context.exit.attempted as usize > input.slots().len()
                    {
                        return Err(format!(
                            "invalid completed DBT attempted count {} for {} slots",
                            context.exit.attempted,
                            input.slots().len()
                        ));
                    }
                    cpu.commit_instructions(context.exit.attempted);
                }
                DbtExitTag::SlowInstruction => {
                    let prefix = context.exit.attempted.checked_sub(1).ok_or_else(|| {
                        "slow DBT exit did not include its instruction attempt".to_string()
                    })?;
                    cpu.commit_instructions(prefix);
                    if context.exit.instruction_pc != cpu.pc() {
                        return Err(format!(
                            "slow DBT exit PC {:#010x} disagrees with canonical PC {:#010x}",
                            context.exit.instruction_pc,
                            cpu.pc()
                        ));
                    }
                    let decoded =
                        self.decoded.get(prefix as usize).copied().ok_or_else(|| {
                            "slow DBT exit points outside decoded block".to_string()
                        })?;
                    match decoded {
                        Rv32ResolvedInstruction::Valid {
                            word,
                            instruction: DecodedInstruction::Ebreak,
                        } if word == context.exit.instruction_word => {
                            return Ok(DirectDbtComputeObservation {
                                checksum: cpu.register(self.result_register as usize),
                                dispatches,
                                attempted_instructions,
                                translation_nanos,
                                publication_nanos,
                                execution_nanos,
                                translated_bytes,
                                reserved_bytes: self.scratch.reserved_bytes(),
                            });
                        }
                        _ => {
                            return Err(format!(
                                "compute32 reached unexpected slow DBT instruction {:#010x} at {:#010x}",
                                context.exit.instruction_word, context.exit.instruction_pc
                            ));
                        }
                    }
                }
                DbtExitTag::BudgetExhausted => {
                    return Err("unbounded compute32 DBT block exhausted its budget".to_string());
                }
                DbtExitTag::MemoryAccess => {
                    return Err(
                        "compute32 unexpectedly requested a DBT memory slow path".to_string()
                    );
                }
            }
        }
    }

    pub(crate) const fn iterations(&self) -> u32 {
        self.iterations
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedDbtComputeObservation {
    pub checksum: u32,
    pub dispatches: u64,
    pub attempted_instructions: u64,
    pub translations: u64,
    pub publications: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub translation_nanos: u128,
    pub publication_nanos: u128,
    pub execution_nanos: u128,
    pub translated_bytes: u64,
    pub reserved_bytes: usize,
    pub metadata_bytes: usize,
}

pub struct PreparedCachedDbtCompute32 {
    iterations: u32,
    words: Vec<u32>,
    result_register: u8,
    memory: MachineMemory,
    cache: DirectDbtCodeCache,
    workspace: DbtTranslationWorkspace,
    decoded: Vec<Rv32ResolvedInstruction>,
}

impl PreparedCachedDbtCompute32 {
    pub fn new(iterations: u32) -> Result<Self, String> {
        if iterations == 0 {
            return Err("cached DBT compute32 iterations must be positive".to_string());
        }
        let image = rv32_workload(IsaBenchmarkWorkload::Compute32, iterations)?;
        let code = image
            .words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let memory = MachineMemory::from_sections(code.len(), &code, &[], 0)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            iterations,
            words: image.words,
            result_register: image.result_register,
            memory,
            cache: DirectDbtCodeCache::new(CACHE_SETS, EXECUTABLE_BYTES)
                .map_err(|error| error.to_string())?,
            workspace: DbtTranslationWorkspace::new(EXECUTABLE_BYTES, MAX_BLOCK_INSTRUCTIONS)
                .map_err(|error| error.to_string())?,
            decoded: Vec::with_capacity(MAX_BLOCK_INSTRUCTIONS),
        })
    }

    pub fn execute(&mut self) -> Result<CachedDbtComputeObservation, String> {
        let executable_end = u32::try_from(self.words.len() * 4)
            .map_err(|_| "compute32 executable exceeds RV32 address space".to_string())?;
        let max_dispatches = u64::from(self.iterations)
            .checked_mul(3)
            .and_then(|count| count.checked_add(16))
            .ok_or_else(|| "compute32 cached DBT dispatch limit overflow".to_string())?;
        let initial_stats = self.cache.stats();
        let mut cpu = Rv32imCpu::new(0);
        let mut dispatches = 0_u64;
        let mut attempted_instructions = 0_u64;
        let mut translations = 0_u64;
        let mut publications = 0_u64;
        let mut translation_nanos = 0_u128;
        let mut publication_nanos = 0_u128;
        let mut execution_nanos = 0_u128;
        let mut translated_bytes = 0_u64;

        loop {
            if dispatches >= max_dispatches {
                return Err(format!(
                    "compute32 cached DBT exceeded {max_dispatches} dispatches at PC {:#010x}",
                    cpu.pc()
                ));
            }
            let key = DbtCacheKey::new(cpu.pc(), 0);
            let handle = if let Some(handle) = self.cache.lookup(key) {
                handle
            } else {
                fill_decoded_block(
                    cpu.pc(),
                    executable_end,
                    MAX_BLOCK_INSTRUCTIONS,
                    &self.memory,
                    &mut self.decoded,
                )
                .map_err(|error| error.to_string())?;

                let translation_started = Instant::now();
                let input = DbtBlockInput::new(cpu.pc(), &self.decoded, DbtBlockMode::Fast)?;
                let compiled = self
                    .workspace
                    .lower(&input)
                    .map_err(|error| error.to_string())?;
                translation_nanos += translation_started.elapsed().as_nanos();
                translations += 1;
                translated_bytes = translated_bytes.saturating_add(compiled.code().len() as u64);

                let publication_started = Instant::now();
                let handle = self
                    .cache
                    .publish(key, &compiled)
                    .map_err(|error| error.to_string())?;
                publication_nanos += publication_started.elapsed().as_nanos();
                publications += 1;
                handle
            };
            let instruction_count = self
                .cache
                .instruction_count(handle)
                .ok_or_else(|| "cached DBT handle lost before entry".to_string())?;
            let entry_address = self
                .cache
                .entry_address(handle)
                .ok_or_else(|| "cached DBT entry is not executable".to_string())?;
            let mut context = DbtContext {
                state: cpu.architectural_state_mut(),
                ram_base: self.memory.as_mut_ptr(),
                ram_len: self.memory.len() as u32,
                page_permissions: std::ptr::null(),
                page_count: 0,
                remaining_budget: u32::MAX,
                reservation_valid: 0,
                reservation_address: 0,
                exit: DbtExitRecord::default(),
            };
            let entry: DbtEntry = unsafe { std::mem::transmute(entry_address) };
            let execution_started = Instant::now();
            let raw_tag = unsafe { entry(&mut context) };
            execution_nanos += execution_started.elapsed().as_nanos();
            let tag = DbtExitTag::try_from(raw_tag)?;
            dispatches += 1;
            attempted_instructions =
                attempted_instructions.saturating_add(u64::from(context.exit.attempted));

            match tag {
                DbtExitTag::Completed => {
                    if context.exit.attempted == 0 || context.exit.attempted > instruction_count {
                        return Err(format!(
                            "invalid completed cached DBT attempted count {} for {instruction_count} slots",
                            context.exit.attempted
                        ));
                    }
                    cpu.commit_instructions(context.exit.attempted);
                }
                DbtExitTag::SlowInstruction => {
                    let prefix = context.exit.attempted.checked_sub(1).ok_or_else(|| {
                        "slow cached DBT exit did not include its instruction attempt".to_string()
                    })?;
                    if context.exit.attempted > instruction_count {
                        return Err(format!(
                            "slow cached DBT attempted count {} exceeds {instruction_count} slots",
                            context.exit.attempted
                        ));
                    }
                    cpu.commit_instructions(prefix);
                    if context.exit.instruction_pc != cpu.pc() {
                        return Err(format!(
                            "slow cached DBT exit PC {:#010x} disagrees with canonical PC {:#010x}",
                            context.exit.instruction_pc,
                            cpu.pc()
                        ));
                    }
                    match decode_product_word(context.exit.instruction_word) {
                        Ok(DecodedInstruction::Ebreak) => {
                            let final_stats = self.cache.stats();
                            return Ok(CachedDbtComputeObservation {
                                checksum: cpu.register(self.result_register as usize),
                                dispatches,
                                attempted_instructions,
                                translations,
                                publications,
                                cache_hits: final_stats.hits.saturating_sub(initial_stats.hits),
                                cache_misses: final_stats
                                    .misses
                                    .saturating_sub(initial_stats.misses),
                                translation_nanos,
                                publication_nanos,
                                execution_nanos,
                                translated_bytes,
                                reserved_bytes: self.cache.reserved_bytes(),
                                metadata_bytes: self.cache.metadata_bytes(),
                            });
                        }
                        _ => {
                            return Err(format!(
                                "compute32 reached unexpected slow cached DBT instruction {:#010x} at {:#010x}",
                                context.exit.instruction_word, context.exit.instruction_pc
                            ));
                        }
                    }
                }
                DbtExitTag::BudgetExhausted => {
                    return Err("unbounded compute32 cached DBT block exhausted its budget".into());
                }
                DbtExitTag::MemoryAccess => {
                    return Err(
                        "compute32 unexpectedly requested a cached DBT memory slow path".into(),
                    );
                }
            }
        }
    }

    pub(crate) const fn iterations(&self) -> u32 {
        self.iterations
    }
}

#[cfg(test)]
mod tests {
    use super::{PreparedCachedDbtCompute32, PreparedDirectDbtCompute32};
    use crate::benchmarks::{native_checksum, BenchmarkWorkload};

    #[test]
    fn direct_dbt_executes_the_existing_compute32_program() {
        for iterations in [1, 2, 17, 257] {
            let mut prepared = PreparedDirectDbtCompute32::new(iterations).unwrap();

            let observation = prepared.execute().unwrap();

            assert_eq!(
                observation.checksum,
                native_checksum(BenchmarkWorkload::Compute32, iterations)
            );
            assert!(observation.dispatches >= 3);
            assert!(observation.attempted_instructions > u64::from(iterations));
            assert!(observation.translated_bytes > 0);
            assert_eq!(observation.reserved_bytes, 128 * 1024);
        }
    }

    #[test]
    fn cached_dbt_reuses_resident_compute32_blocks() {
        let mut prepared = PreparedCachedDbtCompute32::new(257).unwrap();

        let cold = prepared.execute().unwrap();
        let warm = prepared.execute().unwrap();

        assert_eq!(
            cold.checksum,
            native_checksum(BenchmarkWorkload::Compute32, 257)
        );
        assert_eq!(warm.checksum, cold.checksum);
        assert!(cold.translations > 0);
        assert_eq!(warm.translations, 0);
        assert_eq!(warm.publications, 0);
        assert!(warm.cache_hits > 0);
        assert_eq!(warm.cache_misses, 0);
        assert_eq!(warm.translated_bytes, 0);
    }
}
