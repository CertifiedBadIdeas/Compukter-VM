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

use compukter_vm::rv32_machine::{
    Rv32ExecutionBackendConfig, Rv32Machine, Rv32MachineConfig, Rv32MachineOutcome,
};
use compukter_vm_benchmarks::{
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
