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

use std::fs;
use std::path::PathBuf;

use compukter_vm::benchmarks::{
    c_comparison_next_batch, c_comparison_qemu_target_nanos, c_comparison_timeout_nanos,
    classify_x86_instruction, compile_equivalent_calls, has_x86_memory_operand,
    optional_phase_rate, parse_c_comparison_result, parse_llvm_symbol, parse_llvm_symbol_range,
    parse_wasmtime_function, InstructionGroup, COMPILATION_PHASE_REPORT_HEADER,
};

#[test]
fn codegen_audit_parses_bounded_real_format_regions() {
    let llvm = "0000000000001000 <benchmark_batch>:\n    1000: 89 04 86 movl %eax, (%rsi,%rax,4)\n    1003: c3 retq\n0000000000001010 <other>:\n    1010: c3 retq\n";
    let native = parse_llvm_symbol(llvm, "benchmark_batch").unwrap();
    assert_eq!(native.len(), 2);
    assert_eq!(native[0].address, 0x1000);
    assert_eq!(native[0].encoded_bytes, Some(3));
    assert_eq!(native[0].mnemonic, "movl");
    assert!(has_x86_memory_operand(&native[0].operands));

    let wasmtime = "wasm[0]::function[0]::benchmark_batch:\n            vpmulld %xmm0, %xmm1, %xmm2\n            retq\nwasm[0]::function[1]::other:\n            retq\n";
    let wasm = parse_wasmtime_function(wasmtime, "benchmark_batch").unwrap();
    assert_eq!(wasm.len(), 2);
    assert_eq!(
        classify_x86_instruction(&wasm[0].mnemonic),
        InstructionGroup::Vector
    );
    assert_eq!(classify_x86_instruction("movl"), InstructionGroup::Move);
    assert!(has_x86_memory_operand("%fs:0x28, %rax"));
    assert!(parse_llvm_symbol(llvm, "missing").is_err());
    assert!(parse_llvm_symbol(
        "0000 <benchmark_batch>:\n0001 <benchmark_batch>:\n  1: c3 retq\n",
        "benchmark_batch"
    )
    .is_err());
    assert!(parse_wasmtime_function(
        "wasm[0]::function[0]::benchmark_batch:\n",
        "benchmark_batch"
    )
    .is_err());
}

#[test]
fn codegen_audit_excludes_alignment_bytes_after_a_live_symbol_range() {
    let llvm = "0000000000000010 <dbt_pc_00001000_off_00000010>:\n\
                    10: 31 c0                         xorl %eax, %eax\n\
                    12: c3                            retq\n\
                    13: 00 00                         addb %al, (%rax)\n\
                0000000000000020 <next>:\n\
                    20: c3                            retq\n";

    let instructions =
        parse_llvm_symbol_range(llvm, "dbt_pc_00001000_off_00000010", 0x10, 3).unwrap();
    assert_eq!(instructions.len(), 2);
    assert_eq!(instructions.last().unwrap().address, 0x12);
    assert!(parse_llvm_symbol_range(llvm, "dbt_pc_00001000_off_00000010", 0x10, 1).is_err());
}

#[test]
fn compilation_report_math_rejects_ambiguous_denominators() {
    assert_eq!(compile_equivalent_calls(1_000, 250).unwrap(), 4.0);
    assert!(compile_equivalent_calls(1_000, 0).is_err());
    assert_eq!(optional_phase_rate(1_000, Some(4)).unwrap(), Some(250.0));
    assert_eq!(optional_phase_rate(1_000, None).unwrap(), None);
    assert!(optional_phase_rate(1_000, Some(0)).is_err());
    assert_eq!(
        COMPILATION_PHASE_REPORT_HEADER,
        "system\tphase\tsamples\tmedian_ns\tp95_ns\tinput_bytes\ttranslated_blocks\tguest_instructions\toutput_bytes\tns_per_input_byte\tns_per_guest_instruction\tcold_to_warm\tequivalent_warm_calls"
    );
}
use compukter_vm::rv32_machine::{
    Rv32ExecutionBackendConfig, Rv32Machine, Rv32MachineConfig, Rv32MachineOutcome,
};

