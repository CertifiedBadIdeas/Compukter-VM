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

#[cfg(feature = "dbt-code-audit")]
use compukter_vm::benchmarks::{
    classify_x86_instruction, has_x86_memory_operand, parse_llvm_symbol, parse_llvm_symbol_range,
    parse_wasmtime_function, DecodedHostInstruction, InstructionGroup, PRODUCT_RAM_BYTES,
};
#[cfg(all(feature = "dbt-code-audit", feature = "dbt-execution-profile"))]
use compukter_vm::rv32_machine::Rv32DbtExecutionProfile;
#[cfg(feature = "dbt-code-audit")]
use compukter_vm::rv32_machine::{
    Rv32DbtCodeSnapshot, Rv32DbtSupportCodeKind, Rv32ExecutionBackendConfig, Rv32Machine,
    Rv32MachineConfig, Rv32MachineOutcome, DEFAULT_DBT_SCRATCH_BYTES,
};
#[cfg(feature = "dbt-code-audit")]
use std::env;
#[cfg(feature = "dbt-code-audit")]
use std::fs;
#[cfg(feature = "dbt-code-audit")]
use std::path::{Path, PathBuf};

#[cfg(feature = "dbt-code-audit")]
#[derive(Debug, Clone)]
struct AuditBlock {
    guest_pc: u32,
    offset: u32,
    length: u32,
    guest_instructions: u32,
    linked_edges: u32,
    unlinked_edges: u32,
}

#[cfg(feature = "dbt-code-audit")]
#[derive(Default)]
struct InstructionCounts {
    groups: [u64; 9],
    memory_operands: u64,
}

#[cfg(feature = "dbt-code-audit")]
const EXPECTED_CHECKSUM: u32 = 0xee05_3d58;

#[cfg(not(feature = "dbt-code-audit"))]
fn main() {
    eprintln!("rv32_c_codegen_audit requires --features dbt-code-audit");
    std::process::exit(2);
}

#[cfg(feature = "dbt-code-audit")]
fn main() {
    if let Err(error) = run() {
        eprintln!("RV32 C codegen audit failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(feature = "dbt-code-audit")]
fn run() -> Result<(), String> {
    let mut args = env::args_os().skip(1);
    let command = args.next().ok_or_else(usage)?;
    let build_dir = PathBuf::from(args.next().ok_or_else(usage)?);
    match command.to_str() {
        Some("export") | Some("report") if args.next().is_some() => Err(usage()),
        Some("export") => export(&build_dir),
        Some("report") => report(&build_dir),
        Some("execute") => {
            let batch = args
                .next()
                .ok_or_else(usage)?
                .to_str()
                .ok_or_else(|| "batch is not UTF-8".to_string())?
                .parse::<u64>()
                .map_err(|error| format!("invalid batch: {error}"))?;
            if batch == 0 || args.next().is_some() {
                return Err(usage());
            }
            execute(&build_dir, batch)
        }
        _ => Err(usage()),
    }
}

#[cfg(feature = "dbt-code-audit")]
fn usage() -> String {
    "usage: rv32_c_codegen_audit <export|report> BUILD_DIR | execute BUILD_DIR BATCH".to_string()
}

#[cfg(feature = "dbt-code-audit")]
fn execute(build_dir: &Path, batch: u64) -> Result<(), String> {
    let elf_path = build_dir.join("product-audit-batch.elf");
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
                scratch_bytes: DEFAULT_DBT_SCRATCH_BYTES,
                cache_bytes: 128 * 1024,
                code_alignment: compukter_vm::rv32_machine::DEFAULT_DBT_CODE_ALIGNMENT,
            },
        },
    )
    .map_err(|error| error.to_string())?;
    let budget = batch
        .checked_mul(5_000_000)
        .ok_or_else(|| "instruction budget overflowed".to_string())?;
    let outcome = machine.run(budget).map_err(|error| error.to_string())?;
    let checksum = match outcome {
        Rv32MachineOutcome::Halted { exit_code, .. } => exit_code as u32,
        other => return Err(format!("audit workload did not halt: {other:?}")),
    };
    if checksum != EXPECTED_CHECKSUM {
        return Err(format!(
            "audit workload checksum mismatch: expected {EXPECTED_CHECKSUM:08x}, actual {checksum:08x}"
        ));
    }
    println!("CK_RESULT\t{checksum:08x}");
    Ok(())
}

