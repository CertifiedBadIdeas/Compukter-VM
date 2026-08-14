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

use compukter_vm::benchmarks::{
    benchmark_geomean, benchmark_normalize_nanos, benchmark_rotating_order,
    execute_product_machine_batch, format_product_active_row, populate_product_ratios,
    product_backend_order, product_machine_batch, product_percentile, PreparedProductNative,
    ProductActiveTiming, ProductExecutionCandidate, ProductMachineBackend, ProductMachineImage,
    ProductMachineObservation, ProductMachineWorkload, PRODUCT_ACTIVE_REPORT_HEADER,
    PRODUCT_BLOCK_CACHE_SETS, PRODUCT_BLOCK_MAX_INSTRUCTIONS, PRODUCT_CACHE_SETS,
    PRODUCT_DBT_CACHE_SETS, PRODUCT_DBT_CODE_BYTES, PRODUCT_DBT_MAX_INSTRUCTIONS,
    PRODUCT_DEBUG_LIMIT, PRODUCT_RAM_BYTES, PRODUCT_RESIDENT_REPORT_HEADER,
};
use compukter_vm::rv32_machine::{
    Rv32DbtCodeAlignment, Rv32DbtRegisterProfile, Rv32ExecutionBackendConfig,
    DEFAULT_DBT_REGISTER_PROFILE, DEFAULT_DBT_SCRATCH_BYTES,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record_allocation(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            record_allocation(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::SeqCst);
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, new_size) };
        if !replacement.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
            if new_size >= layout.size() {
                grow_live(new_size - layout.size());
            } else {
                LIVE_BYTES.fetch_sub(layout.size() - new_size, Ordering::SeqCst);
            }
        }
        replacement
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn record_allocation(bytes: usize) {
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    ALLOCATED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    grow_live(bytes);
}

fn grow_live(bytes: usize) {
    let live = LIVE_BYTES.fetch_add(bytes, Ordering::SeqCst) + bytes;
    let mut peak = PEAK_LIVE_BYTES.load(Ordering::SeqCst);
    while live > peak {
        match PEAK_LIVE_BYTES.compare_exchange_weak(peak, live, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => break,
            Err(actual) => peak = actual,
        }
    }
}

fn main() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let first = arguments
        .next()
        .ok_or_else(|| "missing iterations or mode".to_string())?;
    if first == "alignment-sweep" {
        let iterations = parse_positive("iterations", arguments.next())?;
        let warm_samples = parse_positive("warm_samples", arguments.next())?.max(21) as usize;
        if arguments.next().is_some() {
            return Err(
                "usage: rv32_machine_benchmarks alignment-sweep <iterations> <warm_samples>"
                    .to_string(),
            );
        }
        return run_alignment_sweep(iterations, warm_samples);
    }
    if first == "register-alignment-matrix" {
        let iterations = parse_positive("iterations", arguments.next())?;
        let warm_samples = parse_positive("warm_samples", arguments.next())?.max(21) as usize;
        if arguments.next().is_some() {
            return Err(
                "usage: rv32_machine_benchmarks register-alignment-matrix <iterations> <warm_samples>"
                    .to_string(),
            );
        }
        return run_register_alignment_matrix(iterations, warm_samples);
    }
    let iterations = parse_positive("iterations", Some(first))?;
    let warm_samples = parse_positive("warm_samples", arguments.next())?.max(21) as usize;
    let resident_samples = parse_positive("resident_samples", arguments.next())?.max(7) as usize;
    if arguments.next().is_some() {
        return Err(
            "usage: rv32_machine_benchmarks <iterations> <warm_samples> <resident_samples>"
                .to_string(),
        );
    }

    let active = measure_active(iterations, warm_samples)?;
    print_active_report(iterations, warm_samples, &active);
    let resident = measure_resident(iterations, resident_samples)?;
    print_resident_report(iterations, resident_samples, &resident);
    Ok(())
}

const ALIGNMENT_DBT_CACHE_SETS: usize = 512;

#[derive(Clone, Copy)]
struct LayoutCandidate {
    name: &'static str,
    register_profile: Rv32DbtRegisterProfile,
    alignment: Rv32DbtCodeAlignment,
}