fn workload_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benchmarks/rv32-c-comparison")
}

#[test]
fn comparison_feature_keeps_wasmtime_out_of_normal_builds() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let gate = fs::read_to_string(root.join("scripts/tests/rv32-c-qemu-comparison.sh")).unwrap();

    assert!(manifest.contains("dbt-translation-timing = []"));
    assert!(
        manifest.contains("wasmtime-comparison = [\"dep:wasmtime\", \"dbt-translation-timing\"]")
    );
    assert!(manifest
        .contains("wasmtime = { version = \"=47.0.3\", optional = true, default-features = false"));
    assert!(gate.contains("--features wasmtime-comparison"));
}

#[test]
fn portable_c_kernel_has_one_platform_neutral_entrypoint() {
    let header = fs::read_to_string(workload_root().join("kernel.h")).unwrap();
    let source = fs::read_to_string(workload_root().join("kernel.c")).unwrap();

    assert!(header.contains("uint32_t benchmark_kernel(uint32_t iterations, uint32_t seed);"));
    assert!(source.contains("CK_COMPUTE_ROUNDS"));
    assert!(source.contains("CK_ARRAY_WORDS"));
    assert!(source.contains("CK_COPY_BYTES"));
    assert!(header.contains("#define CK_ORACLE_ITERATIONS 1000u"));
    assert!(header.contains("#define CK_ORACLE_SEED 0x12345678u"));
    assert!(header.contains("#define CK_ORACLE_CHECKSUM 3993320792u"));

    for forbidden in [
        "malloc",
        "free(",
        "printf",
        "puts(",
        "clock(",
        "fopen",
        "volatile",
        "0x10000000",
    ] {
        assert!(
            !source.contains(forbidden),
            "portable kernel contains forbidden platform/libc token {forbidden}"
        );
    }
}

#[test]
fn comparison_build_keeps_shared_rv32_and_wasm_kernel_sources() {
    let script = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/compile-rv32-c-comparison.sh"),
    )
    .unwrap();

    assert_eq!(script.matches("-c \"$SOURCE_ROOT/kernel.c\"").count(), 2);
    assert!(script.matches("\"$BUILD_DIR/kernel-rv32.o\"").count() >= 3);
    assert!(script.contains("-O3 -march=native -flto"));
    assert!(script.contains("RV32_C_RV32_MARCH:=rv32im_zicsr"));
    assert!(script.contains("-march=\"$RV32_C_RV32_MARCH\""));
    assert!(script.contains("-mabi=ilp32"));
    assert!(script.contains("kernel-object-sha256"));
    assert!(script.contains("product.elf"));
    assert!(script.contains("qemu.elf"));
    assert!(script.contains("--target=wasm32-unknown-unknown"));
    assert!(script.contains("-msimd128"));
    assert!(script.contains("--export=benchmark_batch"));
    assert!(script.contains("kernel-wasm.o"));
    assert!(script.contains("wasm-flags"));
    assert!(script.contains("wasm-sha256"));
    assert!(script.contains("wasm-readobj.txt"));
    assert!(script.contains("Type:[[:space:]]+IMPORT"));
    assert!(script.contains("Wasm module unexpectedly imports host functions"));

    let wrapper = fs::read_to_string(workload_root().join("wasm-wrapper.c")).unwrap();
    assert!(wrapper.contains("benchmark_batch"));
    assert!(wrapper.contains("benchmark_kernel(runtime_iterations, runtime_seed)"));
    assert!(!wrapper.contains("wasi"));
}