#[cfg(all(feature = "dbt-code-audit", not(feature = "dbt-execution-profile")))]
fn export(_build_dir: &Path) -> Result<(), String> {
    Err("export requires --features dbt-code-audit,dbt-execution-profile".to_string())
}

#[cfg(all(feature = "dbt-code-audit", feature = "dbt-execution-profile"))]
fn export(build_dir: &Path) -> Result<(), String> {
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
                scratch_bytes: DEFAULT_DBT_SCRATCH_BYTES,
                cache_bytes: 128 * 1024,
                code_alignment: compukter_vm::rv32_machine::DEFAULT_DBT_CODE_ALIGNMENT,
            },
        },
    )
    .map_err(|error| error.to_string())?;
    let outcome = machine.run(20_100_000).map_err(|error| error.to_string())?;
    let checksum = match outcome {
        Rv32MachineOutcome::Halted { exit_code, .. } => exit_code as u32,
        other => return Err(format!("canonical audit machine did not halt: {other:?}")),
    };
    if checksum != EXPECTED_CHECKSUM {
        return Err(format!(
            "canonical audit checksum mismatch: expected {EXPECTED_CHECKSUM:08x}, actual {checksum:08x}"
        ));
    }
    let snapshot = machine
        .dbt_code_snapshot()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "canonical Cached DBT backend did not expose a snapshot".to_string())?;
    if snapshot.blocks.is_empty() || snapshot.used_bytes.is_empty() {
        return Err("canonical DBT snapshot is empty".to_string());
    }

    let mut profiled_machine = Rv32Machine::from_elf(
        &elf,
        Rv32MachineConfig {
            ram_size: PRODUCT_RAM_BYTES,
            debug_limit: 0,
            execution: Rv32ExecutionBackendConfig::CachedDbt {
                sets: 512,
                max_instructions: 16,
                scratch_bytes: DEFAULT_DBT_SCRATCH_BYTES,
                cache_bytes: 128 * 1024,
                code_alignment: compukter_vm::rv32_machine::DEFAULT_DBT_CODE_ALIGNMENT,
            },
        },
    )
    .map_err(|error| error.to_string())?;
    profiled_machine
        .enable_dbt_execution_profile(4096)
        .map_err(|error| error.to_string())?;
    let profiled_outcome = profiled_machine
        .run(20_100_000)
        .map_err(|error| error.to_string())?;
    let profiled_checksum = match profiled_outcome {
        Rv32MachineOutcome::Halted { exit_code, .. } => exit_code as u32,
        other => return Err(format!("profiled audit machine did not halt: {other:?}")),
    };
    if profiled_checksum != EXPECTED_CHECKSUM {
        return Err(format!(
            "profiled audit checksum mismatch: expected {EXPECTED_CHECKSUM:08x}, actual {profiled_checksum:08x}"
        ));
    }
    let profile = profiled_machine
        .dbt_execution_profile()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "profiled audit machine returned no execution profile".to_string())?;
    fs::write(
        build_dir.join("dbt-register-pressure.tsv"),
        register_pressure_report(&snapshot, &profile)?,
    )
    .map_err(|error| format!("failed to write register-pressure report: {error}"))?;

    let binary_path = build_dir.join("dbt-code-cache.bin");
    fs::write(&binary_path, &snapshot.used_bytes)
        .map_err(|error| format!("failed to write {}: {error}", binary_path.display()))?;
    fs::write(build_dir.join("dbt-blocks.tsv"), block_report(&snapshot))
        .map_err(|error| format!("failed to write DBT block report: {error}"))?;
    fs::write(build_dir.join("dbt-support.tsv"), support_report(&snapshot))
        .map_err(|error| format!("failed to write DBT support report: {error}"))?;
    fs::write(
        build_dir.join("dbt-code-cache.S"),
        assembly_wrapper(&snapshot, &binary_path)?,
    )
    .map_err(|error| format!("failed to write DBT assembly wrapper: {error}"))?;
    println!(
        "CK_CODEGEN_EXPORT\t{checksum:08x}\t{}\t{}\t{}",
        snapshot.generation,
        snapshot.blocks.len(),
        snapshot.used_bytes.len()
    );
    Ok(())
}