const ALIGNMENT_CANDIDATES: [LayoutCandidate; 5] = [
    LayoutCandidate {
        name: "block-base-16",
        register_profile: DEFAULT_DBT_REGISTER_PROFILE,
        alignment: Rv32DbtCodeAlignment::BlockBase(16),
    },
    LayoutCandidate {
        name: "block-base-32",
        register_profile: DEFAULT_DBT_REGISTER_PROFILE,
        alignment: Rv32DbtCodeAlignment::BlockBase(32),
    },
    LayoutCandidate {
        name: "block-base-64",
        register_profile: DEFAULT_DBT_REGISTER_PROFILE,
        alignment: Rv32DbtCodeAlignment::BlockBase(64),
    },
    LayoutCandidate {
        name: "block-base-128",
        register_profile: DEFAULT_DBT_REGISTER_PROFILE,
        alignment: Rv32DbtCodeAlignment::BlockBase(128),
    },
    LayoutCandidate {
        name: "chain-entry-32",
        register_profile: DEFAULT_DBT_REGISTER_PROFILE,
        alignment: Rv32DbtCodeAlignment::ChainEntry(32),
    },
];

const REGISTER_ALIGNMENT_CANDIDATES: [LayoutCandidate; 4] = [
    LayoutCandidate {
        name: "stable7-base32",
        register_profile: Rv32DbtRegisterProfile::Stable7,
        alignment: Rv32DbtCodeAlignment::BlockBase(32),
    },
    LayoutCandidate {
        name: "stable7-base64",
        register_profile: Rv32DbtRegisterProfile::Stable7,
        alignment: Rv32DbtCodeAlignment::BlockBase(64),
    },
    LayoutCandidate {
        name: "rcx8-base32",
        register_profile: Rv32DbtRegisterProfile::RcxOverflow8,
        alignment: Rv32DbtCodeAlignment::BlockBase(32),
    },
    LayoutCandidate {
        name: "rcx8-base64",
        register_profile: Rv32DbtRegisterProfile::RcxOverflow8,
        alignment: Rv32DbtCodeAlignment::BlockBase(64),
    },
];

struct LayoutMeasurement {
    workload: ProductMachineWorkload,
    candidate: LayoutCandidate,
    batch: u64,
    construction_nanos: Vec<u128>,
    execution_nanos: Vec<u128>,
    observation: Option<ProductMachineObservation>,
    steady_allocations: u64,
    steady_allocated_bytes: u64,
}

fn layout_execution(candidate: LayoutCandidate, sets: usize) -> Rv32ExecutionBackendConfig {
    Rv32ExecutionBackendConfig::CachedDbt {
        sets,
        max_instructions: PRODUCT_DBT_MAX_INSTRUCTIONS,
        scratch_bytes: DEFAULT_DBT_SCRATCH_BYTES,
        cache_bytes: PRODUCT_DBT_CODE_BYTES,
        code_alignment: candidate.alignment,
        register_profile: candidate.register_profile,
    }
}

fn run_alignment_sweep(iterations: u32, warm_samples: usize) -> Result<(), String> {
    run_layout_sweep(
        "RV32 Cached DBT code-alignment sweep",
        iterations,
        warm_samples,
        ALIGNMENT_DBT_CACHE_SETS,
        ALIGNMENT_CANDIDATES,
        "block-base-32",
    )
}

fn run_register_alignment_matrix(iterations: u32, warm_samples: usize) -> Result<(), String> {
    run_layout_sweep(
        "RV32 Cached DBT register/alignment matrix",
        iterations,
        warm_samples,
        PRODUCT_DBT_CACHE_SETS,
        REGISTER_ALIGNMENT_CANDIDATES,
        "stable7-base32",
    )
}

