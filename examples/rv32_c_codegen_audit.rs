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
use compukter_vm::benchmarks::PRODUCT_RAM_BYTES;
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
    let command = args
        .next()
        .ok_or_else(|| "usage: rv32_c_codegen_audit export BUILD_DIR".to_string())?;
    let build_dir = PathBuf::from(
        args.next()
            .ok_or_else(|| "usage: rv32_c_codegen_audit export BUILD_DIR".to_string())?,
    );
    if args.next().is_some() || command != "export" {
        return Err("usage: rv32_c_codegen_audit export BUILD_DIR".to_string());
    }
    export(&build_dir)
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

#[cfg(all(test, feature = "dbt-code-audit"))]
mod tests {
    use super::assembly_wrapper;
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
}
