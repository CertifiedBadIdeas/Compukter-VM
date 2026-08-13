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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedHostInstruction {
    pub address: u64,
    pub encoded_bytes: Option<usize>,
    pub mnemonic: String,
    pub operands: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum InstructionGroup {
    Move,
    ConditionalBranch,
    UnconditionalBranch,
    ArithmeticLogical,
    ShiftRotate,
    MultiplyDivide,
    CallReturn,
    Vector,
    Other,
}

pub fn parse_llvm_symbol(input: &str, symbol: &str) -> Result<Vec<DecodedHostInstruction>, String> {
    let mut matches = Vec::new();
    let lines = input.lines().collect::<Vec<_>>();
    let mut index = 0;
    while index < lines.len() {
        if llvm_symbol_name(lines[index]).is_some_and(|name| name == symbol) {
            let mut instructions = Vec::new();
            index += 1;
            while index < lines.len() && llvm_symbol_name(lines[index]).is_none() {
                if let Some(instruction) = parse_llvm_instruction(lines[index])? {
                    instructions.push(instruction);
                }
                index += 1;
            }
            matches.push(instructions);
            continue;
        }
        index += 1;
    }
    unique_nonempty_region(matches, "LLVM", symbol)
}

pub fn parse_wasmtime_function(
    input: &str,
    function: &str,
) -> Result<Vec<DecodedHostInstruction>, String> {
    let suffix = format!("::{function}:");
    let mut matches = Vec::new();
    let lines = input.lines().collect::<Vec<_>>();
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed.starts_with("wasm[") && trimmed.ends_with(&suffix) {
            let mut instructions = Vec::new();
            index += 1;
            while index < lines.len() {
                let line = lines[index].trim();
                if line.starts_with("wasm[") && line.ends_with(':') {
                    break;
                }
                if !line.is_empty() && !line.starts_with('╰') {
                    let (mnemonic, operands) = line
                        .split_once(char::is_whitespace)
                        .map_or((line, ""), |(mnemonic, operands)| {
                            (mnemonic, operands.trim())
                        });
                    instructions.push(DecodedHostInstruction {
                        address: instructions.len() as u64,
                        encoded_bytes: None,
                        mnemonic: mnemonic.to_ascii_lowercase(),
                        operands: operands.to_string(),
                    });
                }
                index += 1;
            }
            matches.push(instructions);
            continue;
        }
        index += 1;
    }
    unique_nonempty_region(matches, "Wasmtime", function)
}

pub fn classify_x86_instruction(mnemonic: &str) -> InstructionGroup {
    let mnemonic = mnemonic.to_ascii_lowercase();
    if mnemonic.starts_with('v') || mnemonic.starts_with("xmm") {
        return InstructionGroup::Vector;
    }
    match mnemonic.as_str() {
        "mov" | "movb" | "movw" | "movl" | "movq" | "movabsq" | "movzx" | "movzbl" | "movzwl"
        | "movsbl" | "movswl" | "movslq" | "lea" | "leal" | "leaq" | "push" | "pushq" | "pop"
        | "popq" => InstructionGroup::Move,
        "je" | "jne" | "ja" | "jae" | "jb" | "jbe" | "jg" | "jge" | "jl" | "jle" | "js" | "jns"
        | "jo" | "jno" | "jp" | "jnp" | "jz" | "jnz" => InstructionGroup::ConditionalBranch,
        "jmp" | "jmpq" => InstructionGroup::UnconditionalBranch,
        "add" | "addl" | "addq" | "sub" | "subl" | "subq" | "and" | "andl" | "andq" | "or"
        | "orl" | "orq" | "xor" | "xorl" | "xorq" | "neg" | "negl" | "negq" | "cmp" | "cmpb"
        | "cmpl" | "cmpq" | "test" | "testb" | "testl" | "testq" | "inc" | "incl" | "incq"
        | "dec" | "decl" | "decq" => InstructionGroup::ArithmeticLogical,
        "shl" | "shll" | "shlq" | "shr" | "shrl" | "shrq" | "sar" | "sarl" | "sarq" | "rol"
        | "roll" | "rolq" | "ror" | "rorl" | "rorq" => InstructionGroup::ShiftRotate,
        "mul" | "mull" | "mulq" | "imul" | "imull" | "imulq" | "div" | "divl" | "divq" | "idiv"
        | "idivl" | "idivq" => InstructionGroup::MultiplyDivide,
        "call" | "callq" | "ret" | "retq" => InstructionGroup::CallReturn,
        _ => InstructionGroup::Other,
    }
}

pub fn has_x86_memory_operand(operands: &str) -> bool {
    operands.contains('(') && operands.contains(')')
}

fn llvm_symbol_name(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let start = trimmed.find('<')? + 1;
    let end = trimmed.strip_suffix(":")?.rfind('>')?;
    (start < end).then_some(&trimmed[start..end])
}

fn parse_llvm_instruction(line: &str) -> Result<Option<DecodedHostInstruction>, String> {
    let trimmed = line.trim();
    let Some((address, rest)) = trimmed.split_once(':') else {
        return Ok(None);
    };
    if address.is_empty() || !address.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let address = u64::from_str_radix(address, 16)
        .map_err(|error| format!("invalid LLVM instruction address {address}: {error}"))?;
    let fields = rest.split_whitespace().collect::<Vec<_>>();
    let byte_count = fields
        .iter()
        .take_while(|field| field.len() == 2 && field.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .count();
    if byte_count == 0 || byte_count == fields.len() {
        return Ok(None);
    }
    let mnemonic = fields[byte_count].to_ascii_lowercase();
    let operands = fields[byte_count + 1..].join(" ");
    Ok(Some(DecodedHostInstruction {
        address,
        encoded_bytes: Some(byte_count),
        mnemonic,
        operands,
    }))
}

fn unique_nonempty_region(
    mut matches: Vec<Vec<DecodedHostInstruction>>,
    format: &str,
    name: &str,
) -> Result<Vec<DecodedHostInstruction>, String> {
    if matches.len() != 1 {
        return Err(format!(
            "expected exactly one {format} region named {name}, found {}",
            matches.len()
        ));
    }
    let region = matches.pop().unwrap();
    if region.is_empty() {
        return Err(format!("{format} region {name} contains no instructions"));
    }
    Ok(region)
}
