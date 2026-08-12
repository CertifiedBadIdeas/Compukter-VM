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
    benchmark_normalize_nanos, benchmark_rotating_order, c_comparison_next_batch,
    c_comparison_qemu_target_nanos, c_comparison_timeout_nanos, parse_c_comparison_result,
    product_percentile,
};
use compukter_vm::rv32_machine::{
    Rv32ExecutionBackendConfig, Rv32Machine, Rv32MachineConfig, Rv32MachineOutcome,
};
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
enum Candidate {
    Native,
    Qemu,
    Cached,
    Predecoded,
    BlockCached,
    DirectDbt,
    CachedDbt16K,
    CachedDbt32K,
    CachedDbt64K,
    CachedDbt128K,
    CachedDbt256K,
    CachedDbt512K,
}

impl Candidate {
    const ALL: [Self; 12] = [
        Self::Native,
        Self::Qemu,
        Self::Cached,
        Self::Predecoded,
        Self::BlockCached,
        Self::DirectDbt,
        Self::CachedDbt16K,
        Self::CachedDbt32K,
        Self::CachedDbt64K,
        Self::CachedDbt128K,
        Self::CachedDbt256K,
        Self::CachedDbt512K,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Native => "native-clang",
            Self::Qemu => "qemu-rv32-tcg",
            Self::Cached => "rv32-cached",
            Self::Predecoded => "rv32-predecoded",
            Self::BlockCached => "rv32-block-cached",
            Self::DirectDbt => "rv32-direct-dbt",
            Self::CachedDbt16K => "rv32-cached-dbt-16k",
            Self::CachedDbt32K => "rv32-cached-dbt-32k",
            Self::CachedDbt64K => "rv32-cached-dbt-64k",
            Self::CachedDbt128K => "rv32-cached-dbt-128k",
            Self::CachedDbt256K => "rv32-cached-dbt-256k",
            Self::CachedDbt512K => "rv32-cached-dbt-512k",
        }
    }

    fn product_config(self) -> Option<Rv32ExecutionBackendConfig> {
        match self {
            Self::Native | Self::Qemu => None,
            Self::Cached => Some(Rv32ExecutionBackendConfig::Cached {
                sets: PRODUCT_CACHE_SETS,
            }),
            Self::Predecoded => Some(Rv32ExecutionBackendConfig::Predecoded),
            Self::BlockCached => Some(Rv32ExecutionBackendConfig::BlockCached {
                sets: PRODUCT_BLOCK_CACHE_SETS,
                max_instructions: PRODUCT_BLOCK_MAX_INSTRUCTIONS,
            }),
            Self::DirectDbt => Some(Rv32ExecutionBackendConfig::DirectDbt {
                max_instructions: PRODUCT_DBT_MAX_INSTRUCTIONS,
                scratch_bytes: PRODUCT_DBT_SCRATCH_BYTES,
            }),
            Self::CachedDbt16K => Some(cached_dbt_config(16 * 1024)),
            Self::CachedDbt32K => Some(cached_dbt_config(32 * 1024)),
            Self::CachedDbt64K => Some(cached_dbt_config(64 * 1024)),
            Self::CachedDbt128K => Some(cached_dbt_config(128 * 1024)),
            Self::CachedDbt256K => Some(cached_dbt_config(256 * 1024)),
            Self::CachedDbt512K => Some(cached_dbt_config(512 * 1024)),
        }
    }

    fn mode(self) -> &'static str {
        match self {
            Self::Native => "clang-O3-native-lto",
            Self::Qemu => "virt-system-tcg",
            Self::Cached => "product-machine-cached",
            Self::Predecoded => "product-machine-predecoded",
            Self::BlockCached => "product-machine-block-cached",
            Self::DirectDbt => "product-machine-direct-dbt",
            Self::CachedDbt16K
            | Self::CachedDbt32K
            | Self::CachedDbt64K
            | Self::CachedDbt128K
            | Self::CachedDbt256K
            | Self::CachedDbt512K => "product-machine-cached-dbt",
        }
    }

    fn artifact_stem(self) -> Option<&'static str> {
        match self {
            Self::Native => None,
            Self::Qemu => Some("qemu"),
            Self::Cached => Some("cached"),
            Self::Predecoded => Some("predecoded"),
            Self::BlockCached => Some("block-cached"),
            Self::DirectDbt => Some("direct-dbt"),
            Self::CachedDbt16K => Some("cached-dbt-16k"),
            Self::CachedDbt32K => Some("cached-dbt-32k"),
            Self::CachedDbt64K => Some("cached-dbt-64k"),
            Self::CachedDbt128K => Some("cached-dbt-128k"),
            Self::CachedDbt256K => Some("cached-dbt-256k"),
            Self::CachedDbt512K => Some("cached-dbt-512k"),
        }
    }
}