fn run_layout_sweep<const N: usize>(
    title: &str,
    iterations: u32,
    warm_samples: usize,
    cache_sets: usize,
    candidates: [LayoutCandidate; N],
    baseline_name: &str,
) -> Result<(), String> {
    let mut completed = Vec::with_capacity(ProductMachineWorkload::all().len() * N);
    for (workload_index, workload) in ProductMachineWorkload::all().iter().copied().enumerate() {
        let image = ProductMachineImage::new(workload, iterations)?;
        let mut measurements = candidates.map(|candidate| LayoutMeasurement {
            workload,
            candidate,
            batch: 1,
            construction_nanos: Vec::with_capacity(warm_samples),
            execution_nanos: Vec::with_capacity(warm_samples),
            observation: None,
            steady_allocations: 0,
            steady_allocated_bytes: 0,
        });

        for measurement in &mut measurements {
            let mut probe = image.prepare_with_execution(
                ProductMachineBackend::CachedDbt,
                layout_execution(measurement.candidate, cache_sets),
            )?;
            let started = Instant::now();
            probe.execute()?;
            measurement.batch = product_machine_batch(started.elapsed().as_nanos());
        }

        for sample_index in 0..warm_samples {
            for candidate_index in benchmark_rotating_order::<N>(workload_index, sample_index) {
                let measurement = &mut measurements[candidate_index];
                let construction_started = Instant::now();
                let mut machines = image.prepare_batch_with_execution(
                    ProductMachineBackend::CachedDbt,
                    layout_execution(measurement.candidate, cache_sets),
                    measurement.batch,
                )?;
                measurement
                    .construction_nanos
                    .push(construction_started.elapsed().as_nanos());

                ALLOCATIONS.store(0, Ordering::Relaxed);
                ALLOCATED_BYTES.store(0, Ordering::Relaxed);
                let execution_started = Instant::now();
                let observation = execute_product_machine_batch(&mut machines)?;
                measurement
                    .execution_nanos
                    .push(execution_started.elapsed().as_nanos());
                let allocations = ALLOCATIONS.load(Ordering::Relaxed);
                let allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
                measurement.steady_allocations = measurement.steady_allocations.max(allocations);
                measurement.steady_allocated_bytes =
                    measurement.steady_allocated_bytes.max(allocated_bytes);
                measurement.observation = Some(observation);
            }
        }

        for measurement in &mut measurements {
            measurement.construction_nanos.sort_unstable();
            measurement.execution_nanos.sort_unstable();
            if measurement.steady_allocations != 0 || measurement.steady_allocated_bytes != 0 {
                return Err(format!(
                    "{} {} allocated during execution: {} allocations, {} bytes",
                    measurement.workload.name(),
                    measurement.candidate.name,
                    measurement.steady_allocations,
                    measurement.steady_allocated_bytes,
                ));
            }
        }
        let expected_retired = measurements[0]
            .observation
            .as_ref()
            .ok_or_else(|| "layout row has no observation".to_string())?
            .retired_instructions;
        for measurement in &measurements[1..] {
            let retired = measurement
                .observation
                .as_ref()
                .ok_or_else(|| "layout row has no observation".to_string())?
                .retired_instructions;
            if retired != expected_retired {
                return Err(format!(
                    "{} retired instruction mismatch: expected {expected_retired}, {} retired {retired}",
                    workload.name(),
                    measurement.candidate.name,
                ));
            }
        }
        completed.extend(measurements);
    }
    print_layout_report(
        title,
        iterations,
        warm_samples,
        cache_sets,
        baseline_name,
        &candidates,
        &completed,
    )
}

fn normalized_percentile(values: &[u128], percentile: usize, batch: u64) -> Result<f64, String> {
    benchmark_normalize_nanos(product_percentile(values, percentile), batch)
}

fn register_profile_name(profile: Rv32DbtRegisterProfile) -> &'static str {
    match profile {
        Rv32DbtRegisterProfile::Stable7 => "stable7",
        Rv32DbtRegisterProfile::RcxOverflow8 => "rcx-overflow8",
    }
}

fn alignment_parts(alignment: Rv32DbtCodeAlignment) -> (&'static str, usize) {
    match alignment {
        Rv32DbtCodeAlignment::BlockBase(bytes) => ("block-base", bytes),
        Rv32DbtCodeAlignment::ChainEntry(bytes) => ("chain-entry", bytes),
    }
}