#[cfg(all(feature = "dbt-code-audit", feature = "dbt-execution-profile"))]
fn register_pressure_report(
    snapshot: &Rv32DbtCodeSnapshot,
    profile: &Rv32DbtExecutionProfile,
) -> Result<String, String> {
    if profile.counter_overflowed {
        return Err("register-pressure execution profile overflowed".to_string());
    }
    let mut static_totals = [0_u128; 10];
    let mut weighted_totals = [0_u128; 10];
    let mut executed_guest_instructions = 0_u128;
    let mut max_resident = 0_u8;
    let mut block_rows = String::new();

    for block in &snapshot.blocks {
        let executions = profile
            .blocks
            .iter()
            .find(|profile_block| profile_block.pc == block.guest_pc)
            .ok_or_else(|| {
                format!(
                    "live DBT block 0x{:08x} is missing from execution profile",
                    block.guest_pc
                )
            })?
            .executions;
        let pressure = block.register_pressure;
        let values = [
            pressure.entry_arch_loads,
            pressure.body_arch_loads,
            pressure.dirty_live_eviction_stores,
            pressure.dead_evictions,
            pressure.clean_evictions,
            pressure.loop_reconcile_stores,
            pressure.allocation_pressure,
            pressure.scratch_clobber_sites[0],
            pressure.scratch_clobber_sites[1],
            pressure.scratch_clobber_sites[2],
        ];
        for (index, value) in values.into_iter().enumerate() {
            static_totals[index] = static_totals[index]
                .checked_add(u128::from(value))
                .ok_or_else(|| "static register-pressure total overflowed".to_string())?;
            weighted_totals[index] = weighted_totals[index]
                .checked_add(u128::from(value) * u128::from(executions))
                .ok_or_else(|| "weighted register-pressure total overflowed".to_string())?;
        }
        executed_guest_instructions = executed_guest_instructions
            .checked_add(u128::from(block.guest_instruction_count) * u128::from(executions))
            .ok_or_else(|| "executed guest-instruction total overflowed".to_string())?;
        max_resident = max_resident.max(pressure.max_resident);
        block_rows.push_str(&format!(
            "0x{:08x}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            block.guest_pc,
            executions,
            block.guest_instruction_count,
            pressure.entry_arch_loads,
            pressure.body_arch_loads,
            pressure.dirty_live_eviction_stores,
            pressure.dead_evictions,
            pressure.clean_evictions,
            pressure.loop_reconcile_stores,
            pressure.allocation_pressure,
            pressure.max_resident,
            pressure.scratch_clobber_sites[0],
            pressure.scratch_clobber_sites[1],
            pressure.scratch_clobber_sites[2],
            block.length,
        ));
    }
    if executed_guest_instructions == 0 {
        return Err(
            "register-pressure profile contains no executed guest instructions".to_string(),
        );
    }

    let names = [
        "entry_arch_loads",
        "body_arch_loads",
        "dirty_live_eviction_stores",
        "dead_evictions",
        "clean_evictions",
        "loop_reconcile_stores",
        "allocation_pressure",
        "scratch_rax_sites",
        "scratch_rcx_sites",
        "scratch_rdx_sites",
    ];
    let mut output =
        String::from("metric\tstatic_total\tweighted_total\tper_million_guest_instructions\n");
    for (index, name) in names.into_iter().enumerate() {
        output.push_str(&format!(
            "{name}\t{}\t{}\t{:.6}\n",
            static_totals[index],
            weighted_totals[index],
            weighted_totals[index] as f64 * 1_000_000.0 / executed_guest_instructions as f64,
        ));
    }
    output.push_str(&format!("max_resident\t{max_resident}\t-\t-\n"));
    output.push_str(&format!(
        "executed_guest_instructions\t-\t{executed_guest_instructions}\t1000000.000000\n"
    ));
    output.push_str("\nblocks\nguest_pc\texecutions\tguest_instructions\tentry_arch_loads\tbody_arch_loads\tdirty_live_eviction_stores\tdead_evictions\tclean_evictions\tloop_reconcile_stores\tallocation_pressure\tmax_resident\tscratch_rax_sites\tscratch_rcx_sites\tscratch_rdx_sites\tcode_bytes\n");
    output.push_str(&block_rows);
    Ok(output)
}