const fn cached_dbt_config(cache_bytes: usize) -> Rv32ExecutionBackendConfig {
    Rv32ExecutionBackendConfig::CachedDbt {
        sets: PRODUCT_DBT_CACHE_SETS,
        max_instructions: PRODUCT_DBT_MAX_INSTRUCTIONS,
        scratch_bytes: PRODUCT_DBT_SCRATCH_BYTES,
        cache_bytes,
    }
}

struct ProcessObservation {
    elapsed_nanos: u128,
    checksum: u32,
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
    dbt_typed_slow_exits: Option<u64>,
    dbt_lowered_load_sites: Option<u64>,
    dbt_lowered_store_sites: Option<u64>,
    dbt_emitted_bytes: Option<u64>,
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

fn main() {
    if let Err(error) = run() {
        eprintln!("RV32 C comparison failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
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

    let source_root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benchmarks/rv32-c-comparison");
    let native = build_dir.join("native-kernel");
    let manifest = read_manifest(&build_dir.join("manifest.tsv"))?;
    let qemu = env::var_os("RV32_C_QEMU").unwrap_or_else(|| "qemu-system-riscv32".into());
    let clang = env::var_os("RV32_C_CLANG").unwrap_or_else(|| "clang".into());
    let linker = env::var_os("RV32_C_LLD").unwrap_or_else(|| "ld.lld".into());

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
    let mut batches = [0_u64; Candidate::ALL.len()];
    batches[0] = native_batch;
    batches[1] = qemu_batch;
    for (index, candidate) in Candidate::ALL.iter().copied().enumerate().skip(2) {
        batches[index] = calibrate_product(
            &linker,
            &source_root,
            &build_dir,
            candidate.product_config().unwrap(),
        )?;
    }
    let candidate_elfs = Candidate::ALL
        .iter()
        .copied()
        .zip(batches)
        .map(|(candidate, batch)| match candidate {
            Candidate::Native => Ok(None),
            Candidate::Qemu => {
                link_platform(&linker, &source_root, &build_dir, "qemu", batch).map(Some)
            }
            _ => link_platform(&linker, &source_root, &build_dir, "product", batch).map(Some),
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut measurements = Candidate::ALL
        .into_iter()
        .zip(batches)
        .map(|(candidate, batch)| CandidateMeasurements {
            candidate,
            batch,
            samples: Vec::with_capacity(samples),
            details: ProductDetails::default(),
        })
        .collect::<Vec<_>>();

    let qemu_timeout = duration_from_nanos(c_comparison_timeout_nanos(qemu_target))?;
    for sample in 0..samples {
        for candidate_index in benchmark_rotating_order::<12>(0, sample) {
            let measurement = &mut measurements[candidate_index];
            let (elapsed, details) = match measurement.candidate {
                Candidate::Native => (
                    run_native(&native, measurement.batch, Duration::from_secs(30))?.elapsed_nanos,
                    ProductDetails::default(),
                ),
                Candidate::Qemu => (
                    run_qemu(
                        &qemu,
                        candidate_elfs[candidate_index].as_ref().unwrap(),
                        qemu_timeout,
                        EXPECTED_CHECKSUM,
                    )?
                    .elapsed_nanos,
                    ProductDetails::default(),
                ),
                candidate => run_product(
                    candidate_elfs[candidate_index].as_ref().unwrap(),
                    measurement.batch,
                    candidate.product_config().unwrap(),
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
    println!("qemu_startup_samples\t{STARTUP_SAMPLES}");
    println!("qemu_startup_median_ns\t{startup_median}");
    println!("qemu_target_ns\t{qemu_target}");
    println!("qemu_mode\t-M virt -bios none -accel tcg -nographic -monitor none");
    println!("qemu-version\t{}", version_line(&qemu)?);
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
    ] {
        println!("{key}\t{}", manifest_value(&manifest, key)?);
    }
    for (candidate, elf) in Candidate::ALL.iter().copied().zip(&candidate_elfs) {
        if let Some(stem) = candidate.artifact_stem() {
            println!(
                "{stem}-calibrated-sha256\t{}",
                sha256_file(elf.as_ref().unwrap())?
            );
        }
    }
    println!(
        "candidate\tmode\titerations\tseed\tbatch\tchecksum\ttotal_median_ns\ttotal_p95_ns\tns_per_kernel\tkernels_per_second\tvs_native\tvs_qemu\ttext_bytes\tqemu_startup_median_ns\tretired_instructions\tlookup_unit\tcache_hits\tcache_misses\tcache_evictions\tblocks_built\tdecoded_slots_built\ttranslation_bytes\tdbt_translations\tdbt_publications\tdbt_native_dispatches\tdbt_typed_slow_exits\tdbt_metadata_evictions\tdbt_overlap_invalidations\tdbt_lowered_load_sites\tdbt_lowered_store_sites\tdbt_emitted_bytes\tdbt_reserved_bytes\tsteady_allocations\tsteady_allocated_bytes"
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

    for (index, measurement) in measurements.iter().enumerate() {
        let median = product_percentile(&measurement.samples, 50);
        let p95 = product_percentile(&measurement.samples, 95);
        let per_kernel = normalized[index];
        let text_bytes = match measurement.candidate {
            Candidate::Native => manifest_value(&manifest, "native-text-bytes")?.to_string(),
            Candidate::Qemu => manifest_value(&manifest, "qemu-text-bytes")?.to_string(),
            _ => measurement.details.executable_bytes.to_string(),
        };
        let mode = measurement.candidate.mode();
        let product_candidate = measurement.candidate.product_config().is_some();
        println!(
            "{}\t{}\t{}\t0x{:08x}\t{}\t{:08x}\t{}\t{}\t{:.3}\t{:.3}\t{:.6}\t{:.6}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            measurement.candidate.name(),
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
            text_bytes,
            if matches!(measurement.candidate, Candidate::Qemu) {
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
            option_u64(measurement.details.dbt_typed_slow_exits),
            option_u64(measurement.details.dbt_metadata_evictions),
            option_u64(measurement.details.dbt_overlap_invalidations),
            option_u64(measurement.details.dbt_lowered_load_sites),
            option_u64(measurement.details.dbt_lowered_store_sites),
            option_u64(measurement.details.dbt_emitted_bytes),
            option_u64(measurement.details.dbt_reserved_bytes),
            option_u64(product_candidate.then_some(measurement.details.steady_allocations)),
            option_u64(product_candidate.then_some(measurement.details.steady_allocated_bytes)),
        );
    }
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
    )?;
    if observation.checksum != expected {
        return Err(format!(
            "QEMU checksum mismatch: expected {expected:08x}, actual {:08x}",
            observation.checksum
        ));
    }
    Ok(observation)
}

fn run_process(
    program: &OsStr,
    arguments: &[OsString],
    timeout: Duration,
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
            if !output.stderr.is_empty() {
                return Err(format!(
                    "{:?} produced unexpected stderr: {}",
                    program,
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            return Ok(ProcessObservation {
                elapsed_nanos,
                checksum: parse_c_comparison_result(&output.stdout)?,
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
            dbt_typed_slow_exits: dbt.map(|value| value.typed_slow_exits),
            dbt_metadata_evictions: dbt.map(|value| value.metadata_evictions),
            dbt_overlap_invalidations: dbt.map(|value| value.overlap_invalidations),
            dbt_lowered_load_sites: dbt.map(|value| value.lowered_load_sites),
            dbt_lowered_store_sites: dbt.map(|value| value.lowered_store_sites),
            dbt_emitted_bytes: dbt.map(|value| value.emitted_bytes),
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