fn print_layout_report(
    title: &str,
    iterations: u32,
    warm_samples: usize,
    cache_sets: usize,
    baseline_name: &str,
    candidates: &[LayoutCandidate],
    rows: &[LayoutMeasurement],
) -> Result<(), String> {
    println!("{title}");
    println!("iterations\t{iterations}");
    println!("warm_samples\t{warm_samples}");
    println!("baseline\t{baseline_name}");
    println!("dbt_cache_sets\t{cache_sets}");
    println!("dbt_max_instructions\t{PRODUCT_DBT_MAX_INSTRUCTIONS}");
    println!("dbt_code_bytes\t{PRODUCT_DBT_CODE_BYTES}");
    println!(
        "workload\tcandidate\tregister_profile\tcode_alignment\talignment_bytes\titerations\tchecksum\tretired_instructions\tbatch\tconstruction_median_ns\tconstruction_p95_ns\twarm_median_ns\twarm_p95_ns\tvs_baseline\tdbt_translations\tdbt_publications\tdbt_native_dispatches\tdbt_links_established\tdbt_emitted_bytes\tdbt_alignment_padding_bytes\tdbt_live_code_bytes\tdbt_code_prefix_bytes\tdbt_reserved_bytes\ttranslation_bytes\tcache_evictions\tsteady_allocations\tsteady_allocated_bytes"
    );

    for workload in ProductMachineWorkload::all() {
        let base = rows
            .iter()
            .find(|row| row.workload == *workload && row.candidate.name == baseline_name)
            .ok_or_else(|| format!("missing {baseline_name} row for {}", workload.name()))?;
        let base_median = normalized_percentile(&base.execution_nanos, 50, base.batch)?;
        for row in rows.iter().filter(|row| row.workload == *workload) {
            let observation = row
                .observation
                .as_ref()
                .ok_or_else(|| "alignment row has no observation".to_string())?;
            let stats = observation
                .dbt_stats
                .ok_or_else(|| "layout row has no DBT statistics".to_string())?;
            let median = normalized_percentile(&row.execution_nanos, 50, row.batch)?;
            let (alignment, alignment_bytes) = alignment_parts(row.candidate.alignment);
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.6}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                row.workload.name(),
                row.candidate.name,
                register_profile_name(row.candidate.register_profile),
                alignment,
                alignment_bytes,
                iterations,
                observation.checksum,
                observation.retired_instructions,
                row.batch,
                normalized_percentile(&row.construction_nanos, 50, row.batch)?,
                normalized_percentile(&row.construction_nanos, 95, row.batch)?,
                median,
                normalized_percentile(&row.execution_nanos, 95, row.batch)?,
                median / base_median,
                stats.translations,
                stats.publications,
                stats.native_dispatches,
                stats.links_established,
                stats.emitted_bytes,
                stats.alignment_padding_bytes,
                stats.live_code_bytes,
                stats.code_prefix_bytes,
                stats.reserved_bytes,
                observation.translation_bytes,
                stats.evictions,
                row.steady_allocations,
                row.steady_allocated_bytes,
            );
        }
    }

    println!();
    println!("candidate\texecution_geomean_vs_baseline");
    for sweep_candidate in candidates {
        let ratios = ProductMachineWorkload::all()
            .iter()
            .map(|workload| {
                let candidate = rows
                    .iter()
                    .find(|row| {
                        row.workload == *workload && row.candidate.name == sweep_candidate.name
                    })
                    .unwrap();
                let base = rows
                    .iter()
                    .find(|row| row.workload == *workload && row.candidate.name == baseline_name)
                    .unwrap();
                Ok(
                    normalized_percentile(&candidate.execution_nanos, 50, candidate.batch)?
                        / normalized_percentile(&base.execution_nanos, 50, base.batch)?,
                )
            })
            .collect::<Result<Vec<_>, String>>()?;
        println!(
            "{}\t{:.6}",
            sweep_candidate.name,
            benchmark_geomean(&ratios)?
        );
    }
    Ok(())
}

struct ActiveMeasurement {
    candidate: ProductExecutionCandidate,
    workload: ProductMachineWorkload,
    prepared: ActivePrepared,
    batch: u64,
    cold_nanos: u128,
    warm_nanos: Vec<u128>,
    observation: Option<ProductMachineObservation>,
    checksum: u32,
    steady_allocations: u64,
    steady_allocated_bytes: u64,
}