#[cfg(feature = "dbt-code-audit")]
#[cfg_attr(not(feature = "dbt-execution-profile"), allow(dead_code))]
fn block_report(snapshot: &Rv32DbtCodeSnapshot) -> String {
    let mut output = String::from(
        "guest_pc\tgeneration\toffset\tlength\tchain_entry_offset\tguest_instructions\tedge_index\ttarget_pc\tdisplacement_offset\treset_target_offset\tlinked\n",
    );
    for block in &snapshot.blocks {
        if block.edges.is_empty() {
            output.push_str(&format!(
                "{:08x}\t{}\t{}\t{}\t{}\t{}\t-\t-\t-\t-\t-\n",
                block.guest_pc,
                block.generation,
                block.offset,
                block.length,
                block.chain_entry_offset,
                block.guest_instruction_count
            ));
        } else {
            for (index, edge) in block.edges.iter().enumerate() {
                output.push_str(&format!(
                    "{:08x}\t{}\t{}\t{}\t{}\t{}\t{}\t{:08x}\t{}\t{}\t{}\n",
                    block.guest_pc,
                    block.generation,
                    block.offset,
                    block.length,
                    block.chain_entry_offset,
                    block.guest_instruction_count,
                    index,
                    edge.target_pc,
                    edge.displacement_offset,
                    edge.reset_target_offset,
                    u8::from(edge.linked)
                ));
            }
        }
    }
    output
}

#[cfg(feature = "dbt-code-audit")]
#[cfg_attr(not(feature = "dbt-execution-profile"), allow(dead_code))]
fn support_report(snapshot: &Rv32DbtCodeSnapshot) -> String {
    let mut output = String::from("kind\toffset\tlength\n");
    for support in &snapshot.support_code {
        output.push_str(&format!(
            "{}\t{}\t{}\n",
            support_symbol(support.kind),
            support.offset,
            support.length
        ));
    }
    output
}

#[cfg(feature = "dbt-code-audit")]
fn assembly_wrapper(snapshot: &Rv32DbtCodeSnapshot, binary_path: &Path) -> Result<String, String> {
    let path = binary_path
        .canonicalize()
        .map_err(|error| format!("failed to canonicalize {}: {error}", binary_path.display()))?;
    let path = path
        .to_str()
        .ok_or_else(|| "DBT binary path is not UTF-8".to_string())?;
    if path.contains(['\n', '\r']) {
        return Err("DBT binary path contains a line break".to_string());
    }
    let path = path.replace('\\', "\\\\").replace('"', "\\\"");
    let mut output = String::from(".section .text.dbt,\"ax\",@progbits\n.p2align 4\n");
    let mut cursor = 0_usize;
    for support in &snapshot.support_code {
        let offset = support.offset as usize;
        let length = support.length as usize;
        if length == 0 || offset < cursor {
            return Err("DBT support-code ranges are empty, unsorted, or overlapping".to_string());
        }
        if offset > cursor {
            output.push_str(&format!(
                ".incbin \"{path}\", {cursor}, {}\n",
                offset - cursor
            ));
        }
        let symbol = support_symbol(support.kind);
        output.push_str(&format!(
            ".global {symbol}\n.type {symbol},@function\n{symbol}:\n.incbin \"{path}\", {offset}, {length}\n.size {symbol}, .-{symbol}\n"
        ));
        cursor = offset + length;
    }
    for block in &snapshot.blocks {
        let offset = block.offset as usize;
        let length = block.length as usize;
        if length == 0 || offset < cursor {
            return Err("DBT block ranges overlap support code or each other".to_string());
        }
        if offset > cursor {
            output.push_str(&format!(
                ".incbin \"{path}\", {cursor}, {}\n",
                offset - cursor
            ));
        }
        let symbol = format!("dbt_pc_{:08x}_off_{:08x}", block.guest_pc, block.offset);
        output.push_str(&format!(
            ".global {symbol}\n.type {symbol},@function\n{symbol}:\n.incbin \"{path}\", {offset}, {length}\n.size {symbol}, .-{symbol}\n"
        ));
        cursor = offset + length;
    }
    if cursor < snapshot.used_bytes.len() {
        output.push_str(&format!(
            ".incbin \"{path}\", {cursor}, {}\n",
            snapshot.used_bytes.len() - cursor
        ));
    }
    if cursor > snapshot.used_bytes.len() {
        return Err("DBT assembly ranges exceed snapshot bytes".to_string());
    }
    output.push_str(".section .note.GNU-stack,\"\",@progbits\n");
    Ok(output)
}

#[cfg(feature = "dbt-code-audit")]
const fn support_symbol(kind: Rv32DbtSupportCodeKind) -> &'static str {
    match kind {
        Rv32DbtSupportCodeKind::CompletedExitStub => "dbt_support_completed_exit_stub",
    }
}

