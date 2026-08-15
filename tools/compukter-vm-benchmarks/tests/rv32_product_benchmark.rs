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

use compukter_vm::rv32_machine::{
    Rv32DbtCodeAlignment, Rv32ExecutionBackendConfig, Rv32TranslationLookupUnit,
    DEFAULT_DBT_MAX_INSTRUCTIONS, DEFAULT_DBT_SCRATCH_BYTES,
};
use compukter_vm_benchmarks::{
    benchmark_geomean, benchmark_normalize_nanos, benchmark_rotating_order,
    execute_product_machine_batch, format_product_active_row, native_checksum,
    populate_product_ratios, product_backend_order, product_machine_batch, product_percentile,
    self_ab_delta, self_ab_order, PreparedProductMachine, PreparedProductNative,
    ProductActiveTiming, ProductExecutionCandidate, ProductMachineBackend, ProductMachineImage,
    ProductMachineWorkload, PRODUCT_ACTIVE_REPORT_HEADER, PRODUCT_DBT_CACHE_SETS,
    PRODUCT_DBT_CODE_BYTES, PRODUCT_DBT_MAX_INSTRUCTIONS, PRODUCT_MACHINE_MAX_BATCH,
    PRODUCT_MACHINE_TARGET_NANOS, PRODUCT_RESIDENT_REPORT_HEADER,
};

#[test]
fn product_benchmark_and_vm_default_use_block_16() {
    assert_eq!(PRODUCT_DBT_MAX_INSTRUCTIONS, 16);
    assert_eq!(DEFAULT_DBT_MAX_INSTRUCTIONS, 16);
}

#[test]
fn product_image_accepts_an_explicit_execution_configuration() {
    let image = ProductMachineImage::new(ProductMachineWorkload::Compute32, 32).unwrap();
    let mut machine = image
        .prepare_with_execution(
            ProductMachineBackend::CachedDbt,
            Rv32ExecutionBackendConfig::CachedDbt {
                sets: PRODUCT_DBT_CACHE_SETS,
                max_instructions: PRODUCT_DBT_MAX_INSTRUCTIONS,
                scratch_bytes: DEFAULT_DBT_SCRATCH_BYTES,
                cache_bytes: PRODUCT_DBT_CODE_BYTES,
                code_alignment: Rv32DbtCodeAlignment::BlockBase(32),
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            },
        )
        .unwrap();

    assert_eq!(
        machine.execute().unwrap().workload,
        ProductMachineWorkload::Compute32
    );
}

#[test]
fn every_product_workload_executes_at_each_base_alignment() {
    for workload in ProductMachineWorkload::all() {
        let image = ProductMachineImage::new(*workload, 4).unwrap();
        for bytes in [16, 32, 64, 128] {
            let execution = Rv32ExecutionBackendConfig::CachedDbt {
                sets: 512,
                max_instructions: PRODUCT_DBT_MAX_INSTRUCTIONS,
                scratch_bytes: DEFAULT_DBT_SCRATCH_BYTES,
                cache_bytes: PRODUCT_DBT_CODE_BYTES,
                code_alignment: Rv32DbtCodeAlignment::BlockBase(bytes),
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            };
            let mut machines = image
                .prepare_batch_with_execution(ProductMachineBackend::CachedDbt, execution, 1)
                .unwrap();
            let observation = execute_product_machine_batch(&mut machines).unwrap();

            assert_eq!(observation.workload, *workload);
            let stats = observation.dbt_stats.unwrap();
            assert!(stats.alignment_padding_bytes > 0);
            assert!(stats.live_code_bytes > 0);
            assert!(stats.code_prefix_bytes >= stats.live_code_bytes);
        }
    }
}

#[test]
fn all_vm_backends_use_identical_strict_elf() {
    for workload in ProductMachineWorkload::all() {
        let image = ProductMachineImage::new(*workload, 17).unwrap();
        let cached = image.prepare(ProductMachineBackend::Cached).unwrap();
        let predecoded = image.prepare(ProductMachineBackend::Predecoded).unwrap();
        let block_cached = image.prepare(ProductMachineBackend::BlockCached).unwrap();
        let direct_dbt = image.prepare(ProductMachineBackend::DirectDbt).unwrap();
        let cached_dbt = image.prepare(ProductMachineBackend::CachedDbt).unwrap();

        assert_eq!(cached.image_fingerprint(), predecoded.image_fingerprint());
        assert_eq!(cached.image_fingerprint(), block_cached.image_fingerprint());
        assert_eq!(cached.image_fingerprint(), direct_dbt.image_fingerprint());
        assert_eq!(cached.image_fingerprint(), cached_dbt.image_fingerprint());
        assert_eq!(&image.elf_bytes()[..4], b"\x7fELF");
        assert_eq!(image.elf_bytes()[4], 1, "ELFCLASS32");
        assert_eq!(image.elf_bytes()[5], 1, "ELFDATA2LSB");
    }
}

