/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */
//! One frame footprint for range admission and scalar promotion analysis.
use super::*;

pub(super) struct Access {
    pub offset: u32,
    pub ty: Type,
    pub write: bool,
}

pub(super) fn footprint(op: Operation, copies: &[Copy]) -> Option<Vec<Access>> {
    let mut accesses = Vec::new();
    let mut add = |offset, ty, write| accesses.push(Access { offset, ty, write });
    if let Some(ty) = numeric_type(op.opcode) {
        add(op.lhs, ty, false);
        if !(21..=28).contains(&op.opcode) {
            add(
                op.rhs,
                if matches!(op.opcode, 134 | 138 | 142) {
                    types::I32
                } else {
                    ty
                },
                false,
            );
        }
        add(
            op.dst,
            if (29..=74).contains(&op.opcode) {
                types::I32
            } else {
                ty
            },
            true,
        );
    } else if let Some(kind) = heap_kind(op.opcode) {
        add(op.lhs, types::I32, false);
        if matches!(op.opcode & 0xffff, 217..=231) {
            add(op.rhs, types::I32, false);
        }
        add(
            op.dst,
            if matches!(kind, 2 | 4) {
                types::I64
            } else {
                types::I32
            },
            !matches!(op.opcode & 0xffff, 209..=215 | 225..=231),
        );
    } else {
        match op.opcode {
            COPY32 | COPY_BOOL | COPY_CHAR | COPY64 => {
                let ty = if op.opcode == COPY64 {
                    types::I64
                } else {
                    types::I32
                };
                add(op.lhs, ty, false);
                add(op.dst, ty, true);
            }
            IMMEDIATE32 | IMMEDIATE64 => {
                add(
                    op.dst,
                    if op.opcode == IMMEDIATE64 {
                        types::I64
                    } else {
                        types::I32
                    },
                    true,
                );
            }
            COPIES => {
                for copy in copies.get(op.dst as usize..op.lhs as usize)? {
                    let ty = match copy.width {
                        4 => types::I32,
                        8 => types::I64,
                        _ => return None,
                    };
                    add(copy.src, ty, false);
                    add(copy.dst, ty, true);
                }
            }
            BRANCH => add(op.dst, types::I32, false),
            JUMP | NOP => {}
            _ => return None,
        }
    }
    Some(accesses)
}