#[cfg(feature = "dbt-code-audit")]
fn report(build_dir: &Path) -> Result<(), String> {
    let block_report = fs::read_to_string(build_dir.join("dbt-blocks.tsv"))
        .map_err(|error| format!("failed to read DBT block report: {error}"))?;
    let blocks = parse_block_report(&block_report)?;
    let support_report = fs::read_to_string(build_dir.join("dbt-support.tsv"))
        .map_err(|error| format!("failed to read DBT support report: {error}"))?;
    let (support_offset, support_length) = parse_support_report(&support_report)?;
    let dbt_disassembly = fs::read_to_string(build_dir.join("dbt-disassembly.txt"))
        .map_err(|error| format!("failed to read DBT disassembly: {error}"))?;
    let native_disassembly = fs::read_to_string(build_dir.join("native-analysis-disassembly.txt"))
        .map_err(|error| format!("failed to read native analysis disassembly: {error}"))?;
    let wasmtime_disassembly = fs::read_to_string(build_dir.join("wasmtime-aot-objdump.txt"))
        .map_err(|error| format!("failed to read Wasmtime disassembly: {error}"))?;

    let mut dbt_instructions = Vec::new();
    let support_instructions = parse_llvm_symbol_range(
        &dbt_disassembly,
        "dbt_support_completed_exit_stub",
        u64::from(support_offset),
        u64::from(support_length),
    )?;
    let support_code_bytes = u64::from(support_length);
    dbt_instructions.extend(support_instructions.iter().cloned());
    let mut hot_blocks = Vec::with_capacity(blocks.len());
    for block in &blocks {
        let symbol = format!("dbt_pc_{:08x}_off_{:08x}", block.guest_pc, block.offset);
        let instructions = parse_llvm_symbol_range(
            &dbt_disassembly,
            &symbol,
            u64::from(block.offset),
            u64::from(block.length),
        )?;
        hot_blocks.push((block.clone(), instructions.len() as u64));
        dbt_instructions.extend(instructions);
    }
    let native_instructions = parse_llvm_symbol(&native_disassembly, "benchmark_kernel")?;
    let wasmtime_instructions = parse_wasmtime_function(&wasmtime_disassembly, "benchmark_batch")?;

    hot_blocks.sort_by(|(lhs, lhs_host), (rhs, rhs_host)| {
        let lhs_ratio = *lhs_host as f64 / f64::from(lhs.guest_instructions);
        let rhs_ratio = *rhs_host as f64 / f64::from(rhs.guest_instructions);
        rhs_ratio
            .total_cmp(&lhs_ratio)
            .then_with(|| rhs_host.cmp(lhs_host))
            .then_with(|| lhs.offset.cmp(&rhs.offset))
    });

    let dbt_block_bytes = blocks
        .iter()
        .map(|block| u64::from(block.length))
        .sum::<u64>();
    let dbt_code_bytes = dbt_block_bytes
        .checked_add(support_code_bytes)
        .ok_or_else(|| "DBT resident byte total overflowed".to_string())?;
    let guest_instructions = blocks
        .iter()
        .map(|block| u64::from(block.guest_instructions))
        .sum();
    let linked_edges = blocks
        .iter()
        .map(|block| u64::from(block.linked_edges))
        .sum();
    let unlinked_edges = blocks
        .iter()
        .map(|block| u64::from(block.unlinked_edges))
        .sum();
    let mut block_sizes = blocks
        .iter()
        .map(|block| u64::from(block.length))
        .collect::<Vec<_>>();
    block_sizes.sort_unstable();
    let block_mean = dbt_block_bytes as f64 / blocks.len() as f64;

    let mut output = String::from(
        "system\tregion\tcode_bytes\thost_instructions\tguest_instructions\thost_per_guest\tbytes_per_guest\tlive_blocks\tblock_mean_bytes\tblock_p50_bytes\tblock_p95_bytes\tblock_max_bytes\tlinked_edges\tunlinked_edges\tmemory_operands\tmove\tconditional_branch\tunconditional_branch\tarithmetic_logical\tshift_rotate\tmultiply_divide\tcall_return\tvector\tother\n",
    );
    output.push_str(&metric_row(
        "rv32-cached-dbt",
        "live-resident-code",
        Some(dbt_code_bytes),
        &dbt_instructions,
        Some(guest_instructions),
        Some(blocks.len() as u64),
        Some(block_mean),
        Some(percentile(&block_sizes, 50)),
        Some(percentile(&block_sizes, 95)),
        block_sizes.last().copied(),
        Some(linked_edges),
        Some(unlinked_edges),
    ));
    output.push_str(&metric_row(
        "native-analysis-object",
        "benchmark_kernel-O3-no-LTO",
        Some(encoded_bytes(&native_instructions)?),
        &native_instructions,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    ));
    output.push_str(&metric_row(
        "wasmtime-aot",
        "benchmark_batch",
        None,
        &wasmtime_instructions,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    ));
    output.push_str("\ndbt_hot_blocks\n");
    output.push_str("rank\tguest_pc\toffset\tbytes\tguest_instructions\thost_instructions\thost_per_guest\tlinked_edges\tunlinked_edges\n");
    for (rank, (block, host_instructions)) in hot_blocks.iter().enumerate() {
        output.push_str(&format!(
            "{}\t{:08x}\t{}\t{}\t{}\t{}\t{:.6}\t{}\t{}\n",
            rank + 1,
            block.guest_pc,
            block.offset,
            block.length,
            block.guest_instructions,
            host_instructions,
            *host_instructions as f64 / f64::from(block.guest_instructions),
            block.linked_edges,
            block.unlinked_edges
        ));
    }
    output.push_str("\ndbt_support_code\n");
    output.push_str("kind\tbytes\thost_instructions\n");
    output.push_str(&format!(
        "completed-exit-stub\t{support_code_bytes}\t{}\n",
        support_instructions.len()
    ));
    fs::write(build_dir.join("codegen-report.tsv"), &output)
        .map_err(|error| format!("failed to write codegen report: {error}"))?;
    print!("{output}");
    Ok(())
}

