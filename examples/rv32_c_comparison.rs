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

use compukter_vm::benchmarks::{
    benchmark_normalize_nanos, c_comparison_next_batch, c_comparison_qemu_target_nanos,
    c_comparison_timeout_nanos, parse_c_comparison_result, product_percentile,
};
#[cfg(feature = "wasmtime-comparison")]
use compukter_vm::benchmarks::{
    benchmark_rotating_order, compile_equivalent_calls, optional_phase_rate,
    COMPILATION_PHASE_REPORT_HEADER,
};
use compukter_vm::rv32_machine::{
    Rv32DbtCodeAlignment, Rv32ExecutionBackendConfig, Rv32Machine, Rv32MachineConfig,
    Rv32MachineOutcome, DEFAULT_DBT_CODE_ALIGNMENT,
};
#[cfg(feature = "dbt-execution-profile")]
use compukter_vm::rv32_machine::{Rv32DbtExecutionProfile, Rv32DbtProfileEdgeKind};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(feature = "wasmtime-comparison")]
use wasmtime::{Config, Engine, Instance, Module, OptLevel, Store, TypedFunc};

const ITERATIONS: u32 = 1000;
const SEED: u32 = 0x1234_5678;
const EXPECTED_CHECKSUM: u32 = 0xee05_3d58;
const MINIMUM_SAMPLES: usize = 21;
const STARTUP_SAMPLES: usize = 7;
const SAMPLE_TARGET_NANOS: u128 = 250_000_000;
const PRODUCT_RAM_BYTES: usize = 16 * 1024;
const PRODUCT_CACHE_SETS: usize = 64;
const PRODUCT_BLOCK_CACHE_SETS: usize = 32;
const PRODUCT_BLOCK_MAX_INSTRUCTIONS: usize = 8;
const PRODUCT_DBT_CACHE_SETS: usize = 32;
const PRODUCT_DBT_MAX_INSTRUCTIONS: usize = 8;
const PRODUCT_DBT_SCRATCH_BYTES: usize = 8 * 1024;

struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy)]
enum CandidateKind {
    Native,
    Qemu,
    Wasmtime,
    Cached,
    Predecoded,
    BlockCached,
    DirectDbt,
    CachedDbt {
        sets: usize,
        cache_bytes: usize,
        max_instructions: usize,
        code_alignment: Rv32DbtCodeAlignment,
    },
}

#[derive(Clone, Copy)]
struct Candidate {
    name: &'static str,
    mode: &'static str,
    artifact_stem: Option<&'static str>,
    kind: CandidateKind,
}

impl Candidate {
    fn product_config(self) -> Option<Rv32ExecutionBackendConfig> {
        match self.kind {
            CandidateKind::Native | CandidateKind::Qemu | CandidateKind::Wasmtime => None,
            CandidateKind::Cached => Some(Rv32ExecutionBackendConfig::Cached {
                sets: PRODUCT_CACHE_SETS,
            }),
            CandidateKind::Predecoded => Some(Rv32ExecutionBackendConfig::Predecoded),
            CandidateKind::BlockCached => Some(Rv32ExecutionBackendConfig::BlockCached {
                sets: PRODUCT_BLOCK_CACHE_SETS,
                max_instructions: PRODUCT_BLOCK_MAX_INSTRUCTIONS,
            }),
            CandidateKind::DirectDbt => Some(Rv32ExecutionBackendConfig::DirectDbt {
                max_instructions: PRODUCT_DBT_MAX_INSTRUCTIONS,
                scratch_bytes: PRODUCT_DBT_SCRATCH_BYTES,
            }),
            CandidateKind::CachedDbt {
                sets,
                cache_bytes,
                max_instructions,
                code_alignment,
            } => Some(Rv32ExecutionBackendConfig::CachedDbt {
                sets,
                max_instructions,
                scratch_bytes: PRODUCT_DBT_SCRATCH_BYTES,
                cache_bytes,
                code_alignment,
            }),
        }
    }

    const fn dbt_alignment(self) -> Option<Rv32DbtCodeAlignment> {
        match self.kind {
            CandidateKind::CachedDbt { code_alignment, .. } => Some(code_alignment),
            _ => None,
        }
    }
}

const fn cached_dbt_candidate(
    name: &'static str,
    artifact_stem: &'static str,
    sets: usize,
    cache_bytes: usize,
) -> Candidate {
    cached_dbt_candidate_with_block_size(
        name,
        artifact_stem,
        sets,
        cache_bytes,
        PRODUCT_DBT_MAX_INSTRUCTIONS,
        DEFAULT_DBT_CODE_ALIGNMENT,
    )
}

const fn cached_dbt_candidate_with_block_size(
    name: &'static str,
    artifact_stem: &'static str,
    sets: usize,
    cache_bytes: usize,
    max_instructions: usize,
    code_alignment: Rv32DbtCodeAlignment,
) -> Candidate {
    Candidate {
        name,
        mode: "product-machine-cached-dbt",
        artifact_stem: Some(artifact_stem),
        kind: CandidateKind::CachedDbt {
            sets,
            cache_bytes,
            max_instructions,
            code_alignment,
        },
    }
}

const fn cached_dbt_alignment_candidate(
    name: &'static str,
    artifact_stem: &'static str,
    alignment: usize,
) -> Candidate {
    cached_dbt_candidate_with_block_size(
        name,
        artifact_stem,
        512,
        128 * 1024,
        16,
        Rv32DbtCodeAlignment::BlockBase(alignment),
    )
}

const COMMON_CANDIDATES: [Candidate; 7] = [
    Candidate {
        name: "native-clang",
        mode: "clang-O3-native-lto",
        artifact_stem: None,
        kind: CandidateKind::Native,
    },
    Candidate {
        name: "qemu-rv32-tcg",
        mode: "virt-system-tcg",
        artifact_stem: Some("qemu"),
        kind: CandidateKind::Qemu,
    },
    Candidate {
        name: "wasmtime-aot",
        mode: "wasmtime compile -O opt-level=2",
        artifact_stem: Some("wasmtime-aot"),
        kind: CandidateKind::Wasmtime,
    },
    Candidate {
        name: "rv32-cached",
        mode: "product-machine-cached",
        artifact_stem: Some("cached"),
        kind: CandidateKind::Cached,
    },
    Candidate {
        name: "rv32-predecoded",
        mode: "product-machine-predecoded",
        artifact_stem: Some("predecoded"),
        kind: CandidateKind::Predecoded,
    },
    Candidate {
        name: "rv32-block-cached",
        mode: "product-machine-block-cached",
        artifact_stem: Some("block-cached"),
        kind: CandidateKind::BlockCached,
    },
    Candidate {
        name: "rv32-direct-dbt",
        mode: "product-machine-direct-dbt",
        artifact_stem: Some("direct-dbt"),
        kind: CandidateKind::DirectDbt,
    },
];

