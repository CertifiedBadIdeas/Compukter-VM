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
    classify_x86_instruction, has_x86_memory_operand, parse_llvm_symbol, parse_wasmtime_function,
    DecodedHostInstruction, InstructionGroup, PRODUCT_RAM_BYTES,
};
#[cfg(feature = "dbt-code-audit")]
use compukter_vm::rv32_machine::{
    Rv32DbtCodeSnapshot, Rv32ExecutionBackendConfig, Rv32Machine, Rv32MachineConfig,
    Rv32MachineOutcome, DEFAULT_DBT_SCRATCH_BYTES,
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

#[cfg(feature = "dbt-code-audit")]
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

    let binary_path = build_dir.join("dbt-code-cache.bin");
    fs::write(&binary_path, &snapshot.used_bytes)
        .map_err(|error| format!("failed to write {}: {error}", binary_path.display()))?;
    fs::write(build_dir.join("dbt-blocks.tsv"), block_report(&snapshot))
        .map_err(|error| format!("failed to write DBT block report: {error}"))?;
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

#[cfg(feature = "dbt-code-audit")]
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
    for block in &snapshot.blocks {
        let offset = block.offset as usize;
        let length = block.length as usize;
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
fn report(build_dir: &Path) -> Result<(), String> {
    let block_report = fs::read_to_string(build_dir.join("dbt-blocks.tsv"))
        .map_err(|error| format!("failed to read DBT block report: {error}"))?;
    let blocks = parse_block_report(&block_report)?;
    let dbt_disassembly = fs::read_to_string(build_dir.join("dbt-disassembly.txt"))
        .map_err(|error| format!("failed to read DBT disassembly: {error}"))?;
    let native_disassembly = fs::read_to_string(build_dir.join("native-analysis-disassembly.txt"))
        .map_err(|error| format!("failed to read native analysis disassembly: {error}"))?;
    let wasmtime_disassembly = fs::read_to_string(build_dir.join("wasmtime-aot-objdump.txt"))
        .map_err(|error| format!("failed to read Wasmtime disassembly: {error}"))?;

    let mut dbt_instructions = Vec::new();
    let mut hot_blocks = Vec::with_capacity(blocks.len());
    for block in &blocks {
        let symbol = format!("dbt_pc_{:08x}_off_{:08x}", block.guest_pc, block.offset);
        let instructions = parse_llvm_symbol(&dbt_disassembly, &symbol)?;
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

    let dbt_code_bytes = blocks.iter().map(|block| u64::from(block.length)).sum();
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
    let block_mean = dbt_code_bytes as f64 / blocks.len() as f64;

    let mut output = String::from(
        "system\tregion\tcode_bytes\thost_instructions\tguest_instructions\thost_per_guest\tbytes_per_guest\tlive_blocks\tblock_mean_bytes\tblock_p50_bytes\tblock_p95_bytes\tblock_max_bytes\tlinked_edges\tunlinked_edges\tmemory_operands\tmove\tconditional_branch\tunconditional_branch\tarithmetic_logical\tshift_rotate\tmultiply_divide\tcall_return\tvector\tother\n",
    );
    output.push_str(&metric_row(
        "rv32-cached-dbt",
        "live-resident-blocks",
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
    use super::{assembly_wrapper, instruction_counts, metric_row, parse_block_report};
    use compukter_vm::benchmarks::{DecodedHostInstruction, InstructionGroup};
    use compukter_vm::rv32_machine::{Rv32DbtCodeBlock, Rv32DbtCodeSnapshot};

    #[test]
    fn assembly_preserves_gaps_and_live_block_ranges() {
        let directory = std::env::temp_dir();
        let binary = directory.join("compukter-codegen-wrapper.bin");
        std::fs::write(&binary, [0_u8; 12]).unwrap();
        let snapshot = Rv32DbtCodeSnapshot {
            generation: 0,
            used_bytes: vec![0; 12],
            blocks: vec![Rv32DbtCodeBlock {
                guest_pc: 0x1000,
                generation: 0,
                offset: 4,
                length: 4,
                chain_entry_offset: 4,
                guest_instruction_count: 1,
                edges: Vec::new(),
            }],
        };
        let assembly = assembly_wrapper(&snapshot, &binary).unwrap();
        assert!(assembly.contains(", 0, 4\n"));
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
}