#[cfg(feature = "dbt-code-audit")]
fn parse_block_report(input: &str) -> Result<Vec<AuditBlock>, String> {
    let mut blocks = Vec::<AuditBlock>::new();
    for line in input.lines().skip(1) {
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.len() != 11 {
            return Err(format!("invalid DBT block row: {line}"));
        }
        let guest_pc = u32::from_str_radix(columns[0], 16)
            .map_err(|error| format!("invalid guest PC: {error}"))?;
        let offset = columns[2]
            .parse::<u32>()
            .map_err(|error| format!("invalid block offset: {error}"))?;
        if blocks
            .last()
            .is_none_or(|block| block.guest_pc != guest_pc || block.offset != offset)
        {
            blocks.push(AuditBlock {
                guest_pc,
                offset,
                length: columns[3]
                    .parse()
                    .map_err(|error| format!("invalid block length: {error}"))?,
                guest_instructions: columns[5]
                    .parse()
                    .map_err(|error| format!("invalid guest instruction count: {error}"))?,
                linked_edges: 0,
                unlinked_edges: 0,
            });
        }
        if columns[10] == "1" {
            blocks.last_mut().unwrap().linked_edges += 1;
        } else if columns[10] == "0" {
            blocks.last_mut().unwrap().unlinked_edges += 1;
        } else if columns[10] != "-" {
            return Err(format!("invalid linked state: {}", columns[10]));
        }
    }
    if blocks.is_empty() {
        return Err("DBT block report contains no blocks".to_string());
    }
    if !blocks.windows(2).all(|pair| {
        pair[0]
            .offset
            .checked_add(pair[0].length)
            .is_some_and(|end| end <= pair[1].offset)
    }) {
        return Err("DBT block rows are unsorted or overlapping".to_string());
    }
    Ok(blocks)
}

#[cfg(feature = "dbt-code-audit")]
fn parse_support_report(input: &str) -> Result<(u32, u32), String> {
    let rows = input.lines().skip(1).collect::<Vec<_>>();
    if rows.len() != 1 {
        return Err("DBT support report must contain exactly one range".to_string());
    }
    let columns = rows[0].split('\t').collect::<Vec<_>>();
    if columns.len() != 3 || columns[0] != "dbt_support_completed_exit_stub" {
        return Err("invalid DBT support row".to_string());
    }
    let offset = columns[1]
        .parse::<u32>()
        .map_err(|error| format!("invalid DBT support offset: {error}"))?;
    let length = columns[2]
        .parse::<u32>()
        .map_err(|error| format!("invalid DBT support length: {error}"))?;
    if length == 0 {
        return Err("DBT support range is empty".to_string());
    }
    Ok((offset, length))
}