const DBT_MATRIX: [Candidate; 19] = [
    cached_dbt_candidate(
        "rv32-cached-dbt-16k",
        "cached-dbt-16k",
        PRODUCT_DBT_CACHE_SETS,
        16 * 1024,
    ),
    cached_dbt_candidate(
        "rv32-cached-dbt-32k",
        "cached-dbt-32k",
        PRODUCT_DBT_CACHE_SETS,
        32 * 1024,
    ),
    cached_dbt_candidate(
        "rv32-cached-dbt-64k",
        "cached-dbt-64k",
        PRODUCT_DBT_CACHE_SETS,
        64 * 1024,
    ),
    cached_dbt_candidate(
        "rv32-cached-dbt-128k",
        "cached-dbt-128k",
        PRODUCT_DBT_CACHE_SETS,
        128 * 1024,
    ),
    cached_dbt_candidate(
        "rv32-cached-dbt-256k",
        "cached-dbt-256k",
        PRODUCT_DBT_CACHE_SETS,
        256 * 1024,
    ),
    cached_dbt_candidate(
        "rv32-cached-dbt-512k",
        "cached-dbt-512k",
        PRODUCT_DBT_CACHE_SETS,
        512 * 1024,
    ),
    cached_dbt_candidate(
        "rv32-cached-dbt-16-sets",
        "cached-dbt-16-sets",
        16,
        128 * 1024,
    ),
    cached_dbt_candidate(
        "rv32-cached-dbt-64-sets",
        "cached-dbt-64-sets",
        64,
        128 * 1024,
    ),
    cached_dbt_candidate(
        "rv32-cached-dbt-128-sets",
        "cached-dbt-128-sets",
        128,
        128 * 1024,
    ),
    cached_dbt_candidate(
        "rv32-cached-dbt-256-sets",
        "cached-dbt-256-sets",
        256,
        128 * 1024,
    ),
    cached_dbt_candidate(
        "rv32-cached-dbt-512-sets",
        "cached-dbt-512-sets",
        512,
        128 * 1024,
    ),
    cached_dbt_candidate_with_block_size(
        "rv32-cached-dbt-block-16",
        "cached-dbt-block-16",
        512,
        128 * 1024,
        16,
        DEFAULT_DBT_CODE_ALIGNMENT,
    ),
    cached_dbt_candidate_with_block_size(
        "rv32-cached-dbt-block-32",
        "cached-dbt-block-32",
        512,
        128 * 1024,
        32,
        DEFAULT_DBT_CODE_ALIGNMENT,
    ),
    cached_dbt_candidate_with_block_size(
        "rv32-cached-dbt-block-64",
        "cached-dbt-block-64",
        512,
        128 * 1024,
        64,
        DEFAULT_DBT_CODE_ALIGNMENT,
    ),
    cached_dbt_alignment_candidate(
        "rv32-cached-dbt-align-base-16",
        "cached-dbt-align-base-16",
        16,
    ),
    cached_dbt_alignment_candidate(
        "rv32-cached-dbt-align-base-32",
        "cached-dbt-align-base-32",
        32,
    ),
    cached_dbt_alignment_candidate(
        "rv32-cached-dbt-align-base-64",
        "cached-dbt-align-base-64",
        64,
    ),
    cached_dbt_alignment_candidate(
        "rv32-cached-dbt-align-base-128",
        "cached-dbt-align-base-128",
        128,
    ),
    cached_dbt_candidate_with_block_size(
        "rv32-cached-dbt-align-chain-32",
        "cached-dbt-align-chain-32",
        512,
        128 * 1024,
        16,
        Rv32DbtCodeAlignment::ChainEntry(32),
    ),
];

struct ProcessObservation {
    elapsed_nanos: u128,
    checksum: u32,
}

#[derive(Clone, Copy)]
enum ProcessOutputFormat {
    ChecksumRecord,
    WasmtimeI32,
}

#[derive(Default, Clone, Copy)]
struct ProductDetails {
    retired_instructions: u64,
    lookup_unit: Option<&'static str>,
    cache_hits: Option<u64>,
    cache_misses: Option<u64>,
    cache_evictions: Option<u64>,
    blocks_built: Option<u64>,
    decoded_slots_built: Option<u64>,
    dbt_translations: Option<u64>,
    dbt_publications: Option<u64>,
    dbt_metadata_evictions: Option<u64>,
    dbt_overlap_invalidations: Option<u64>,
    dbt_native_dispatches: Option<u64>,
    dbt_chain_transitions: Option<u64>,
    dbt_budget_overshoot: Option<u64>,
    dbt_max_budget_overshoot: Option<u64>,
    dbt_links_established: Option<u64>,
    dbt_links_reset: Option<u64>,
    dbt_typed_slow_exits: Option<u64>,
    dbt_lowered_load_sites: Option<u64>,
    dbt_lowered_store_sites: Option<u64>,
    dbt_local_self_backedge_sites: Option<u64>,
    dbt_emitted_bytes: Option<u64>,
    dbt_alignment_padding_bytes: Option<u64>,
    dbt_live_code_bytes: Option<u64>,
    dbt_code_prefix_bytes: Option<u64>,
    dbt_reserved_bytes: Option<u64>,
    translation_bytes: usize,
    executable_bytes: usize,
    steady_allocations: u64,
    steady_allocated_bytes: u64,
}

struct CandidateMeasurements {
    candidate: Candidate,
    batch: u64,
    samples: Vec<u128>,
    details: ProductDetails,
}