#[test]
fn product_workloads_halt_with_expected_checksum() {
    for backend in ProductMachineBackend::all() {
        for workload in ProductMachineWorkload::all() {
            let mut prepared = PreparedProductMachine::new(*backend, *workload, 17).unwrap();
            let observation = prepared.execute().unwrap();
            let expected = match workload {
                ProductMachineWorkload::TrapRoundtrip => 17,
                _ => native_checksum(workload.decoder_workload().unwrap(), 17),
            };

            assert!(observation.complete_machine);
            assert_eq!(observation.checksum, expected, "{backend:?} {workload:?}");
            assert!(observation.retired_instructions > 0);
        }
    }
}

#[test]
fn product_observation_reports_backend_owned_storage() {
    let mut cached = PreparedProductMachine::new(
        ProductMachineBackend::Cached,
        ProductMachineWorkload::PacketRing,
        8,
    )
    .unwrap();
    let cached = cached.execute().unwrap();
    assert_eq!(cached.ram_bytes, 16 * 1024);
    assert!(cached.executable_bytes > 0);
    assert!(cached.translation_bytes > 0);
    let cached_stats = cached.translation_stats.unwrap();
    assert_eq!(
        cached_stats.lookup_unit,
        Rv32TranslationLookupUnit::Instruction
    );
    assert!(cached_stats.misses > 0);

    let mut predecoded = PreparedProductMachine::new(
        ProductMachineBackend::Predecoded,
        ProductMachineWorkload::PacketRing,
        8,
    )
    .unwrap();
    let predecoded = predecoded.execute().unwrap();
    assert!(predecoded.translation_bytes >= predecoded.executable_bytes);
    assert!(predecoded.translation_stats.is_none());

    let mut block_cached = PreparedProductMachine::new(
        ProductMachineBackend::BlockCached,
        ProductMachineWorkload::PacketRing,
        8,
    )
    .unwrap();
    let block_cached = block_cached.execute().unwrap();
    let block_stats = block_cached.translation_stats.unwrap();
    assert_eq!(block_stats.lookup_unit, Rv32TranslationLookupUnit::Block);
    assert!(block_stats.blocks_built > 0);
    assert!(block_stats.decoded_slots_built >= block_stats.blocks_built);
    assert!(block_cached.translation_bytes > 0);

    for backend in [
        ProductMachineBackend::DirectDbt,
        ProductMachineBackend::CachedDbt,
    ] {
        let mut prepared =
            PreparedProductMachine::new(backend, ProductMachineWorkload::PacketRing, 8).unwrap();
        let observation = prepared.execute().unwrap();
        let stats = observation.translation_stats.unwrap();
        assert_eq!(stats.lookup_unit, Rv32TranslationLookupUnit::Block);
        assert!(stats.blocks_built > 0);
        assert!(stats.decoded_slots_built >= stats.blocks_built);
        match backend {
            ProductMachineBackend::DirectDbt => {
                assert!(observation.translation_bytes > 16 * 1024);
                assert!(observation.translation_bytes < 64 * 1024);
            }
            ProductMachineBackend::CachedDbt => {
                assert!(observation.translation_bytes > 128 * 1024);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn product_observation_exposes_lazy_chain_counters() {
    let mut direct = PreparedProductMachine::new(
        ProductMachineBackend::DirectDbt,
        ProductMachineWorkload::Compute32,
        32,
    )
    .unwrap();
    let direct = direct.execute().unwrap().dbt_stats.unwrap();
    #[cfg(not(feature = "dbt-chain-stats"))]
    assert_eq!(direct.chain_transitions, None);
    #[cfg(feature = "dbt-chain-stats")]
    assert_eq!(direct.chain_transitions, Some(0));
    assert_eq!(direct.links_established, 0);
    assert_eq!(direct.links_reset, 0);

    let mut cached = PreparedProductMachine::new(
        ProductMachineBackend::CachedDbt,
        ProductMachineWorkload::Compute32,
        32,
    )
    .unwrap();
    let cached = cached.execute().unwrap().dbt_stats.unwrap();
    assert!(cached.links_established > 0);
    #[cfg(not(feature = "dbt-chain-stats"))]
    assert_eq!(cached.chain_transitions, None);
    #[cfg(feature = "dbt-chain-stats")]
    {
        let transitions = cached.chain_transitions.unwrap();
        assert!(transitions > 0);
        assert!(cached.native_dispatches < transitions);
    }
}

#[test]
fn product_report_formats_optional_exact_chain_transitions() {
    let mut prepared = PreparedProductMachine::new(
        ProductMachineBackend::CachedDbt,
        ProductMachineWorkload::Compute32,
        32,
    )
    .unwrap();
    let observation = prepared.execute().unwrap();
    let chain_transitions = observation.dbt_stats.unwrap().chain_transitions;
    let row = ProductActiveTiming {
        candidate: ProductExecutionCandidate::CachedDbt,
        workload: ProductMachineWorkload::Compute32,
        iterations: 32,
        checksum: observation.checksum,
        batch: 1,
        cold_nanos: 1.0,
        warm_median_nanos: 1.0,
        warm_p95_nanos: 1.0,
        machine: Some(observation),
        steady_allocations: 0,
        steady_allocated_bytes: 0,
        vs_native: 1.0,
    };

    let fields = format_product_active_row(&row)
        .split('\t')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match chain_transitions {
        Some(value) => assert_eq!(fields[17], value.to_string()),
        None => assert_eq!(fields[17], "-"),
    }
}

#[test]
fn product_sampling_order_is_interleaved_and_percentiles_are_stable() {
    assert_eq!(
        product_backend_order(0, 0),
        [
            ProductMachineBackend::DirectDbt,
            ProductMachineBackend::CachedDbt,
        ]
    );
    assert_eq!(
        product_backend_order(0, 1),
        [
            ProductMachineBackend::CachedDbt,
            ProductMachineBackend::DirectDbt,
        ]
    );
    assert_eq!(
        ProductExecutionCandidate::all(),
        [
            ProductExecutionCandidate::NativeHost,
            ProductExecutionCandidate::DirectDbt,
            ProductExecutionCandidate::CachedDbt,
        ]
    );
    assert_eq!(product_percentile(&[10, 20, 30, 40, 50], 50), 30);
    assert_eq!(product_percentile(&[10, 20, 30, 40, 50], 95), 50);
}

#[test]
fn native_product_workloads_match_machine_checksums() {
    for workload in ProductMachineWorkload::all() {
        let mut native = PreparedProductNative::new(*workload, 17).unwrap();
        let observation = native.execute_batch(3).unwrap();
        let expected = match workload {
            ProductMachineWorkload::TrapRoundtrip => 17,
            _ => native_checksum(workload.decoder_workload().unwrap(), 17),
        };
        assert_eq!(observation.checksum, expected);
        assert_eq!(observation.batch, 3);
    }
}

#[test]
fn product_timing_math_is_normalized_and_rotated() {
    assert_eq!(benchmark_normalize_nanos(10_001, 10).unwrap(), 1_000.1);
    assert_eq!(benchmark_rotating_order::<3>(0, 0), [0, 1, 2]);
    assert_eq!(benchmark_rotating_order::<3>(0, 1), [1, 2, 0]);
    assert_eq!(benchmark_rotating_order::<3>(1, 1), [2, 0, 1]);
    assert_eq!(benchmark_rotating_order::<4>(0, 1), [1, 2, 3, 0]);
    assert!((benchmark_geomean(&[4.0, 9.0]).unwrap() - 6.0).abs() < 1e-12);
    assert_eq!(ProductExecutionCandidate::NativeHost.name(), "native-host");
    assert_eq!(
        ProductExecutionCandidate::BlockCached.name(),
        "rv32-block-cached"
    );
    assert_eq!(
        ProductExecutionCandidate::DirectDbt.name(),
        "rv32-direct-dbt"
    );
    assert_eq!(
        ProductExecutionCandidate::CachedDbt.name(),
        "rv32-cached-dbt"
    );
}

#[test]
fn self_ab_helpers_alternate_pairs_and_use_baseline_as_denominator() {
    assert_eq!(self_ab_order(0), [0, 1]);
    assert_eq!(self_ab_order(1), [1, 0]);
    assert_eq!(self_ab_order(2), [0, 1]);
    assert!((self_ab_delta(100.0, 90.0).unwrap() + 0.1).abs() < f64::EPSILON);
    assert!((self_ab_delta(100.0, 125.0).unwrap() - 0.25).abs() < f64::EPSILON);
    assert!(self_ab_delta(0.0, 1.0).is_err());
}

#[test]
fn short_product_machine_samples_use_bounded_power_of_two_batches() {
    assert_eq!(PRODUCT_MACHINE_TARGET_NANOS, 5_000_000);
    assert_eq!(PRODUCT_MACHINE_MAX_BATCH, 1024);
    assert_eq!(product_machine_batch(10_000_000), 1);
    assert_eq!(product_machine_batch(5_000_000), 1);
    assert_eq!(product_machine_batch(2_500_000), 2);
    assert_eq!(product_machine_batch(1_250_000), 4);
    assert_eq!(product_machine_batch(10_000), 512);
    assert_eq!(product_machine_batch(1), PRODUCT_MACHINE_MAX_BATCH);
    assert_eq!(product_machine_batch(0), PRODUCT_MACHINE_MAX_BATCH);
}

#[test]
fn product_machine_batch_executes_independent_prepared_machines() {
    let image = ProductMachineImage::new(ProductMachineWorkload::Compute32, 32).unwrap();
    assert!(image
        .prepare_batch(ProductMachineBackend::CachedDbt, 0)
        .is_err());
    let mut machines = image
        .prepare_batch(ProductMachineBackend::CachedDbt, 3)
        .unwrap();

    let observation = execute_product_machine_batch(&mut machines).unwrap();

    assert_eq!(machines.len(), 3);
    assert_eq!(observation.backend, ProductMachineBackend::CachedDbt);
    assert_eq!(observation.workload, ProductMachineWorkload::Compute32);
    assert!(observation.complete_machine);
    assert!(observation.retired_instructions > 0);
}

#[test]
fn resident_report_header_remains_stable() {
    assert_eq!(
        PRODUCT_RESIDENT_REPORT_HEADER,
        "backend\tpopulation\tconstruction_median_ns\tconstruction_p95_ns\tresident_live_bytes\tpeak_construction_bytes\tlive_bytes_per_machine\taggregate_ram_bytes\telf_bytes\texecutable_bytes\trw_initialized_bytes\tram_bytes\tdebug_limit\tcache_sets\tblock_cache_sets\tblock_max_instructions\tdbt_cache_sets\tdbt_max_instructions\tdbt_code_bytes"
    );
    assert_eq!(
        PRODUCT_ACTIVE_REPORT_HEADER,
        "workload\tcandidate\titerations\tchecksum\tbatch\tcold_ns\twarm_median_ns\twarm_p95_ns\toperations_per_second\tretired_instructions\tlookup_unit\tcache_hits\tcache_misses\tcache_evictions\tblocks_built\tdecoded_slots_built\tdbt_native_dispatches\tdbt_chain_transitions\tdbt_links_established\tdbt_links_reset\tram_bytes\texecutable_bytes\ttranslation_bytes\tsteady_allocations\tsteady_allocated_bytes\tvs_native\tdbt_budget_overshoot\tdbt_max_budget_overshoot"
    );
}

#[test]
fn native_rows_use_dashes_and_vm_ratios_use_normalized_medians() {
    let native = ProductActiveTiming::native(
        ProductMachineWorkload::Compute32,
        1000,
        7,
        2_500.0,
        2_700.0,
        42,
    );
    let cached = ProductActiveTiming::machine(
        ProductExecutionCandidate::Cached,
        ProductMachineWorkload::Compute32,
        1000,
        100_000.0,
        110_000.0,
        1_133_597_426,
    );
    let rows = populate_product_ratios(vec![native, cached]).unwrap();
    assert_eq!(rows[0].vs_native, 1.0);
    assert_eq!(rows[1].vs_native, 40.0);
    assert!(format_product_active_row(&rows[0]).contains("\t-\t-\t-\t-\t-\t-\t"));
}
