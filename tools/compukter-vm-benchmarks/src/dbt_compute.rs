/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

use super::rv32::rv32_workload;
use super::IsaBenchmarkWorkload;
use compukter_vm::benchmark_support::{self, DbtComputeImage};

pub use benchmark_support::{CachedDbtComputeObservation, DirectDbtComputeObservation};

pub struct PreparedDirectDbtCompute32 {
    iterations: u32,
    inner: benchmark_support::PreparedDirectDbtCompute32,
}

impl PreparedDirectDbtCompute32 {
    pub fn new(iterations: u32) -> Result<Self, String> {
        let image = rv32_workload(IsaBenchmarkWorkload::Compute32, iterations)?;
        Ok(Self {
            iterations,
            inner: benchmark_support::PreparedDirectDbtCompute32::new(DbtComputeImage {
                words: image.words,
                result_register: image.result_register,
                iterations,
            })?,
        })
    }

    pub fn execute(&mut self) -> Result<DirectDbtComputeObservation, String> {
        self.inner.execute()
    }

    pub(crate) const fn iterations(&self) -> u32 {
        self.iterations
    }
}

pub struct PreparedCachedDbtCompute32 {
    iterations: u32,
    inner: benchmark_support::PreparedCachedDbtCompute32,
}

impl PreparedCachedDbtCompute32 {
    pub fn new(iterations: u32) -> Result<Self, String> {
        let image = rv32_workload(IsaBenchmarkWorkload::Compute32, iterations)?;
        Ok(Self {
            iterations,
            inner: benchmark_support::PreparedCachedDbtCompute32::new(DbtComputeImage {
                words: image.words,
                result_register: image.result_register,
                iterations,
            })?,
        })
    }

    pub fn execute(&mut self) -> Result<CachedDbtComputeObservation, String> {
        self.inner.execute()
    }

    pub(crate) const fn iterations(&self) -> u32 {
        self.iterations
    }
}

#[cfg(test)]
mod tests {
    use super::{PreparedCachedDbtCompute32, PreparedDirectDbtCompute32};
    use crate::{native_checksum, BenchmarkWorkload};

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
            assert_eq!(observation.reserved_bytes, 16 * 1024);
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