#[cfg(feature = "wasmtime-comparison")]
struct CompilationPhaseMeasurement {
    system: &'static str,
    phase: &'static str,
    nanos: Vec<u128>,
    input_bytes: Option<u64>,
    translated_blocks: Option<u64>,
    guest_instructions: Option<u64>,
    output_bytes: Option<u64>,
    warm_nanos: Option<u128>,
    amortized: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("RV32 C comparison failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if arguments
        .first()
        .is_some_and(|argument| argument == "profile")
    {
        #[cfg(feature = "dbt-execution-profile")]
        return run_execution_profile(&arguments[1..]);
        #[cfg(not(feature = "dbt-execution-profile"))]
        return Err("profile mode requires the dbt-execution-profile feature".to_string());
    }
    if arguments.len() != 2 {
        return Err("usage: rv32_c_comparison BUILD_DIR WARM_SAMPLES".to_string());
    }
    let build_dir = PathBuf::from(&arguments[0]);
    let samples = arguments[1]
        .to_str()
        .ok_or_else(|| "warm sample count is not UTF-8".to_string())?
        .parse::<usize>()
        .map_err(|error| format!("invalid warm sample count: {error}"))?;
    if samples < MINIMUM_SAMPLES {
        return Err(format!(
            "warm sample count must be at least {MINIMUM_SAMPLES}"
        ));
    }
    let candidates = COMMON_CANDIDATES
        .iter()
        .chain(&DBT_MATRIX)
        .copied()
        .collect::<Vec<_>>();

    let source_root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benchmarks/rv32-c-comparison");
    let native = build_dir.join("native-kernel");
    let manifest = read_manifest(&build_dir.join("manifest.tsv"))?;
    let qemu = env::var_os("RV32_C_QEMU").unwrap_or_else(|| "qemu-system-riscv32".into());
    let wasmtime = env::var_os("RV32_C_WASMTIME").unwrap_or_else(|| "wasmtime".into());
    let clang = env::var_os("RV32_C_CLANG").unwrap_or_else(|| "clang".into());
    let linker = env::var_os("RV32_C_LLD").unwrap_or_else(|| "ld.lld".into());
    let wasm = build_dir.join("module.wasm");
    let cwasm = build_dir.join("module.cwasm");
    compile_wasmtime(&wasmtime, &wasm, &cwasm)?;

    let empty_qemu_elf = link_platform(&linker, &source_root, &build_dir, "qemu", 0)?;
    let mut startup_durations = Vec::with_capacity(STARTUP_SAMPLES);
    for _ in 0..STARTUP_SAMPLES {
        let observation = run_qemu(&qemu, &empty_qemu_elf, Duration::from_secs(10), 0)?;
        startup_durations.push(observation.elapsed_nanos);
    }
    startup_durations.sort_unstable();
    let startup_median = product_percentile(&startup_durations, 50);
    let qemu_target = c_comparison_qemu_target_nanos(startup_median)?;

    let native_batch = calibrate_process(SAMPLE_TARGET_NANOS, |batch| {
        run_native(&native, batch, Duration::from_secs(30))
    })?;
    let qemu_batch = calibrate_process(qemu_target, |batch| {
        let elf = link_platform(&linker, &source_root, &build_dir, "qemu", batch)?;
        run_qemu(&qemu, &elf, Duration::from_secs(60), EXPECTED_CHECKSUM)
    })?;
    let wasmtime_batch = calibrate_process(SAMPLE_TARGET_NANOS, |batch| {
        run_wasmtime(&wasmtime, &cwasm, batch, Duration::from_secs(30))
    })?;
    let mut batches = vec![0_u64; candidates.len()];
    batches[0] = native_batch;
    batches[1] = qemu_batch;
    batches[2] = wasmtime_batch;
    for (index, candidate) in candidates.iter().copied().enumerate().skip(3) {
        batches[index] = calibrate_product(
            &linker,
            &source_root,
            &build_dir,
            candidate.product_config().unwrap(),
        )?;
    }
    let candidate_elfs = candidates
        .iter()
        .copied()
        .zip(batches.iter().copied())
        .map(|(candidate, batch)| match candidate.kind {
            CandidateKind::Native => Ok(None),
            CandidateKind::Qemu => {
                link_platform(&linker, &source_root, &build_dir, "qemu", batch).map(Some)
            }
            CandidateKind::Wasmtime => Ok(Some(cwasm.clone())),
            _ => link_platform(&linker, &source_root, &build_dir, "product", batch).map(Some),
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut measurements = candidates
        .iter()
        .copied()
        .zip(batches.iter().copied())
        .map(|(candidate, batch)| CandidateMeasurements {
            candidate,
            batch,
            samples: Vec::with_capacity(samples),
            details: ProductDetails::default(),
        })
        .collect::<Vec<_>>();

    let qemu_timeout = duration_from_nanos(c_comparison_timeout_nanos(qemu_target))?;
    for sample in 0..samples {
        for offset in 0..candidates.len() {
            let candidate_index = (sample + offset) % candidates.len();
            let measurement = &mut measurements[candidate_index];
            let (elapsed, details) = match measurement.candidate.kind {
                CandidateKind::Native => (
                    run_native(&native, measurement.batch, Duration::from_secs(30))?.elapsed_nanos,
                    ProductDetails::default(),
                ),
                CandidateKind::Qemu => (
                    run_qemu(
                        &qemu,
                        candidate_elfs[candidate_index].as_ref().unwrap(),
                        qemu_timeout,
                        EXPECTED_CHECKSUM,
                    )?
                    .elapsed_nanos,
                    ProductDetails::default(),
                ),
                CandidateKind::Wasmtime => (
                    run_wasmtime(
                        &wasmtime,
                        candidate_elfs[candidate_index].as_ref().unwrap(),
                        measurement.batch,
                        Duration::from_secs(30),
                    )?
                    .elapsed_nanos,
                    ProductDetails::default(),
                ),
                _ => run_product(
                    candidate_elfs[candidate_index].as_ref().unwrap(),
                    measurement.batch,
                    measurement.candidate.product_config().unwrap(),
                )?,
            };
            measurement.samples.push(elapsed);
            measurement.details = details;
        }
    }

    println!("RV32 optimized C comparison");
    println!("iterations\t{ITERATIONS}");
    println!("seed\t0x{SEED:08x}");
    println!("warm_samples\t{samples}");
    println!("dbt_matrix\tcache+sets+alignment");
    println!("qemu_startup_samples\t{STARTUP_SAMPLES}");
    println!("qemu_startup_median_ns\t{startup_median}");
    println!("qemu_target_ns\t{qemu_target}");
    println!("qemu_mode\t-M virt -bios none -accel tcg -nographic -monitor none");
    println!("qemu-version\t{}", version_line(&qemu)?);
    println!("wasmtime-version\t{}", version_line(&wasmtime)?);
    println!("clang-version\t{}", version_line(&clang)?);
    println!("lld-version\t{}", version_line(&linker)?);
    for key in [
        "native-flags",
        "rv32-flags",
        "kernel-object-sha256",
        "native-sha256",
        "native-text-bytes",
        "product-text-bytes",
        "qemu-text-bytes",
        "wasm-flags",
        "wasm-sha256",
        "wasm-bytes",
    ] {
        println!("{key}\t{}", manifest_value(&manifest, key)?);
    }
    for (candidate, elf) in candidates.iter().copied().zip(&candidate_elfs) {
        if let Some(stem) = candidate.artifact_stem {
            println!(
                "{stem}-calibrated-sha256\t{}",
                sha256_file(elf.as_ref().unwrap())?
            );
        }
    }
    println!(
        "candidate\tmode\titerations\tseed\tbatch\tchecksum\ttotal_median_ns\ttotal_p95_ns\tns_per_kernel\tkernels_per_second\tvs_native\tvs_qemu\tvs_wasmtime\ttext_bytes\tqemu_startup_median_ns\tretired_instructions\tlookup_unit\tcache_hits\tcache_misses\tcache_evictions\tblocks_built\tdecoded_slots_built\ttranslation_bytes\tdbt_translations\tdbt_publications\tdbt_native_dispatches\tdbt_chain_transitions\tdbt_links_established\tdbt_links_reset\tdbt_typed_slow_exits\tdbt_metadata_evictions\tdbt_overlap_invalidations\tdbt_lowered_load_sites\tdbt_lowered_store_sites\tdbt_local_self_backedge_sites\tdbt_emitted_bytes\tdbt_reserved_bytes\tsteady_allocations\tsteady_allocated_bytes\tdbt_budget_overshoot\tdbt_max_budget_overshoot\tdbt_code_alignment\tdbt_alignment_anchor\tdbt_alignment_padding_bytes\tdbt_live_code_bytes\tdbt_code_prefix_bytes"
    );

    let normalized = measurements
        .iter_mut()
        .map(|measurement| {
            measurement.samples.sort_unstable();
            benchmark_normalize_nanos(
                product_percentile(&measurement.samples, 50),
                measurement.batch,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let native_nanos = normalized[0];
    let qemu_nanos = normalized[1];
    let wasmtime_nanos = normalized[2];

    for (index, measurement) in measurements.iter().enumerate() {
        let median = product_percentile(&measurement.samples, 50);
        let p95 = product_percentile(&measurement.samples, 95);
        let per_kernel = normalized[index];
        let text_bytes = match measurement.candidate.kind {
            CandidateKind::Native => manifest_value(&manifest, "native-text-bytes")?.to_string(),
            CandidateKind::Qemu => manifest_value(&manifest, "qemu-text-bytes")?.to_string(),
            CandidateKind::Wasmtime => fs::metadata(&cwasm)
                .map_err(|error| format!("failed to inspect {}: {error}", cwasm.display()))?
                .len()
                .to_string(),
            _ => measurement.details.executable_bytes.to_string(),
        };
        let mode = measurement.candidate.mode;
        let product_candidate = measurement.candidate.product_config().is_some();
        println!(
            "{}\t{}\t{}\t0x{:08x}\t{}\t{:08x}\t{}\t{}\t{:.3}\t{:.3}\t{:.6}\t{:.6}\t{:.6}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            measurement.candidate.name,
            mode,
            ITERATIONS,
            SEED,
            measurement.batch,
            EXPECTED_CHECKSUM,
            median,
            p95,
            per_kernel,
            1_000_000_000.0 / per_kernel,
            per_kernel / native_nanos,
            per_kernel / qemu_nanos,
            per_kernel / wasmtime_nanos,
            text_bytes,
            if matches!(measurement.candidate.kind, CandidateKind::Qemu) {
                startup_median.to_string()
            } else {
                "-".to_string()
            },
            option_u64(product_candidate.then_some(measurement.details.retired_instructions)),
            measurement.details.lookup_unit.unwrap_or("-"),
            option_u64(measurement.details.cache_hits),
            option_u64(measurement.details.cache_misses),
            option_u64(measurement.details.cache_evictions),
            option_u64(measurement.details.blocks_built),
            option_u64(measurement.details.decoded_slots_built),
            if product_candidate {
                measurement.details.translation_bytes.to_string()
            } else {
                "-".to_string()
            },
            option_u64(measurement.details.dbt_translations),
            option_u64(measurement.details.dbt_publications),
            option_u64(measurement.details.dbt_native_dispatches),
            option_u64(measurement.details.dbt_chain_transitions),
            option_u64(measurement.details.dbt_links_established),
            option_u64(measurement.details.dbt_links_reset),
            option_u64(measurement.details.dbt_typed_slow_exits),
            option_u64(measurement.details.dbt_metadata_evictions),
            option_u64(measurement.details.dbt_overlap_invalidations),
            option_u64(measurement.details.dbt_lowered_load_sites),
            option_u64(measurement.details.dbt_lowered_store_sites),
            option_u64(measurement.details.dbt_local_self_backedge_sites),
            option_u64(measurement.details.dbt_emitted_bytes),
            option_u64(measurement.details.dbt_reserved_bytes),
            option_u64(product_candidate.then_some(measurement.details.steady_allocations)),
            option_u64(product_candidate.then_some(measurement.details.steady_allocated_bytes)),
            option_u64(measurement.details.dbt_budget_overshoot),
            option_u64(measurement.details.dbt_max_budget_overshoot),
            measurement
                .candidate
                .dbt_alignment()
                .map_or_else(|| "-".to_string(), |value| value.bytes().to_string()),
            measurement.candidate.dbt_alignment().map_or("-", |value| match value {
                Rv32DbtCodeAlignment::BlockBase(_) => "block-base",
                Rv32DbtCodeAlignment::ChainEntry(_) => "chain-entry",
            }),
            option_u64(measurement.details.dbt_alignment_padding_bytes),
            option_u64(measurement.details.dbt_live_code_bytes),
            option_u64(measurement.details.dbt_code_prefix_bytes),
        );
    }
    #[cfg(feature = "wasmtime-comparison")]
    {
        let dbt_index = candidates
            .iter()
            .position(|candidate| candidate.name == "rv32-cached-dbt-block-16")
            .ok_or_else(|| "missing block-16 DBT comparison candidate".to_string())?;
        print_compilation_report(
            &wasmtime,
            &linker,
            &source_root,
            &build_dir,
            &wasm,
            samples,
            measurements[2]
                .samples
                .iter()
                .map(|nanos| nanos / u128::from(measurements[2].batch))
                .collect(),
            measurements[dbt_index]
                .samples
                .iter()
                .map(|nanos| nanos / u128::from(measurements[dbt_index].batch))
                .collect(),
        )?;
    }
    Ok(())
}

#[cfg(feature = "dbt-execution-profile")]
fn run_execution_profile(arguments: &[OsString]) -> Result<(), String> {
    if arguments.len() != 3 {
        return Err(
            "usage: rv32_c_comparison profile BUILD_DIR ITERATIONS PROFILE_CAPACITY".to_string(),
        );
    }
    let build_dir = PathBuf::from(&arguments[0]);
    let iterations = parse_profile_argument::<u32>(&arguments[1], "iterations")?;
    if iterations != ITERATIONS {
        return Err(format!(
            "profile iterations must match the shared artifact oracle ({ITERATIONS})"
        ));
    }
    let capacity = parse_profile_argument::<usize>(&arguments[2], "profile capacity")?;
    let elf_path = build_dir.join("product.elf");
    let elf = fs::read(&elf_path)
        .map_err(|error| format!("failed to read {}: {error}", elf_path.display()))?;
    let mut machine = Rv32Machine::from_elf(
        &elf,
        Rv32MachineConfig {
            ram_size: PRODUCT_RAM_BYTES,
            debug_limit: 0,
            execution: Rv32ExecutionBackendConfig::CachedDbt {
                sets: 512,
                max_instructions: 16,
                scratch_bytes: PRODUCT_DBT_SCRATCH_BYTES,
                cache_bytes: 128 * 1024,
                code_alignment: Rv32DbtCodeAlignment::BlockBase(32),
            },
        },
    )
    .map_err(|error| error.to_string())?;
    machine
        .enable_dbt_execution_profile(capacity)
        .map_err(|error| error.to_string())?;

    let started = Instant::now();
    let outcome = machine.run(20_100_000).map_err(|error| error.to_string())?;
    let instrumented_ns = started.elapsed().as_nanos();
    let checksum = match outcome {
        Rv32MachineOutcome::Halted { exit_code, .. } => exit_code as u32,
        other => return Err(format!("profiled Cached DBT did not halt: {other:?}")),
    };
    validate_profile_checksum(checksum)?;
    let profile = machine
        .dbt_execution_profile()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "profiled Cached DBT returned no execution profile".to_string())?;
    print_execution_profile(iterations, checksum, instrumented_ns, &profile)
}

#[cfg(feature = "dbt-execution-profile")]
fn parse_profile_argument<T>(argument: &OsStr, name: &str) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    argument
        .to_str()
        .ok_or_else(|| format!("{name} is not UTF-8"))?
        .parse::<T>()
        .map_err(|error| format!("invalid {name}: {error}"))
}

#[cfg(feature = "dbt-execution-profile")]
fn validate_profile_checksum(checksum: u32) -> Result<(), String> {
    if checksum != EXPECTED_CHECKSUM {
        return Err(format!(
            "profile checksum mismatch: expected {EXPECTED_CHECKSUM:08x}, actual {checksum:08x}"
        ));
    }
    Ok(())
}

#[cfg(feature = "dbt-execution-profile")]
fn print_execution_profile(
    iterations: u32,
    checksum: u32,
    instrumented_ns: u128,
    profile: &Rv32DbtExecutionProfile,
) -> Result<(), String> {
    if profile.counter_overflowed {
        return Err("exact execution-profile counter overflowed".to_string());
    }
    if profile.blocks.is_empty() || profile.static_edges.is_empty() {
        return Err("exact execution profile contains no blocks or static edges".to_string());
    }
    let total_blocks = profile
        .blocks
        .iter()
        .try_fold(0_u128, |total, block| {
            total.checked_add(u128::from(block.executions))
        })
        .ok_or_else(|| "block execution total overflowed".to_string())?;
    let total_edges = profile
        .static_edges
        .iter()
        .try_fold(0_u128, |total, edge| {
            total.checked_add(u128::from(edge.executions))
        })
        .ok_or_else(|| "static-edge execution total overflowed".to_string())?;
    if total_blocks == 0 || total_edges == 0 {
        return Err("exact execution profile contains only zero counters".to_string());
    }

    println!("profile_summary\titerations\tchecksum\tinstrumented_ns\tcapacity\tused_records\tretained_bytes\tcounter_overflowed\tunique_blocks\tunique_static_edges");
    println!(
        "profile_summary\t{iterations}\t{checksum:08x}\t{instrumented_ns}\t{}\t{}\t{}\t{}\t{}\t{}",
        profile.capacity,
        profile.used_records,
        profile.retained_bytes,
        profile.counter_overflowed,
        profile.blocks.len(),
        profile.static_edges.len(),
    );

    println!("hot_blocks\trank\tpc\texecutions\tshare\tcumulative_share");
    let mut cumulative = 0_u128;
    for (index, block) in profile.blocks.iter().enumerate() {
        cumulative += u128::from(block.executions);
        println!(
            "hot_blocks\t{}\t0x{:08x}\t{}\t{:.9}\t{:.9}",
            index + 1,
            block.pc,
            block.executions,
            block.executions as f64 / total_blocks as f64,
            cumulative as f64 / total_blocks as f64,
        );
    }

    println!(
        "hot_static_edges\trank\tsource_pc\ttarget_pc\tkind\texecutions\tshare\tcumulative_share"
    );
    cumulative = 0;
    for (index, edge) in profile.static_edges.iter().enumerate() {
        cumulative += u128::from(edge.executions);
        let kind = match edge.kind {
            Rv32DbtProfileEdgeKind::Taken => "taken",
            Rv32DbtProfileEdgeKind::Fallthrough => "fallthrough",
            Rv32DbtProfileEdgeKind::Jump => "jump",
        };
        println!(
            "hot_static_edges\t{}\t0x{:08x}\t0x{:08x}\t{}\t{}\t{:.9}\t{:.9}",
            index + 1,
            edge.source_pc,
            edge.target_pc,
            kind,
            edge.executions,
            edge.executions as f64 / total_edges as f64,
            cumulative as f64 / total_edges as f64,
        );
    }

    println!("coverage\tpercent\tblocks_required");
    for percent in [50_u128, 90, 95, 99] {
        let mut covered = 0_u128;
        let required = profile
            .blocks
            .iter()
            .position(|block| {
                covered += u128::from(block.executions);
                covered.saturating_mul(100) >= total_blocks.saturating_mul(percent)
            })
            .map(|index| index + 1)
            .unwrap_or(profile.blocks.len());
        println!("coverage\t{percent}\t{required}");
    }

    let exits = profile.dynamic_exits;
    println!("dynamic_exits\tjalr\tbudget\tslow_instruction\tmemory_access\ttrap_or_terminal");
    println!(
        "dynamic_exits\t{}\t{}\t{}\t{}\t{}",
        exits.jalr,
        exits.budget,
        exits.slow_instruction,
        exits.memory_access,
        exits.trap_or_terminal,
    );
    Ok(())
}

fn calibrate_process<F>(target_nanos: u128, mut execute: F) -> Result<u64, String>
where
    F: FnMut(u64) -> Result<ProcessObservation, String>,
{
    let mut batch = 1;
    loop {
        let observation = execute(batch)?;
        if observation.checksum != EXPECTED_CHECKSUM {
            return Err(format!(
                "calibration checksum mismatch: expected {EXPECTED_CHECKSUM:08x}, actual {:08x}",
                observation.checksum
            ));
        }
        match c_comparison_next_batch(batch, observation.elapsed_nanos, target_nanos)? {
            Some(next) => batch = next,
            None => return Ok(batch),
        }
    }
}

#[cfg(feature = "wasmtime-comparison")]
#[allow(clippy::too_many_arguments)]
fn print_compilation_report(
    wasmtime_cli: &OsStr,
    linker: &OsStr,
    source_root: &Path,
    build_dir: &Path,
    wasm: &Path,
    samples: usize,
    wasmtime_process_warm_nanos: Vec<u128>,
    dbt_warm_nanos: Vec<u128>,
) -> Result<(), String> {
    let cli_version = version_line(wasmtime_cli)?;
    if cli_version.split_whitespace().nth(1) != Some("47.0.3") {
        return Err(format!(
            "embedded Wasmtime 47.0.3 does not match CLI version: {cli_version}"
        ));
    }
    let wasm_bytes =
        fs::read(wasm).map_err(|error| format!("failed to read {}: {error}", wasm.display()))?;
    let mut config = Config::new();
    config.cranelift_opt_level(OptLevel::Speed);
    let engine = Engine::new(&config).map_err(|error| error.to_string())?;

    let module = Module::new(&engine, &wasm_bytes).map_err(|error| error.to_string())?;
    let mut compile_nanos = Vec::with_capacity(samples);
    let mut instantiate_nanos = Vec::with_capacity(samples);
    let mut first_call_nanos = Vec::with_capacity(samples);
    const WARM_CALL_BATCH: u32 = 1024;
    let (mut warm_store, warm_function) = embedded_instance(&engine, &module)?;
    let mut warm_call_nanos = Vec::with_capacity(samples);
    let mut cli_nanos = Vec::with_capacity(samples);
    let mut cli_output_bytes = Vec::with_capacity(samples);
    let product_elf = link_platform(linker, source_root, build_dir, "product", 1)?;
    let product_bytes = fs::read(&product_elf)
        .map_err(|error| format!("failed to read {}: {error}", product_elf.display()))?;
    let dbt_config = Rv32ExecutionBackendConfig::CachedDbt {
        sets: 512,
        max_instructions: 16,
        scratch_bytes: PRODUCT_DBT_SCRATCH_BYTES,
        cache_bytes: 128 * 1024,
        code_alignment: compukter_vm::rv32_machine::DEFAULT_DBT_CODE_ALIGNMENT,
    };
    let mut construct_nanos = Vec::with_capacity(samples);
    let mut first_completion_nanos = Vec::with_capacity(samples);
    let mut lift_nanos = Vec::with_capacity(samples);
    let mut lower_nanos = Vec::with_capacity(samples);
    let mut publish_nanos = Vec::with_capacity(samples);
    let mut last_stats = None;
    for sample in 0..samples {
        for candidate in benchmark_rotating_order::<8>(0, sample) {
            match candidate {
                0 => {
                    let started = Instant::now();
                    let compiled =
                        Module::new(&engine, &wasm_bytes).map_err(|error| error.to_string())?;
                    compile_nanos.push(started.elapsed().as_nanos());
                    std::hint::black_box(compiled);
                }
                1 => {
                    let started = Instant::now();
                    let mut store = Store::new(&engine, ());
                    let instance = Instance::new(&mut store, &module, &[])
                        .map_err(|error| error.to_string())?;
                    instantiate_nanos.push(started.elapsed().as_nanos());
                    std::hint::black_box(instance);
                }
                2 => {
                    let (mut store, function) = embedded_instance(&engine, &module)?;
                    let started = Instant::now();
                    let checksum = function
                        .call(&mut store, (ITERATIONS, SEED, 1))
                        .map_err(|error| error.to_string())?
                        as u32;
                    first_call_nanos.push(started.elapsed().as_nanos());
                    validate_checksum("embedded Wasmtime first call", checksum)?;
                }
                3 => {
                    let started = Instant::now();
                    let checksum = warm_function
                        .call(&mut warm_store, (ITERATIONS, SEED, WARM_CALL_BATCH))
                        .map_err(|error| error.to_string())?
                        as u32;
                    warm_call_nanos
                        .push(started.elapsed().as_nanos() / u128::from(WARM_CALL_BATCH));
                    validate_checksum("embedded Wasmtime warm call", checksum)?;
                }
                4 => {
                    let output_path = build_dir.join(format!("module-cli-sample-{sample}.cwasm"));
                    let started = Instant::now();
                    let output = Command::new(wasmtime_cli)
                        .args(["compile", "-O", "opt-level=2", "-o"])
                        .arg(&output_path)
                        .arg(wasm)
                        .output()
                        .map_err(|error| format!("failed to run Wasmtime CLI compiler: {error}"))?;
                    let elapsed = started.elapsed().as_nanos();
                    if !output.status.success() {
                        return Err(format!(
                            "Wasmtime CLI compilation failed with {}; stderr: {}",
                            output.status,
                            String::from_utf8_lossy(&output.stderr)
                        ));
                    }
                    let output_len = fs::metadata(&output_path)
                        .map_err(|error| {
                            format!("failed to inspect {}: {error}", output_path.display())
                        })?
                        .len();
                    if output_len == 0 {
                        return Err("Wasmtime CLI emitted an empty artifact".to_string());
                    }
                    cli_output_bytes.push(output_len);
                    fs::remove_file(&output_path).map_err(|error| {
                        format!("failed to remove {}: {error}", output_path.display())
                    })?;
                    cli_nanos.push(elapsed);
                }
                5 => {
                    let started = Instant::now();
                    let machine = Rv32Machine::from_elf(
                        &product_bytes,
                        Rv32MachineConfig {
                            ram_size: PRODUCT_RAM_BYTES,
                            debug_limit: 0,
                            execution: dbt_config,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                    construct_nanos.push(started.elapsed().as_nanos());
                    std::hint::black_box(machine);
                }
                6 => {
                    let started = Instant::now();
                    let mut machine = Rv32Machine::from_elf(
                        &product_bytes,
                        Rv32MachineConfig {
                            ram_size: PRODUCT_RAM_BYTES,
                            debug_limit: 0,
                            execution: dbt_config,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                    let outcome = machine.run(20_100_000).map_err(|error| error.to_string())?;
                    first_completion_nanos.push(started.elapsed().as_nanos());
                    let checksum = match outcome {
                        Rv32MachineOutcome::Halted { exit_code, .. } => exit_code as u32,
                        other => return Err(format!("cold Cached DBT did not halt: {other:?}")),
                    };
                    validate_checksum("cold Cached DBT", checksum)?;
                }
                7 => {
                    let mut machine = Rv32Machine::from_elf(
                        &product_bytes,
                        Rv32MachineConfig {
                            ram_size: PRODUCT_RAM_BYTES,
                            debug_limit: 0,
                            execution: dbt_config,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                    machine.enable_dbt_translation_timing();
                    let outcome = machine.run(20_100_000).map_err(|error| error.to_string())?;
                    let checksum = match outcome {
                        Rv32MachineOutcome::Halted { exit_code, .. } => exit_code as u32,
                        other => return Err(format!("timed Cached DBT did not halt: {other:?}")),
                    };
                    validate_checksum("timed Cached DBT", checksum)?;
                    let stats = machine
                        .dbt_stats()
                        .ok_or_else(|| "cold Cached DBT did not expose DBT stats".to_string())?;
                    lift_nanos.push(u128::from(stats.lift_nanos));
                    lower_nanos.push(u128::from(stats.lower_nanos));
                    publish_nanos.push(u128::from(stats.publish_nanos));
                    if stats.timed_translations != stats.translations {
                        return Err("DBT phase timer did not cover every translation".to_string());
                    }
                    last_stats = Some(stats);
                }
                _ => unreachable!("rotating compilation candidate is in range"),
            }
        }
    }
    let cli_output_bytes = cli_output_bytes
        .first()
        .copied()
        .filter(|first| cli_output_bytes.iter().all(|size| size == first))
        .ok_or_else(|| "Wasmtime CLI output sizes were empty or inconsistent".to_string())?;
    let wasmtime_embedded_warm_median = product_percentile(&warm_call_nanos, 50);
    let wasmtime_process_warm_median = product_percentile(&wasmtime_process_warm_nanos, 50);
    let dbt_warm_median = product_percentile(&dbt_warm_nanos, 50);
    let stats = last_stats.unwrap();
    let blocks = Some(stats.translations);
    let instructions = Some(stats.decoded_slots_built);
    let output_bytes = Some(stats.emitted_bytes);
    let rows = vec![
        phase(
            "wasmtime-embedded",
            "compile",
            compile_nanos,
            Some(wasm_bytes.len() as u64),
            None,
            None,
            None,
            Some(wasmtime_embedded_warm_median),
            true,
        ),
        phase(
            "wasmtime-embedded",
            "instantiate",
            instantiate_nanos,
            None,
            None,
            None,
            None,
            Some(wasmtime_embedded_warm_median),
            false,
        ),
        phase(
            "wasmtime-embedded",
            "first-call",
            first_call_nanos,
            None,
            None,
            None,
            None,
            Some(wasmtime_embedded_warm_median),
            false,
        ),
        phase(
            "wasmtime-embedded",
            "warm-call",
            warm_call_nanos,
            None,
            None,
            None,
            None,
            None,
            false,
        ),
        phase(
            "wasmtime-cli",
            "process-compile-serialize",
            cli_nanos,
            Some(wasm_bytes.len() as u64),
            None,
            None,
            Some(cli_output_bytes),
            Some(wasmtime_process_warm_median),
            true,
        ),
        phase(
            "rv32-cached-dbt",
            "machine-construct",
            construct_nanos,
            Some(product_bytes.len() as u64),
            None,
            None,
            None,
            Some(dbt_warm_median),
            false,
        ),
        phase(
            "rv32-cached-dbt",
            "first-completion",
            first_completion_nanos,
            Some(product_bytes.len() as u64),
            blocks,
            instructions,
            output_bytes,
            Some(dbt_warm_median),
            false,
        ),
        phase(
            "rv32-cached-dbt",
            "lift",
            lift_nanos,
            None,
            blocks,
            instructions,
            None,
            Some(dbt_warm_median),
            true,
        ),
        phase(
            "rv32-cached-dbt",
            "lower",
            lower_nanos,
            None,
            blocks,
            instructions,
            output_bytes,
            Some(dbt_warm_median),
            true,
        ),
        phase(
            "rv32-cached-dbt",
            "publish",
            publish_nanos,
            None,
            blocks,
            instructions,
            output_bytes,
            Some(dbt_warm_median),
            true,
        ),
        phase(
            "rv32-cached-dbt",
            "warm-execution",
            dbt_warm_nanos,
            None,
            None,
            None,
            None,
            None,
            false,
        ),
    ];

    println!("\nCompilation and startup phase report");
    println!("scope_note\tWasmtime compile covers the whole module; RV32 DBT phases cover only lazily reached blocks");
    println!("{COMPILATION_PHASE_REPORT_HEADER}");
    for row in rows {
        println!("{}", format_compilation_phase(row)?);
    }
    Ok(())
}

#[cfg(feature = "wasmtime-comparison")]
fn embedded_instance(
    engine: &Engine,
    module: &Module,
) -> Result<(Store<()>, TypedFunc<(u32, u32, u32), i32>), String> {
    let mut store = Store::new(engine, ());
    let instance = Instance::new(&mut store, module, &[]).map_err(|error| error.to_string())?;
    let function = instance
        .get_typed_func::<(u32, u32, u32), i32>(&mut store, "benchmark_batch")
        .map_err(|error| error.to_string())?;
    Ok((store, function))
}

#[cfg(feature = "wasmtime-comparison")]
fn validate_checksum(owner: &str, checksum: u32) -> Result<(), String> {
    if checksum != EXPECTED_CHECKSUM {
        return Err(format!(
            "{owner} checksum mismatch: expected {EXPECTED_CHECKSUM:08x}, actual {checksum:08x}"
        ));
    }
    Ok(())
}

#[cfg(feature = "wasmtime-comparison")]
#[allow(clippy::too_many_arguments)]
fn phase(
    system: &'static str,
    phase: &'static str,
    nanos: Vec<u128>,
    input_bytes: Option<u64>,
    translated_blocks: Option<u64>,
    guest_instructions: Option<u64>,
    output_bytes: Option<u64>,
    warm_nanos: Option<u128>,
    amortized: bool,
) -> CompilationPhaseMeasurement {
    CompilationPhaseMeasurement {
        system,
        phase,
        nanos,
        input_bytes,
        translated_blocks,
        guest_instructions,
        output_bytes,
        warm_nanos,
        amortized,
    }
}

#[cfg(feature = "wasmtime-comparison")]
fn format_compilation_phase(mut row: CompilationPhaseMeasurement) -> Result<String, String> {
    row.nanos.sort_unstable();
    let median = product_percentile(&row.nanos, 50);
    let p95 = product_percentile(&row.nanos, 95);
    let per_input = optional_phase_rate(median, row.input_bytes)?;
    let per_instruction = optional_phase_rate(median, row.guest_instructions)?;
    let cold_to_warm = row
        .warm_nanos
        .map(|warm| compile_equivalent_calls(median, warm))
        .transpose()?;
    let equivalent = if row.amortized { cold_to_warm } else { None };
    Ok(format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        row.system,
        row.phase,
        row.nanos.len(),
        median,
        p95,
        option_u64(row.input_bytes),
        option_u64(row.translated_blocks),
        option_u64(row.guest_instructions),
        option_u64(row.output_bytes),
        option_f64(per_input),
        option_f64(per_instruction),
        option_f64(cold_to_warm),
        option_f64(equivalent),
    ))
}

fn calibrate_product(
    linker: &OsStr,
    source_root: &Path,
    build_dir: &Path,
    backend: Rv32ExecutionBackendConfig,
) -> Result<u64, String> {
    let mut batch = 1;
    loop {
        let elf = link_platform(linker, source_root, build_dir, "product", batch)?;
        let (elapsed, _) = run_product(&elf, batch, backend)?;
        match c_comparison_next_batch(batch, elapsed, SAMPLE_TARGET_NANOS)? {
            Some(next) => batch = next,
            None => return Ok(batch),
        }
    }
}

fn run_native(
    executable: &Path,
    batch: u64,
    timeout: Duration,
) -> Result<ProcessObservation, String> {
    run_process(
        executable.as_os_str(),
        &[
            ITERATIONS.to_string().into(),
            format!("0x{SEED:08x}").into(),
            batch.to_string().into(),
        ],
        timeout,
        ProcessOutputFormat::ChecksumRecord,
    )
}

fn run_qemu(
    qemu: &OsStr,
    elf: &Path,
    timeout: Duration,
    expected: u32,
) -> Result<ProcessObservation, String> {
    let observation = run_process(
        qemu,
        &[
            "-M".into(),
            "virt".into(),
            "-bios".into(),
            "none".into(),
            "-accel".into(),
            "tcg".into(),
            "-nographic".into(),
            "-monitor".into(),
            "none".into(),
            "-kernel".into(),
            elf.as_os_str().to_owned(),
        ],
        timeout,
        ProcessOutputFormat::ChecksumRecord,
    )?;
    if observation.checksum != expected {
        return Err(format!(
            "QEMU checksum mismatch: expected {expected:08x}, actual {:08x}",
            observation.checksum
        ));
    }
    Ok(observation)
}

fn compile_wasmtime(wasmtime: &OsStr, wasm: &Path, cwasm: &Path) -> Result<(), String> {
    let output = Command::new(wasmtime)
        .args(["compile", "-O", "opt-level=2", "-o"])
        .arg(cwasm)
        .arg(wasm)
        .output()
        .map_err(|error| format!("failed to precompile {}: {error}", wasm.display()))?;
    if !output.status.success() {
        return Err(format!(
            "Wasmtime AOT compilation failed with {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(())
}

fn run_wasmtime(
    wasmtime: &OsStr,
    cwasm: &Path,
    batch: u64,
    timeout: Duration,
) -> Result<ProcessObservation, String> {
    let observation = run_process(
        wasmtime,
        &[
            "run".into(),
            "--allow-precompiled".into(),
            "--invoke".into(),
            "benchmark_batch".into(),
            cwasm.as_os_str().to_owned(),
            ITERATIONS.to_string().into(),
            SEED.to_string().into(),
            batch.to_string().into(),
        ],
        timeout,
        ProcessOutputFormat::WasmtimeI32,
    )?;
    if observation.checksum != EXPECTED_CHECKSUM {
        return Err(format!(
            "Wasmtime checksum mismatch: expected {EXPECTED_CHECKSUM:08x}, actual {:08x}",
            observation.checksum,
        ));
    }
    Ok(observation)
}

fn run_process(
    program: &OsStr,
    arguments: &[OsString],
    timeout: Duration,
    format: ProcessOutputFormat,
) -> Result<ProcessObservation, String> {
    let start = Instant::now();
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start {:?}: {error}", program))?;
    loop {
        if child
            .try_wait()
            .map_err(|error| format!("failed to poll {:?}: {error}", program))?
            .is_some()
        {
            let elapsed_nanos = start.elapsed().as_nanos();
            let output = child
                .wait_with_output()
                .map_err(|error| format!("failed to collect {:?}: {error}", program))?;
            if !output.status.success() {
                return Err(format!(
                    "{:?} exited with {}; stderr: {}",
                    program,
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            if matches!(format, ProcessOutputFormat::ChecksumRecord) && !output.stderr.is_empty() {
                return Err(format!(
                    "{:?} produced unexpected stderr: {}",
                    program,
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            return Ok(ProcessObservation {
                elapsed_nanos,
                checksum: match format {
                    ProcessOutputFormat::ChecksumRecord => {
                        parse_c_comparison_result(&output.stdout)?
                    }
                    ProcessOutputFormat::WasmtimeI32 => parse_wasmtime_i32(&output.stdout)?,
                },
            });
        }
        if start.elapsed() >= timeout {
            child
                .kill()
                .map_err(|error| format!("failed to kill timed-out {:?}: {error}", program))?;
            let _ = child.wait();
            return Err(format!("{:?} exceeded timeout {timeout:?}", program));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn parse_wasmtime_i32(output: &[u8]) -> Result<u32, String> {
    let text = std::str::from_utf8(output)
        .map_err(|error| format!("Wasmtime result is not UTF-8: {error}"))?;
    let value = text
        .trim_end()
        .parse::<i32>()
        .map_err(|error| format!("Wasmtime result is not one i32: {error}"))?;
    Ok(value as u32)
}

fn run_product(
    elf: &Path,
    batch: u64,
    execution: Rv32ExecutionBackendConfig,
) -> Result<(u128, ProductDetails), String> {
    let bytes =
        fs::read(elf).map_err(|error| format!("failed to read {}: {error}", elf.display()))?;
    let mut machine = Rv32Machine::from_elf(
        &bytes,
        Rv32MachineConfig {
            ram_size: PRODUCT_RAM_BYTES,
            debug_limit: 0,
            execution,
        },
    )
    .map_err(|error| error.to_string())?;
    let budget = 20_000_000u64
        .checked_mul(batch)
        .and_then(|value| value.checked_add(100_000))
        .ok_or_else(|| "product instruction budget overflowed".to_string())?;
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let start = Instant::now();
    let outcome = machine.run(budget).map_err(|error| error.to_string())?;
    let elapsed = start.elapsed().as_nanos();
    let steady_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let steady_allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    if steady_allocations != 0 || steady_allocated_bytes != 0 {
        return Err(format!(
            "product C comparison allocated during run: {steady_allocations} allocations, {steady_allocated_bytes} bytes"
        ));
    }
    let (checksum, retired_instructions) = match outcome {
        Rv32MachineOutcome::Halted {
            exit_code,
            retired_delta,
            ..
        } => (exit_code as u32, retired_delta),
        other => return Err(format!("product C comparison did not halt: {other:?}")),
    };
    if checksum != EXPECTED_CHECKSUM {
        return Err(format!(
            "product checksum mismatch: expected {EXPECTED_CHECKSUM:08x}, actual {checksum:08x}"
        ));
    }
    let stats = machine.translation_stats();
    let dbt = machine.dbt_stats();
    Ok((
        elapsed,
        ProductDetails {
            retired_instructions,
            lookup_unit: stats.map(|value| value.lookup_unit.name()),
            cache_hits: stats.map(|value| value.hits),
            cache_misses: stats.map(|value| value.misses),
            cache_evictions: stats.map(|value| value.evictions),
            blocks_built: stats.map(|value| value.blocks_built),
            decoded_slots_built: stats.map(|value| value.decoded_slots_built),
            dbt_translations: dbt.map(|value| value.translations),
            dbt_publications: dbt.map(|value| value.publications),
            dbt_native_dispatches: dbt.map(|value| value.native_dispatches),
            dbt_chain_transitions: dbt.and_then(|value| value.chain_transitions),
            dbt_budget_overshoot: dbt.map(|value| value.budget_overshoot),
            dbt_max_budget_overshoot: dbt.map(|value| u64::from(value.max_budget_overshoot)),
            dbt_links_established: dbt.map(|value| value.links_established),
            dbt_links_reset: dbt.map(|value| value.links_reset),
            dbt_typed_slow_exits: dbt.map(|value| value.typed_slow_exits),
            dbt_metadata_evictions: dbt.map(|value| value.metadata_evictions),
            dbt_overlap_invalidations: dbt.map(|value| value.overlap_invalidations),
            dbt_lowered_load_sites: dbt.map(|value| value.lowered_load_sites),
            dbt_lowered_store_sites: dbt.map(|value| value.lowered_store_sites),
            dbt_local_self_backedge_sites: dbt.map(|value| value.local_self_backedge_sites),
            dbt_emitted_bytes: dbt.map(|value| value.emitted_bytes),
            dbt_alignment_padding_bytes: dbt.map(|value| value.alignment_padding_bytes),
            dbt_live_code_bytes: dbt.map(|value| value.live_code_bytes as u64),
            dbt_code_prefix_bytes: dbt.map(|value| value.code_prefix_bytes as u64),
            dbt_reserved_bytes: dbt.map(|value| value.reserved_bytes as u64),
            translation_bytes: machine.translation_bytes(),
            executable_bytes: machine.executable_bytes(),
            steady_allocations,
            steady_allocated_bytes,
        },
    ))
}

fn link_platform(
    linker: &OsStr,
    source_root: &Path,
    build_dir: &Path,
    platform: &str,
    batch: u64,
) -> Result<PathBuf, String> {
    let output = build_dir.join(format!("{platform}-batch-{batch}.elf"));
    let status = Command::new(linker)
        .args([
            OsStr::new("-m"),
            OsStr::new("elf32lriscv"),
            OsStr::new("--no-relax"),
            OsStr::new("--fatal-warnings"),
        ])
        .arg(format!("--defsym=__ck_batch={batch}"))
        .arg("-T")
        .arg(source_root.join(format!("{platform}.ld")))
        .arg(build_dir.join(format!("{platform}-start.o")))
        .arg(build_dir.join(format!("{platform}-wrapper.o")))
        .arg(build_dir.join("kernel-rv32.o"))
        .arg("-o")
        .arg(&output)
        .status()
        .map_err(|error| format!("failed to start {:?}: {error}", linker))?;
    if !status.success() {
        return Err(format!(
            "platform link for {platform} batch {batch} failed: {status}"
        ));
    }
    Ok(output)
}

fn read_manifest(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    text.lines()
        .skip(1)
        .map(|line| {
            line.split_once('\t')
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .ok_or_else(|| format!("invalid manifest line: {line}"))
        })
        .collect()
}

fn manifest_value<'a>(
    manifest: &'a BTreeMap<String, String>,
    key: &str,
) -> Result<&'a str, String> {
    manifest
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("comparison manifest lacks {key}"))
}

fn duration_from_nanos(nanos: u128) -> Result<Duration, String> {
    let nanos = u64::try_from(nanos).map_err(|_| "comparison timeout exceeds u64".to_string())?;
    Ok(Duration::from_nanos(nanos))
}

fn option_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| value.to_string())
}

#[cfg(feature = "wasmtime-comparison")]
fn option_f64(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.6}"))
        .unwrap_or_else(|| "-".to_string())
}

fn version_line(program: &OsStr) -> Result<String, String> {
    let output = Command::new(program)
        .arg("--version")
        .output()
        .map_err(|error| format!("failed to query {:?} version: {error}", program))?;
    if !output.status.success() {
        return Err(format!(
            "{:?} --version failed with {}",
            program, output.status
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("{:?} version is not UTF-8: {error}", program))?
        .lines()
        .next()
        .map(str::to_string)
        .ok_or_else(|| format!("{:?} returned an empty version", program))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|error| format!("failed to hash {}: {error}", path.display()))?;
    if !output.status.success() {
        return Err(format!("sha256sum failed for {}", path.display()));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|error| format!("sha256sum output is not UTF-8: {error}"))?;
    text.split_whitespace()
        .next()
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_string)
        .ok_or_else(|| format!("sha256sum returned an invalid hash for {}", path.display()))
}

#[cfg(all(test, feature = "wasmtime-comparison"))]
mod compilation_report_tests {
    use super::{format_compilation_phase, phase};

    #[test]
    fn formatter_preserves_real_distribution_and_unavailable_rates() {
        let formatted = format_compilation_phase(phase(
            "rv32-cached-dbt",
            "warm-execution",
            vec![10, 20, 30],
            None,
            None,
            None,
            None,
            None,
            false,
        ))
        .unwrap();
        let columns = formatted.split('\t').collect::<Vec<_>>();

        assert_eq!(columns[2], "3");
        assert_eq!(columns[3], "20");
        assert_eq!(columns[4], "30");
        assert_eq!(&columns[5..], &["-"; 8]);
    }

    #[test]
    fn lazy_phase_uses_guest_instructions_but_not_whole_elf_bytes() {
        let formatted = format_compilation_phase(phase(
            "rv32-cached-dbt",
            "lower",
            vec![1_000],
            None,
            Some(2),
            Some(4),
            Some(32),
            Some(250),
            true,
        ))
        .unwrap();
        let columns = formatted.split('\t').collect::<Vec<_>>();

        assert_eq!(columns[5], "-");
        assert_eq!(columns[9], "-");
        assert_eq!(columns[10], "250.000000");
        assert_eq!(columns[11], "4.000000");
        assert_eq!(columns[12], "4.000000");
    }
}
