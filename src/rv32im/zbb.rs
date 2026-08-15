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

#![allow(
    dead_code,
    reason = "Zbb decoder and execution integration follows in issue #18"
)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZbbOp {
    Andn,
    Orn,
    Xnor,
    Clz,
    Ctz,
    Cpop,
    Min,
    Minu,
    Max,
    Maxu,
    SextB,
    SextH,
    ZextH,
    Rol,
    Ror,
    Rori,
    OrcB,
    Rev8,
}

pub(crate) fn execute_zbb(op: ZbbOp, lhs: u32, operand: u32) -> u32 {
    match op {
        ZbbOp::Andn => lhs & !operand,
        ZbbOp::Orn => lhs | !operand,
        ZbbOp::Xnor => !(lhs ^ operand),
        ZbbOp::Clz => lhs.leading_zeros(),
        ZbbOp::Ctz => lhs.trailing_zeros(),
        ZbbOp::Cpop => lhs.count_ones(),
        ZbbOp::Min => (lhs as i32).min(operand as i32) as u32,
        ZbbOp::Minu => lhs.min(operand),
        ZbbOp::Max => (lhs as i32).max(operand as i32) as u32,
        ZbbOp::Maxu => lhs.max(operand),
        ZbbOp::SextB => lhs as u8 as i8 as i32 as u32,
        ZbbOp::SextH => lhs as u16 as i16 as i32 as u32,
        ZbbOp::ZextH => lhs & 0xffff,
        ZbbOp::Rol => lhs.rotate_left(operand & 31),
        ZbbOp::Ror | ZbbOp::Rori => lhs.rotate_right(operand & 31),
        ZbbOp::OrcB => {
            let mut result = 0;
            for shift in [0, 8, 16, 24] {
                if lhs >> shift & 0xff != 0 {
                    result |= 0xff << shift;
                }
            }
            result
        }
        ZbbOp::Rev8 => lhs.swap_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::{execute_zbb, ZbbOp};
    use crate::rv32im::encoding::{andn, clz, rev8, rori};

    #[test]
    fn canonical_semantics_cover_every_rv32_zbb_operation() {
        let cases = [
            (ZbbOp::Andn, 0xaaaa_5555, 0x0f0f_f0f0, 0xa0a0_0505),
            (ZbbOp::Orn, 0xaaaa_5555, 0x0f0f_f0f0, 0xfafa_5f5f),
            (ZbbOp::Xnor, 0xaaaa_5555, 0x0f0f_f0f0, 0x5a5a_5a5a),
            (ZbbOp::Clz, 0, 0, 32),
            (ZbbOp::Clz, 0x0000_0100, 0, 23),
            (ZbbOp::Ctz, 0, 0, 32),
            (ZbbOp::Ctz, 0x0000_0100, 0, 8),
            (ZbbOp::Cpop, 0x0000_f0f0, 0, 8),
            (ZbbOp::Min, 0x8000_0000, 1, 0x8000_0000),
            (ZbbOp::Minu, 0x8000_0000, 1, 1),
            (ZbbOp::Max, 0x8000_0000, 1, 1),
            (ZbbOp::Maxu, 0x8000_0000, 1, 0x8000_0000),
            (ZbbOp::SextB, 0x0000_0080, 0, 0xffff_ff80),
            (ZbbOp::SextH, 0x0000_8000, 0, 0xffff_8000),
            (ZbbOp::ZextH, 0xffff_8001, 0, 0x0000_8001),
            (ZbbOp::Rol, 0x8000_0001, 1, 0x0000_0003),
            (ZbbOp::Rol, 0x8000_0001, 33, 0x0000_0003),
            (ZbbOp::Ror, 0x8000_0001, 1, 0xc000_0000),
            (ZbbOp::Rori, 0x8000_0001, 1, 0xc000_0000),
            (ZbbOp::OrcB, 0x0001_8000, 0, 0x00ff_ff00),
            (ZbbOp::Rev8, 0x1234_5678, 0, 0x7856_3412),
        ];

        for (op, lhs, operand, expected) in cases {
            assert_eq!(execute_zbb(op, lhs, operand), expected, "{op:?}");
        }
    }

    #[test]
    fn encoding_helpers_emit_ratified_words() {
        assert_eq!(andn(3, 1, 2), 0x4020_f1b3);
        assert_eq!(clz(3, 1), 0x6000_9193);
        assert_eq!(rori(3, 1, 31), 0x61f0_d193);
        assert_eq!(rev8(3, 1), 0x6980_d193);
    }
}