enum ActivePrepared {
    Native(Box<PreparedProductNative>),
    Machine {
        image: ProductMachineImage,
        backend: ProductMachineBackend,
    },
}

fn measure_active(iterations: u32, warm_samples: usize) -> Result<Vec<ActiveMeasurement>, String> {
    let mut completed = Vec::with_capacity(
        ProductMachineWorkload::all().len() * ProductExecutionCandidate::all().len(),
    );
    for (workload_index, workload) in ProductMachineWorkload::all().iter().copied().enumerate() {
        let image = ProductMachineImage::new(workload, iterations)?;
        let mut measurements = ProductExecutionCandidate::all()
            .iter()
            .copied()
            .map(|candidate| {
                let prepared = match candidate {
                    ProductExecutionCandidate::NativeHost => ActivePrepared::Native(Box::new(
                        PreparedProductNative::new(workload, iterations)?,
                    )),
                    ProductExecutionCandidate::Cached
                    | ProductExecutionCandidate::Predecoded
                    | ProductExecutionCandidate::BlockCached
                    | ProductExecutionCandidate::DirectDbt
                    | ProductExecutionCandidate::CachedDbt => {
                        let backend = candidate_backend(candidate);
                        ActivePrepared::Machine {
                            image: image.clone(),
                            backend,
                        }
                    }
                };
                Ok(ActiveMeasurement {
                    candidate,
                    workload,
                    prepared,
                    batch: 1,
                    cold_nanos: 0,
                    warm_nanos: Vec::with_capacity(warm_samples),
                    observation: None,
                    checksum: 0,
                    steady_allocations: 0,
                    steady_allocated_bytes: 0,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

        let native = active_for_candidate(&mut measurements, ProductExecutionCandidate::NativeHost);
        native.batch = calibrate_native(native)?;
        for candidate in [
            ProductExecutionCandidate::DirectDbt,
            ProductExecutionCandidate::CachedDbt,
        ] {
            let measurement = active_for_candidate(&mut measurements, candidate);
            measurement.batch = calibrate_machine(measurement)?;
        }
        for candidate_index in benchmark_rotating_order::<3>(workload_index, 0) {
            let candidate = ProductExecutionCandidate::all()[candidate_index];
            let measurement = active_for_candidate(&mut measurements, candidate);
            let (checksum, observation, nanos, allocations, allocated_bytes) =
                measure_active_execution(measurement)?;
            measurement.cold_nanos = nanos;
            measurement.checksum = checksum;
            measurement.observation = observation;
            measurement.steady_allocations = allocations;
            measurement.steady_allocated_bytes = allocated_bytes;
        }
        for sample_index in 0..warm_samples {
            for candidate_index in benchmark_rotating_order::<3>(workload_index, sample_index + 1) {
                let candidate = ProductExecutionCandidate::all()[candidate_index];
                let measurement = active_for_candidate(&mut measurements, candidate);
                let (checksum, observation, nanos, allocations, allocated_bytes) =
                    measure_active_execution(measurement)?;
                measurement.warm_nanos.push(nanos);
                measurement.checksum = checksum;
                measurement.observation = observation;
                measurement.steady_allocations = measurement.steady_allocations.max(allocations);
                measurement.steady_allocated_bytes =
                    measurement.steady_allocated_bytes.max(allocated_bytes);
            }
        }
        for measurement in &mut measurements {
            measurement.warm_nanos.sort_unstable();
            if measurement.steady_allocations != 0 || measurement.steady_allocated_bytes != 0 {
                return Err(format!(
                    "{} {} allocated during run: {} allocations, {} bytes",
                    measurement.workload.name(),
                    measurement.candidate.name(),
                    measurement.steady_allocations,
                    measurement.steady_allocated_bytes,
                ));
            }
        }
        completed.extend(measurements);
    }
    Ok(completed)
}

fn active_for_candidate(
    measurements: &mut [ActiveMeasurement],
    candidate: ProductExecutionCandidate,
) -> &mut ActiveMeasurement {
    measurements
        .iter_mut()
        .find(|measurement| measurement.candidate == candidate)
        .unwrap()
}

fn candidate_backend(candidate: ProductExecutionCandidate) -> ProductMachineBackend {
    match candidate {
        ProductExecutionCandidate::Cached => ProductMachineBackend::Cached,
        ProductExecutionCandidate::Predecoded => ProductMachineBackend::Predecoded,
        ProductExecutionCandidate::BlockCached => ProductMachineBackend::BlockCached,
        ProductExecutionCandidate::DirectDbt => ProductMachineBackend::DirectDbt,
        ProductExecutionCandidate::CachedDbt => ProductMachineBackend::CachedDbt,
        ProductExecutionCandidate::NativeHost => unreachable!(),
    }
}

fn calibrate_native(measurement: &mut ActiveMeasurement) -> Result<u64, String> {
    let ActivePrepared::Native(prepared) = &mut measurement.prepared else {
        return Err("native calibration requires a native candidate".to_string());
    };
    let mut batch = 1_u64;
    loop {
        let started = Instant::now();
        prepared.execute_batch(batch)?;
        if started.elapsed().as_nanos() >= 1_000_000 {
            return Ok(batch);
        }
        batch = batch
            .checked_mul(2)
            .ok_or_else(|| "native calibration batch overflow".to_string())?;
    }
}

fn calibrate_machine(measurement: &ActiveMeasurement) -> Result<u64, String> {
    let ActivePrepared::Machine { image, backend } = &measurement.prepared else {
        return Err("machine calibration requires a machine candidate".to_string());
    };
    let mut probe = image.prepare(*backend)?;
    let started = Instant::now();
    probe.execute()?;
    Ok(product_machine_batch(started.elapsed().as_nanos()))
}

fn measure_active_execution(
    measurement: &mut ActiveMeasurement,
) -> Result<(u32, Option<ProductMachineObservation>, u128, u64, u64), String> {
    let mut machine_batch = match &measurement.prepared {
        ActivePrepared::Native(_) => None,
        ActivePrepared::Machine { image, backend } => {
            Some(image.prepare_batch(*backend, measurement.batch)?)
        }
    };
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let started = Instant::now();
    let (checksum, observation) = match (&mut measurement.prepared, machine_batch.as_mut()) {
        (ActivePrepared::Native(prepared), None) => {
            let observation = prepared.execute_batch(measurement.batch)?;
            (observation.checksum, None)
        }
        (ActivePrepared::Machine { .. }, Some(machines)) => {
            let observation = execute_product_machine_batch(machines)?;
            (observation.checksum, Some(observation))
        }
        _ => unreachable!("prepared product candidate must match its batch storage"),
    };
    let nanos = started.elapsed().as_nanos();
    Ok((
        checksum,
        observation,
        nanos,
        ALLOCATIONS.load(Ordering::Relaxed),
        ALLOCATED_BYTES.load(Ordering::Relaxed),
    ))
}

fn print_active_report(iterations: u32, warm_samples: usize, rows: &[ActiveMeasurement]) {
    println!("RV32 product machine execution report");
    println!("iterations\t{iterations}");
    println!("warm_samples\t{warm_samples}");
    println!("ram_bytes\t{PRODUCT_RAM_BYTES}");
    println!("debug_limit\t{PRODUCT_DEBUG_LIMIT}");
    println!("cached_sets\t{PRODUCT_CACHE_SETS}");
    println!("cached_entries\t{}", PRODUCT_CACHE_SETS * 2);
    println!("block_cached_sets\t{PRODUCT_BLOCK_CACHE_SETS}");
    println!("block_max_instructions\t{PRODUCT_BLOCK_MAX_INSTRUCTIONS}");
    println!("{PRODUCT_ACTIVE_REPORT_HEADER}");
    let timings = rows
        .iter()
        .map(|row| {
            Ok(ProductActiveTiming {
                candidate: row.candidate,
                workload: row.workload,
                iterations,
                checksum: row.checksum,
                batch: row.batch,
                cold_nanos: benchmark_normalize_nanos(row.cold_nanos, row.batch)?,
                warm_median_nanos: benchmark_normalize_nanos(
                    product_percentile(&row.warm_nanos, 50),
                    row.batch,
                )?,
                warm_p95_nanos: benchmark_normalize_nanos(
                    product_percentile(&row.warm_nanos, 95),
                    row.batch,
                )?,
                machine: row.observation.clone(),
                steady_allocations: row.steady_allocations,
                steady_allocated_bytes: row.steady_allocated_bytes,
                vs_native: 0.0,
            })
        })
        .collect::<Result<Vec<_>, String>>()
        .and_then(populate_product_ratios)
        .unwrap();
    for timing in &timings {
        println!("{}", format_product_active_row(timing));
    }
    println!();
    println!("RV32 product machine native summary");
    println!("candidate\thost_overhead_geomean");
    for candidate in ProductExecutionCandidate::all() {
        let ratios = timings
            .iter()
            .filter(|timing| timing.candidate == *candidate)
            .map(|timing| timing.vs_native)
            .collect::<Vec<_>>();
        println!(
            "{}\t{:.6}",
            candidate.name(),
            benchmark_geomean(&ratios).unwrap()
        );
    }
    println!();
}

struct ResidentSample {
    nanos: u128,
    live_bytes: usize,
    peak_bytes: usize,
}

struct ResidentMeasurement {
    backend: ProductMachineBackend,
    population: usize,
    samples: Vec<ResidentSample>,
}

fn measure_resident(
    iterations: u32,
    sample_count: usize,
) -> Result<Vec<ResidentMeasurement>, String> {
    let image = ProductMachineImage::new(ProductMachineWorkload::PacketRing, iterations)?;
    let mut completed = Vec::with_capacity(20);
    for (population_index, population) in [1_usize, 32, 256, 1024].into_iter().enumerate() {
        let mut measurements = ProductMachineBackend::all()
            .iter()
            .copied()
            .map(|backend| ResidentMeasurement {
                backend,
                population,
                samples: Vec::with_capacity(sample_count),
            })
            .collect::<Vec<_>>();
        for sample_index in 0..sample_count {
            for backend in product_backend_order(population_index, sample_index) {
                let live_before = LIVE_BYTES.load(Ordering::SeqCst);
                PEAK_LIVE_BYTES.store(live_before, Ordering::SeqCst);
                let started = Instant::now();
                let mut machines = Vec::with_capacity(population);
                for _ in 0..population {
                    machines.push(image.prepare(backend)?);
                }
                let nanos = started.elapsed().as_nanos();
                let live_bytes = LIVE_BYTES
                    .load(Ordering::SeqCst)
                    .checked_sub(live_before)
                    .ok_or_else(|| "resident live heap underflow".to_string())?;
                let peak_bytes = PEAK_LIVE_BYTES
                    .load(Ordering::SeqCst)
                    .checked_sub(live_before)
                    .ok_or_else(|| "resident peak heap underflow".to_string())?;
                drop(machines);
                let live_after = LIVE_BYTES.load(Ordering::SeqCst);
                if live_after != live_before {
                    return Err(format!(
                        "resident {} population {} returned to {live_after} live bytes instead of baseline {live_before}",
                        backend.name(), population,
                    ));
                }
                measurements
                    .iter_mut()
                    .find(|measurement| measurement.backend == backend)
                    .unwrap()
                    .samples
                    .push(ResidentSample {
                        nanos,
                        live_bytes,
                        peak_bytes,
                    });
            }
        }
        completed.extend(measurements);
    }
    Ok(completed)
}

fn print_resident_report(iterations: u32, sample_count: usize, rows: &[ResidentMeasurement]) {
    let image = ProductMachineImage::new(ProductMachineWorkload::PacketRing, iterations).unwrap();
    println!("RV32 resident population report");
    println!("resident_samples\t{sample_count}");
    println!("workload\t{}", ProductMachineWorkload::PacketRing.name());
    println!("{PRODUCT_RESIDENT_REPORT_HEADER}");
    for row in rows {
        let mut nanos = row
            .samples
            .iter()
            .map(|sample| sample.nanos)
            .collect::<Vec<_>>();
        let mut live = row
            .samples
            .iter()
            .map(|sample| sample.live_bytes as u128)
            .collect::<Vec<_>>();
        let mut peak = row
            .samples
            .iter()
            .map(|sample| sample.peak_bytes as u128)
            .collect::<Vec<_>>();
        nanos.sort_unstable();
        live.sort_unstable();
        peak.sort_unstable();
        let resident_live_bytes = product_percentile(&live, 50);
        let cache_sets = match row.backend {
            ProductMachineBackend::Cached => PRODUCT_CACHE_SETS.to_string(),
            ProductMachineBackend::Predecoded
            | ProductMachineBackend::BlockCached
            | ProductMachineBackend::DirectDbt
            | ProductMachineBackend::CachedDbt => "-".to_string(),
        };
        let (block_cache_sets, block_max_instructions) = match row.backend {
            ProductMachineBackend::BlockCached => (
                PRODUCT_BLOCK_CACHE_SETS.to_string(),
                PRODUCT_BLOCK_MAX_INSTRUCTIONS.to_string(),
            ),
            ProductMachineBackend::Cached
            | ProductMachineBackend::Predecoded
            | ProductMachineBackend::DirectDbt
            | ProductMachineBackend::CachedDbt => ("-".to_string(), "-".to_string()),
        };
        let (dbt_cache_sets, dbt_max_instructions, dbt_code_bytes) = match row.backend {
            ProductMachineBackend::DirectDbt => (
                "-".to_string(),
                PRODUCT_DBT_MAX_INSTRUCTIONS.to_string(),
                PRODUCT_DBT_CODE_BYTES.to_string(),
            ),
            ProductMachineBackend::CachedDbt => (
                PRODUCT_DBT_CACHE_SETS.to_string(),
                PRODUCT_DBT_MAX_INSTRUCTIONS.to_string(),
                PRODUCT_DBT_CODE_BYTES.to_string(),
            ),
            ProductMachineBackend::Cached
            | ProductMachineBackend::Predecoded
            | ProductMachineBackend::BlockCached => {
                ("-".to_string(), "-".to_string(), "-".to_string())
            }
        };
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            row.backend.name(),
            row.population,
            product_percentile(&nanos, 50),
            product_percentile(&nanos, 95),
            resident_live_bytes,
            product_percentile(&peak, 50),
            resident_live_bytes as f64 / row.population as f64,
            row.population * PRODUCT_RAM_BYTES,
            image.elf_bytes().len(),
            image.executable_bytes(),
            image.rw_initialized_bytes(),
            PRODUCT_RAM_BYTES,
            PRODUCT_DEBUG_LIMIT,
            cache_sets,
            block_cache_sets,
            block_max_instructions,
            dbt_cache_sets,
            dbt_max_instructions,
            dbt_code_bytes,
        );
    }
}

fn parse_positive(name: &str, value: Option<String>) -> Result<u32, String> {
    let value = value.ok_or_else(|| format!("missing {name}"))?;
    let parsed = value
        .parse::<u32>()
        .map_err(|error| format!("invalid {name} {value:?}: {error}"))?;
    if parsed == 0 {
        return Err(format!("{name} must be positive"));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_alignment_matrix_executes_all_four_configurations() {
        assert_eq!(
            REGISTER_ALIGNMENT_CANDIDATES.map(|candidate| candidate.name),
            [
                "stable7-base32",
                "stable7-base64",
                "rcx8-base32",
                "rcx8-base64",
            ]
        );

        for workload in ProductMachineWorkload::all().iter().copied() {
            let image = ProductMachineImage::new(workload, 4).unwrap();
            let mut expected = None;
            for candidate in REGISTER_ALIGNMENT_CANDIDATES {
                let mut machine = image
                    .prepare_with_execution(
                        ProductMachineBackend::CachedDbt,
                        layout_execution(candidate, PRODUCT_DBT_CACHE_SETS),
                    )
                    .unwrap();
                let observation = machine.execute().unwrap();
                assert_eq!(
                    *expected
                        .get_or_insert((observation.checksum, observation.retired_instructions,)),
                    (observation.checksum, observation.retired_instructions),
                    "{} produced a different result for {}",
                    candidate.name,
                    workload.name(),
                );
            }
        }
    }
}