#[cfg(feature = "dbt-code-audit")]
fn metric_row(
    system: &str,
    region: &str,
    code_bytes: Option<u64>,
    instructions: &[DecodedHostInstruction],
    guest_instructions: Option<u64>,
    live_blocks: Option<u64>,
    block_mean: Option<f64>,
    block_p50: Option<u64>,
    block_p95: Option<u64>,
    block_max: Option<u64>,
    linked_edges: Option<u64>,
    unlinked_edges: Option<u64>,
) -> String {
    let counts = instruction_counts(instructions);
    let host = instructions.len() as u64;
    let host_per_guest = guest_instructions.map(|guest| host as f64 / guest as f64);
    let bytes_per_guest = code_bytes
        .zip(guest_instructions)
        .map(|(bytes, guest)| bytes as f64 / guest as f64);
    format!(
        "{system}\t{region}\t{}\t{host}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        option_u64(code_bytes),
        option_u64(guest_instructions),
        option_f64(host_per_guest),
        option_f64(bytes_per_guest),
        option_u64(live_blocks),
        option_f64(block_mean),
        option_u64(block_p50),
        option_u64(block_p95),
        option_u64(block_max),
        option_u64(linked_edges),
        option_u64(unlinked_edges),
        counts.memory_operands,
        counts.groups[0],
        counts.groups[1],
        counts.groups[2],
        counts.groups[3],
        counts.groups[4],
        counts.groups[5],
        counts.groups[6],
        counts.groups[7],
        counts.groups[8],
    )
}

#[cfg(feature = "dbt-code-audit")]
fn instruction_counts(instructions: &[DecodedHostInstruction]) -> InstructionCounts {
    let mut counts = InstructionCounts::default();
    for instruction in instructions {
        if has_x86_memory_operand(&instruction.operands) {
            counts.memory_operands += 1;
        }
        let index = match classify_x86_instruction(&instruction.mnemonic) {
            InstructionGroup::Move => 0,
            InstructionGroup::ConditionalBranch => 1,
            InstructionGroup::UnconditionalBranch => 2,
            InstructionGroup::ArithmeticLogical => 3,
            InstructionGroup::ShiftRotate => 4,
            InstructionGroup::MultiplyDivide => 5,
            InstructionGroup::CallReturn => 6,
            InstructionGroup::Vector => 7,
            InstructionGroup::Other => 8,
        };
        counts.groups[index] += 1;
    }
    counts
}

#[cfg(feature = "dbt-code-audit")]
fn encoded_bytes(instructions: &[DecodedHostInstruction]) -> Result<u64, String> {
    instructions.iter().try_fold(0_u64, |total, instruction| {
        instruction
            .encoded_bytes
            .ok_or_else(|| "LLVM instruction is missing encoded bytes".to_string())
            .and_then(|bytes| {
                total
                    .checked_add(bytes as u64)
                    .ok_or_else(|| "encoded byte total overflowed".to_string())
            })
    })
}

#[cfg(feature = "dbt-code-audit")]
fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let index = (sorted.len() * percentile).div_ceil(100).saturating_sub(1);
    sorted[index]
}

#[cfg(feature = "dbt-code-audit")]
fn option_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| value.to_string())
}

#[cfg(feature = "dbt-code-audit")]
fn option_f64(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| format!("{value:.6}"))
}

#[cfg(all(test, feature = "dbt-code-audit"))]
mod tests {
    #[cfg(feature = "dbt-execution-profile")]
    use super::register_pressure_report;
    use super::{
        assembly_wrapper, instruction_counts, metric_row, parse_block_report, parse_support_report,
    };
    use compukter_vm::benchmarks::{DecodedHostInstruction, InstructionGroup};
    use compukter_vm::rv32_machine::{
        Rv32DbtCodeBlock, Rv32DbtCodeSnapshot, Rv32DbtSupportCodeKind, Rv32DbtSupportCodeRange,
    };
    #[cfg(feature = "dbt-execution-profile")]
    use compukter_vm::rv32_machine::{
        Rv32DbtDynamicExitCounts, Rv32DbtExecutionProfile, Rv32DbtProfileBlock,
    };