#[test]
#[ignore = "requires the focused Clang/LLD C comparison artifacts"]
fn product_c_artifact_matches_the_fixed_native_and_qemu_oracle() {
    let path = std::env::var_os("RV32_C_PRODUCT_ELF")
        .expect("RV32_C_PRODUCT_ELF must name the product comparison ELF");
    let elf = fs::read(path).unwrap();

    for execution in [
        Rv32ExecutionBackendConfig::Cached { sets: 64 },
        Rv32ExecutionBackendConfig::Predecoded,
        Rv32ExecutionBackendConfig::BlockCached {
            sets: 32,
            max_instructions: 8,
        },
        Rv32ExecutionBackendConfig::DirectDbt {
            max_instructions: 8,
            scratch_bytes: 8 * 1024,
        },
        Rv32ExecutionBackendConfig::CachedDbt {
            sets: 32,
            max_instructions: 8,
            scratch_bytes: 8 * 1024,
            cache_bytes: 64 * 1024,
            code_alignment: compukter_vm::rv32_machine::DEFAULT_DBT_CODE_ALIGNMENT,
            register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
        },
    ] {
        let mut machine = Rv32Machine::from_elf(
            &elf,
            Rv32MachineConfig {
                ram_size: 16 * 1024,
                debug_limit: 0,
                execution,
            },
        )
        .unwrap();
        let outcome = machine.run(20_000_000).unwrap();
        assert!(matches!(
            outcome,
            Rv32MachineOutcome::Halted {
                exit_code: -301_646_504,
                ..
            }
        ));
    }
}

#[test]
fn comparison_parser_and_calibration_reject_ambiguous_measurements() {
    assert_eq!(
        parse_c_comparison_result(b"CK_RESULT\tee053d58\n").unwrap(),
        0xee05_3d58
    );
    assert_eq!(
        parse_c_comparison_result(b"CK_RESULT\tee053d58\r\n").unwrap(),
        0xee05_3d58
    );
    assert!(parse_c_comparison_result(b"CK_RESULT\tee053d58\nextra\n").is_err());
    assert!(parse_c_comparison_result(b"CK_RESULT\tee053d5\n").is_err());
    assert!(parse_c_comparison_result(b"").is_err());

    assert_eq!(c_comparison_next_batch(1, 249, 250).unwrap(), Some(2));
    assert_eq!(c_comparison_next_batch(8, 250, 250).unwrap(), None);
    assert!(c_comparison_next_batch(u64::MAX / 2 + 1, 1, 250).is_err());
    assert_eq!(
        c_comparison_qemu_target_nanos(8_000_000).unwrap(),
        400_000_000
    );
    assert_eq!(
        c_comparison_qemu_target_nanos(1_000_000).unwrap(),
        250_000_000
    );
    assert_eq!(c_comparison_timeout_nanos(400_000_000), 6_600_000_000);
}

