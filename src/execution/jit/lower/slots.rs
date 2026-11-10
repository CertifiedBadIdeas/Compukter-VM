/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */
//! Invocation-local typed SSA with exact frame materialization on every exit.
use super::*;
use cranelift_codegen::ir::Block;
use cranelift_frontend::Variable;
use std::collections::BTreeMap;

const MAX_CELLS: usize = 64;

// A cold benchmark-only control; no per-session setting or hot-path test.
#[cfg(feature = "guest-benchmark")]
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

#[cfg(feature = "guest-benchmark")]
pub(in crate::execution) fn benchmark_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

struct Cell {
    offset: u32,
    ty: Type,
    write: bool,
    uses: usize,
    computational: bool,
}

struct Slot {
    cell: Cell,
    variable: Variable,
}

pub(super) struct Cache {
    slots: Vec<Slot>,
    pub exit: Block,
}

fn eligible(plan: &Plan) -> Vec<Cell> {
    if plan.region.is_empty() || plan.has_heap {
        return Vec::new();
    }
    let mut cells = BTreeMap::<(u32, u32), Cell>::new();
    for &op in &plan.ops {
        // Copy-only temporaries make every side exit carry more values without
        // removing arithmetic frame traffic. Keep them memory-backed.
        let computational = numeric_type(op.opcode).is_some()
            || matches!(op.opcode, BRANCH | COPY_BOOL | COPY_CHAR);
        for access in access::footprint(op, &plan.copies).expect("verified frame footprint") {
            let cell = cells
                .entry((access.offset, access.ty.bytes()))
                .or_insert(Cell {
                    offset: access.offset,
                    ty: access.ty,
                    write: false,
                    uses: 0,
                    computational: false,
                });
            if access.ty.is_float() {
                cell.ty = access.ty;
            }
            cell.write |= access.write;
            cell.uses += 1;
            cell.computational |= computational;
        }
    }
    let ranges: Vec<_> = cells.keys().copied().collect();
    let mut cells: Vec<_> = cells
        .into_iter()
        .filter_map(|((offset, width), cell)| {
            // Every partially overlapping access remains memory-backed. Exact-range
            // typed aliases share one variable and lossless bitcasts instead.
            (cell.computational
                && !ranges.iter().any(|&(other, size)| {
                    (other, size) != (offset, width)
                        && offset < other + size
                        && other < offset + width
                }))
            .then_some(cell)
        })
        .collect();
    cells.sort_by_key(|cell| (std::cmp::Reverse(cell.uses), cell.offset));
    cells.truncate(MAX_CELLS);
    cells
}

impl Cache {
    pub(super) fn new(plan: &Plan, b: &mut FunctionBuilder<'_>, base: Value) -> Option<Self> {
        #[cfg(feature = "guest-benchmark")]
        if !ENABLED.load(Ordering::Relaxed) {
            return None;
        }
        let cells = eligible(plan);
        if cells.is_empty() {
            return None;
        }
        let exit = b.create_block();
        b.append_block_param(exit, types::I64);
        let slots = cells
            .into_iter()
            .map(|cell| {
                let variable = b.declare_var(cell.ty);
                let value = load(b, base, cell.ty, cell.offset);
                b.def_var(variable, value);
                Slot { cell, variable }
            })
            .collect();
        Some(Self { slots, exit })
    }

    fn get(&self, offset: u32, width: u32) -> Option<&Slot> {
        self.slots
            .iter()
            .find(|slot| slot.cell.offset == offset && slot.cell.ty.bytes() == width)
    }

    pub(super) fn finish(&self, b: &mut FunctionBuilder<'_>, base: Value) {
        b.switch_to_block(self.exit);
        let status = b.block_params(self.exit)[0];
        for slot in &self.slots {
            if slot.cell.write {
                let value = b.use_var(slot.variable);
                store(b, base, slot.cell.offset, value);
            }
        }
        b.ins().return_(&[status]);
    }
}

fn cast(b: &mut FunctionBuilder<'_>, value: Value, ty: Type) -> Value {
    if b.func.dfg.value_type(value) == ty {
        value
    } else {
        b.ins().bitcast(ty, MemFlagsData::new(), value)
    }
}

pub(super) fn read(
    b: &mut FunctionBuilder<'_>,
    base: Value,
    cache: Option<&Cache>,
    ty: Type,
    offset: u32,
) -> Value {
    if let Some(slot) = cache.and_then(|cache| cache.get(offset, ty.bytes())) {
        let value = b.use_var(slot.variable);
        cast(b, value, ty)
    } else {
        load(b, base, ty, offset)
    }
}

pub(super) fn write(
    b: &mut FunctionBuilder<'_>,
    base: Value,
    cache: Option<&Cache>,
    offset: u32,
    value: Value,
) {
    if let Some(slot) =
        cache.and_then(|cache| cache.get(offset, b.func.dfg.value_type(value).bytes()))
    {
        let value = cast(b, value, slot.cell.ty);
        b.def_var(slot.variable, value);
    } else {
        store(b, base, offset, value);
    }
}
