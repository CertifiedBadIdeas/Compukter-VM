/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use super::emitter::{EmitError, Gpr, Mem, X64Emitter};
use crate::rv32_dbt::abi::{DbtContext, DbtExitRecord, DbtExitTag};
use crate::rv32im::Rv32ArchitecturalState;

const STUB_CAPACITY: usize = 128;

pub(crate) fn build_completed_exit_stub() -> Result<Vec<u8>, EmitError> {
    let mut out = X64Emitter::new(STUB_CAPACITY, 1)?;
    write_u32(
        &mut out,
        Gpr::R14,
        Rv32ArchitecturalState::register_offset(0),
        0,
    )?;
    out.mov_m32_r32(
        Mem::base_disp(Gpr::R14, Rv32ArchitecturalState::PC_OFFSET as i32),
        Gpr::Rdx,
    )?;
    out.mov_m32_r32(
        Mem::base_disp(
            Gpr::R15,
            (DbtContext::EXIT_OFFSET + DbtExitRecord::NEXT_PC_OFFSET) as i32,
        ),
        Gpr::Rdx,
    )?;
    out.mov_r32_m32(
        Gpr::Rax,
        Mem::base_disp(Gpr::R15, DbtContext::REMAINING_BUDGET_OFFSET as i32),
    )?;
    out.add_r64_r64(Gpr::Rax, Gpr::Rdi)?;
    out.mov_m32_r32(
        Mem::base_disp(
            Gpr::R15,
            (DbtContext::EXIT_OFFSET + DbtExitRecord::ATTEMPTED_OFFSET) as i32,
        ),
        Gpr::Rax,
    )?;
    for offset in [
        DbtExitRecord::INSTRUCTION_PC_OFFSET,
        DbtExitRecord::INSTRUCTION_WORD_OFFSET,
        DbtExitRecord::ADDRESS_OFFSET,
        DbtExitRecord::ACCESS_SIZE_OFFSET,
    ] {
        write_u32(&mut out, Gpr::R15, DbtContext::EXIT_OFFSET + offset, 0)?;
    }
    for register in [Gpr::R15, Gpr::R14, Gpr::R13, Gpr::R12, Gpr::Rbp, Gpr::Rbx] {
        out.pop(register)?;
    }
    out.mov_r32_imm32(Gpr::Rax, DbtExitTag::Completed as u32)?;
    out.ret()?;
    Ok(out.finish()?.to_vec())
}

fn write_u32(out: &mut X64Emitter, base: Gpr, offset: usize, value: u32) -> Result<(), EmitError> {
    out.mov_r32_imm32(Gpr::Rax, value)?;
    out.mov_m32_r32(Mem::base_disp(base, offset as i32), Gpr::Rax)
}

#[cfg(test)]
mod tests {
    use super::build_completed_exit_stub;

    #[test]
    fn completed_exit_stub_is_small_and_ends_in_ret() {
        let code = build_completed_exit_stub().unwrap();
        assert!(code.len() < 96);
        assert_eq!(code.last(), Some(&0xc3));
    }
}