    #[test]
    fn assembly_preserves_gaps_and_live_block_ranges() {
        let directory = std::env::temp_dir();
        let binary = directory.join("compukter-codegen-wrapper.bin");
        std::fs::write(&binary, [0_u8; 12]).unwrap();
        let snapshot = Rv32DbtCodeSnapshot {
            generation: 0,
            used_bytes: vec![0; 12],
            support_code: vec![Rv32DbtSupportCodeRange {
                kind: Rv32DbtSupportCodeKind::CompletedExitStub,
                offset: 0,
                length: 2,
            }],
            blocks: vec![Rv32DbtCodeBlock {
                guest_pc: 0x1000,
                generation: 0,
                offset: 4,
                length: 4,
                chain_entry_offset: 4,
                guest_instruction_count: 1,
                register_pressure: Default::default(),
                edges: Vec::new(),
            }],
        };
        let assembly = assembly_wrapper(&snapshot, &binary).unwrap();
        assert!(assembly.contains("dbt_support_completed_exit_stub"));
        assert!(assembly.contains(", 0, 2\n"));
        assert!(assembly.contains(", 2, 2\n"));
        assert!(assembly.contains(", 4, 4\n"));
        assert!(assembly.contains(", 8, 4\n"));
    }

    #[test]
    fn report_counts_every_instruction_once_and_rejects_overlaps() {
        let instructions = vec![
            DecodedHostInstruction {
                address: 0,
                encoded_bytes: Some(2),
                mnemonic: "movl".to_string(),
                operands: "%eax, (%rdi)".to_string(),
            },
            DecodedHostInstruction {
                address: 2,
                encoded_bytes: Some(1),
                mnemonic: "retq".to_string(),
                operands: String::new(),
            },
        ];
        let counts = instruction_counts(&instructions);
        assert_eq!(counts.groups.iter().sum::<u64>(), 2);
        assert_eq!(counts.groups[InstructionGroup::Move as usize], 1);
        assert_eq!(counts.memory_operands, 1);
        assert_eq!(
            metric_row(
                "x",
                "y",
                Some(3),
                &instructions,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None
            )
            .split('\t')
            .count(),
            24
        );

        let overlap = "header\n00001000\t0\t0\t8\t0\t1\t-\t-\t-\t-\t-\n00001004\t0\t4\t8\t4\t1\t-\t-\t-\t-\t-\n";
        assert!(parse_block_report(overlap).is_err());
    }

    #[test]
    fn support_report_requires_one_exact_nonempty_range() {
        assert_eq!(
            parse_support_report("kind\toffset\tlength\ndbt_support_completed_exit_stub\t0\t79\n"),
            Ok((0, 79))
        );
        assert!(parse_support_report(
            "kind\toffset\tlength\ndbt_support_completed_exit_stub\t0\t0\n"
        )
        .is_err());
        assert!(parse_support_report("kind\toffset\tlength\n").is_err());
    }

    #[cfg(feature = "dbt-execution-profile")]
    #[test]
    fn register_pressure_report_weights_translation_events_by_executions() {
        let snapshot = Rv32DbtCodeSnapshot {
            generation: 0,
            used_bytes: vec![0; 8],
            support_code: Vec::new(),
            blocks: vec![Rv32DbtCodeBlock {
                guest_pc: 0x1000,
                generation: 0,
                offset: 0,
                length: 8,
                chain_entry_offset: 0,
                guest_instruction_count: 4,
                register_pressure: compukter_vm::rv32_machine::Rv32DbtRegisterPressure {
                    body_arch_loads: 2,
                    dirty_live_eviction_stores: 1,
                    scratch_clobber_sites: [3, 0, 1],
                    max_resident: 7,
                    ..Default::default()
                },
                edges: Vec::new(),
            }],
        };
        let profile = Rv32DbtExecutionProfile {
            blocks: vec![Rv32DbtProfileBlock {
                pc: 0x1000,
                executions: 5,
            }],
            static_edges: Vec::new(),
            dynamic_exits: Rv32DbtDynamicExitCounts::default(),
            capacity: 16,
            used_records: 1,
            retained_bytes: 16,
            counter_overflowed: false,
        };

        let report = register_pressure_report(&snapshot, &profile).unwrap();

        assert!(report.contains("body_arch_loads\t2\t10\t500000.000000\n"));
        assert!(report.contains("dirty_live_eviction_stores\t1\t5\t250000.000000\n"));
        assert!(report.contains("scratch_rax_sites\t3\t15\t750000.000000\n"));
        assert!(report.contains("max_resident\t7\t-\t-\n"));
    }
}