#[test]
fn comparison_runner_keeps_qemu_system_tcg_explicit_and_report_stable() {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/rv32_c_comparison.rs"),
    )
    .unwrap();

    for token in [
        "qemu-system-riscv32",
        "\"-M\".into()",
        "\"virt\".into()",
        "\"-bios\".into()",
        "\"none\".into()",
        "\"-accel\".into()",
        "\"tcg\".into()",
        "\"-nographic\".into()",
        "\"-monitor\".into()",
    ] {
        assert!(
            source.contains(token),
            "runner lacks required QEMU token {token}"
        );
    }
    assert!(source.contains("candidate\\tmode\\titerations\\tseed\\tbatch\\tchecksum"));
    assert!(source.contains("rv32-block-cached"));
    assert!(source.contains("wasmtime-aot"));
    assert!(source.contains("wasmtime compile"));
    assert!(source.contains("benchmark_batch"));
    assert!(source.contains("vs_wasmtime"));
    assert!(source.contains("rv32-direct-dbt"));
    assert!(source.contains("rv32-cached-dbt"));
    for cache_kib in [16, 32, 64, 128, 256, 512] {
        assert!(source.contains(&format!("rv32-cached-dbt-{cache_kib}k")));
    }
    for sets in [16, 64, 128, 256, 512] {
        assert!(source.contains(&format!("rv32-cached-dbt-{sets}-sets")));
    }
    for max_instructions in [16, 32, 64] {
        assert!(source.contains(&format!("rv32-cached-dbt-block-{max_instructions}")));
    }
    for alignment in [16, 32, 64, 128] {
        assert!(source.contains(&format!("rv32-cached-dbt-align-base-{alignment}")));
    }
    assert!(source.contains("rv32-cached-dbt-align-chain-32"));
    assert_eq!(
        source
            .matches("Rv32DbtCodeAlignment::ChainEntry(32)")
            .count(),
        1
    );
    assert!(!source.contains("rv32-cached-dbt-32-sets"));
    assert!(!source.contains("rv32-cached-dbt-block-4"));
    assert!(source.contains("const DBT_MATRIX: [Candidate; 19]"));
    assert!(source.contains("usage: rv32_c_comparison BUILD_DIR WARM_SAMPLES"));
    assert!(source.contains("dbt_matrix\\tcache+sets+alignment"));
    assert!(!source.contains("cache|sets"));
    assert!(source.contains("product-machine-block-cached"));
    assert!(source.contains("lookup_unit\\tcache_hits\\tcache_misses\\tcache_evictions"));
    assert!(source.contains("blocks_built\\tdecoded_slots_built\\ttranslation_bytes"));
    assert!(source.contains("dbt_translations\\tdbt_publications\\tdbt_native_dispatches"));
    assert!(source.contains(
        "dbt_native_dispatches\\tdbt_chain_transitions\\tdbt_links_established\\tdbt_links_reset"
    ));
    assert!(source.contains("dbt_metadata_evictions\\tdbt_overlap_invalidations"));
    assert!(source.contains(
        "dbt_typed_slow_exits\\tdbt_metadata_evictions\\tdbt_overlap_invalidations\\tdbt_lowered_load_sites\\tdbt_lowered_store_sites"
    ));
    assert!(source.contains("steady_allocations\\tsteady_allocated_bytes"));
    assert!(source.contains("dbt_budget_overshoot\\tdbt_max_budget_overshoot"));
    assert!(
        source.contains("dbt_code_alignment\\tdbt_alignment_anchor\\tdbt_alignment_padding_bytes")
    );
    assert!(source.contains("dbt_live_code_bytes\\tdbt_code_prefix_bytes"));
    assert!(source.contains("dbt_local_self_backedge_sites"));
    assert!(source.contains("artifact_stem: Some(\"block-cached\")"));
    assert!(source.contains("artifact_stem: Some(\"direct-dbt\")"));
    assert!(source.contains("cached-dbt-64k"));
    assert!(source.contains("{stem}-calibrated-sha256"));
    assert!(!source.contains("Command::new(\"sh\")"));
    assert!(!source.contains("Command::new(\"bash\")"));
}

#[cfg(feature = "dbt-execution-profile")]
#[test]
fn comparison_runner_exposes_exact_profile_mode_and_report() {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/rv32_c_comparison.rs"),
    )
    .unwrap();

    for contract in [
        "rv32_c_comparison profile BUILD_DIR ITERATIONS PROFILE_CAPACITY",
        r"profile_summary\titerations\tchecksum\tinstrumented_ns\tcapacity\tused_records\tretained_bytes\tcounter_overflowed\tunique_blocks\tunique_static_edges",
        r"hot_blocks\trank\tpc\texecutions\tshare\tcumulative_share",
        r"hot_static_edges\trank\tsource_pc\ttarget_pc\tkind\texecutions\tshare\tcumulative_share",
        r"coverage\tpercent\tblocks_required",
        r"dynamic_exits\tjalr\tbudget\tslow_instruction\tmemory_access\ttrap_or_terminal",
    ] {
        assert!(
            source.contains(contract),
            "missing profile contract {contract}"
        );
    }
    assert!(source.contains("enable_dbt_execution_profile"));
    assert!(source.contains("dbt_execution_profile"));
    assert!(source.contains("code_alignment: DEFAULT_DBT_CODE_ALIGNMENT"));
}

#[test]
fn comparison_runner_exposes_distinct_compilation_phase_rows() {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/rv32_c_comparison.rs"),
    )
    .unwrap();

    for token in [
        "\"wasmtime-embedded\"",
        "\"compile\"",
        "\"instantiate\"",
        "\"first-call\"",
        "\"warm-call\"",
        "\"wasmtime-cli\"",
        "\"process-compile-serialize\"",
        "\"rv32-cached-dbt\"",
        "\"machine-construct\"",
        "\"first-completion\"",
        "\"lift\"",
        "\"lower\"",
        "\"publish\"",
        "\"warm-execution\"",
    ] {
        assert!(source.contains(token), "missing phase marker {token}");
    }
    assert!(source.contains("Module::new(&engine, &wasm_bytes)"));
}

#[test]
fn comparison_runner_exposes_product_only_self_ab_mode() {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/rv32_c_comparison.rs"),
    )
    .unwrap();

    for contract in [
        "rv32_c_comparison self-ab BUILD_DIR BASELINE CANDIDATE WARM_SAMPLES",
        "run_self_ab",
        "self-A/B candidates must be distinct",
        "self-A/B requires product DBT candidates",
    ] {
        assert!(
            source.contains(contract),
            "missing self-A/B contract {contract}"
        );
    }
}

#[test]
fn focused_self_ab_wrapper_avoids_external_runtime_dependencies() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = fs::read_to_string(root.join("scripts/tests/rv32-c-self-ab.sh")).unwrap();

    assert!(script.contains("--features dbt-translation-timing"));
    assert!(
        script.contains("self-ab \"$BUILD_DIR\" \"$BASELINE\" \"$CANDIDATE\" \"$WARM_SAMPLES\"")
    );
    assert!(script.contains("RV32_C_SELF_AB_BASELINE"));
    assert!(script.contains("RV32_C_SELF_AB_CANDIDATE"));
    assert!(!script.contains("qemu-system"));
    assert!(!script.contains("wasmtime"));
}

#[test]
fn self_ab_mode_calibrates_and_reports_only_the_selected_product_pair() {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/rv32_c_comparison.rs"),
    )
    .unwrap();

    for contract in [
        "self_ab_order",
        "self_ab_delta",
        "self_ab\\tbaseline\\tcandidate\\twarm_samples",
        "role\\tcandidate\\tbatch\\tchecksum\\ttotal_median_ns\\ttotal_p95_ns\\tns_per_kernel\\tdelta_vs_baseline_percent",
        "dbt_translations\\tdbt_publications\\tdbt_native_dispatches",
        "dbt_emitted_bytes\\tdbt_reserved_bytes\\tsteady_allocations\\tsteady_allocated_bytes",
        "self_ab_phase\\trole\\tcandidate\\tphase\\tsamples\\tmedian_ns\\tp95_ns\\tdelta_vs_baseline_percent",
        "baseline",
        "candidate",
        "self-A/B normalized retired instruction mismatch",
        "DBT phase timer did not cover every translation",
    ] {
        assert!(source.contains(contract), "missing self-A/B report contract {contract}");
    }
    assert!(source.contains("calibrate_product"));
    assert!(source.contains("run_product"));
}

#[test]
fn codegen_audit_export_contract_is_explicit() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("examples/rv32_c_codegen_audit.rs")).unwrap();

    assert!(source.contains("\"export\""));
    assert!(source.contains("dbt-code-cache.bin"));
    assert!(source.contains("dbt-blocks.tsv"));
    assert!(source.contains("dbt-support.tsv"));
    assert!(source.contains("dbt-register-pressure.tsv"));
    assert!(source.contains("dbt-register-pressure-weighted.tsv"));
    assert!(source.contains("dbt-code-cache.S"));
    assert!(source.contains("ee05_3d58"));
    assert!(source.contains("dbt_code_snapshot"));
}

#[test]
fn codegen_audit_report_contract_is_explicit() {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/rv32_c_codegen_audit.rs"),
    )
    .unwrap();

    assert!(source.contains("\"report\""));
    assert!(source.contains("codegen-report.tsv"));
    assert!(source.contains("native-analysis-disassembly.txt"));
    assert!(source.contains("wasmtime-aot-objdump.txt"));
    assert!(source.contains("dbt_hot_blocks"));
    assert!(source.contains("dbt_support_code"));
}

#[test]
fn focused_codegen_audit_keeps_perf_optional() {
    let script = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/tests/rv32-c-codegen-audit.sh"),
    )
    .unwrap();

    assert!(script.contains("compile-rv32-c-comparison.sh"));
    assert!(script.contains("--features dbt-code-audit,dbt-execution-profile"));
    assert!(script.contains("dbt-register-pressure-weighted.tsv"));
    assert!(script.contains("native-analysis.o"));
    assert!(script.contains("--disassemble-symbols=benchmark_kernel"));
    assert!(script.contains("dbt-code-cache.S"));
    assert!(script.contains("codegen-report.tsv"));
    assert!(script.contains("perf-report.tsv"));
    assert!(script.contains("cache_misses\\tcommand"));
    assert!(script.contains("status\\tunavailable"));
}

#[test]
fn focused_qemu_gate_is_not_hidden_behind_a_normal_verification_fallback() {
    let source = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/tests/rv32-c-qemu-comparison.sh"),
    )
    .unwrap();

    assert!(source.contains("qemu-system-riscv32"));
    assert!(source.contains("wasmtime"));
    assert!(source.contains("compile-rv32-c-comparison.sh"));
    assert!(source.contains("rv32_c_comparison"));
    assert!(!source.contains("RV32_C_DBT_SWEEP"));
    assert!(source.contains("--ignored --exact"));
    assert!(source.contains("rv32-block-cached"));
    assert!(source.contains("rv32-direct-dbt"));
    assert!(source.contains("rv32-cached-dbt"));
    assert!(source.contains("count == 26"));
    assert!(source.contains("wasmtime-aot"));
    assert!(source.contains("module.cwasm"));
    assert!(source.contains("product-block-cached-calibrated-disassembly.txt"));
    assert!(source.contains("product-direct-dbt-calibrated-disassembly.txt"));
    assert!(source.contains("for cache_kib in 16 32 64 128 256 512"));
    assert!(source.contains("for sets in 16 64 128 256 512"));
    assert!(source.contains("for max_instructions in 16 32 64"));
    assert!(source.contains("rv32-cached-dbt-block-${max_instructions}"));
    assert!(source.contains("for alignment in 16 32 64 128"));
    assert!(source.contains("rv32-cached-dbt-align-base-${alignment}"));
    assert!(source.contains("rv32-cached-dbt-align-chain-32"));
    assert!(source.contains("product-${candidate}-calibrated-disassembly.txt"));
    assert!(!source.contains("|| true"));
    assert!(!source.contains("qemu-riscv32"));
}

#[test]
fn standalone_comparison_paths_do_not_reach_into_a_parent_repository() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runner = fs::read_to_string(root.join("examples/rv32_c_comparison.rs")).unwrap();
    let compiler = fs::read_to_string(root.join("scripts/compile-rv32-c-comparison.sh")).unwrap();
    let gate = fs::read_to_string(root.join("scripts/tests/rv32-c-qemu-comparison.sh")).unwrap();

    assert!(runner.contains("join(\"benchmarks/rv32-c-comparison\")"));
    assert!(!runner.contains("../../tools"));
    for source in [compiler, gate] {
        assert!(!source.contains("host/compukter-vm"));
        assert!(!source.contains("../"));
    }
}
