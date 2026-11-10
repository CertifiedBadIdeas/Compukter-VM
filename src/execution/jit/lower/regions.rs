/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */
//! Same-function CFG plans and invocation-local deterministic accounting.
use super::*;
use crate::execution::image::ExecutionImage;
use cranelift_codegen::ir::Block;

#[derive(Debug)]
pub(super) struct RegionBlock {
    id: usize,
    start: usize,
    end: usize,
    cost: u32,
}

impl Plan {
    pub(in super::super) fn region(image: &ExecutionImage, entry: usize) -> Option<Self> {
        let function = image.block(entry)?.function;
        let mut result = Self {
            region: Vec::new(),
            ops: Vec::new(),
            copies: Vec::new(),
            required_bytes: 0,
            has_heap: false,
            #[cfg(test)]
            writes: Vec::new(),
        };
        let mut pending = vec![entry];
        let mut seen = Vec::new();
        while let Some(id) = pending.pop() {
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            if seen.len() > 16 || result.ops.len() == MAX_OPS {
                break;
            }
            let block = image.block(id)?;
            if block.function != function {
                continue;
            }
            let Some(mut prefix) = Self::prefix(
                image.code(),
                block.code_start,
                block.instruction_count.min(MAX_OPS - result.ops.len()),
            ) else {
                continue;
            };
            if result.copies.len() + prefix.copies.len() > MAX_OPS {
                continue;
            }
            for op in &mut prefix.ops {
                if op.opcode == COPIES {
                    op.dst += result.copies.len() as u32;
                    op.lhs += result.copies.len() as u32;
                }
            }
            let last = *prefix.ops.last()?;
            if last.opcode == JUMP {
                pending.push(last.dst as usize);
            }
            if last.opcode == BRANCH {
                pending.extend([last.rhs as usize, last.lhs as usize]);
            }
            result.region.push(RegionBlock {
                id,
                start: result.ops.len(),
                end: result.ops.len() + prefix.ops.len(),
                cost: block.fixed_cost,
            });
            result.ops.extend(prefix.ops);
            result.copies.extend(prefix.copies);
            result.required_bytes = result.required_bytes.max(prefix.required_bytes);
            result.has_heap |= prefix.has_heap;
            #[cfg(test)]
            result.writes.extend(prefix.writes);
        }
        // Never displace the prefix tier without at least one connected edge.
        let connected = result.region.iter().any(|block| {
            let last = result.ops[block.end - 1];
            let targets = match last.opcode {
                JUMP => [last.dst, last.dst],
                BRANCH => [last.lhs, last.rhs],
                _ => return false,
            };
            targets
                .iter()
                .any(|target| result.region.iter().any(|b| b.id == *target as usize))
        });
        (result.region.first()?.id == entry && connected && result.worth_compiling())
            .then_some(result)
    }
    // Native region entry/accounting has a fixed cost. Avoid short acyclic
    // bridges before lifecycle operations: they add native invocations without
    // amortizing that cost. Cycles amortize across iterations; acyclic plans must
    // remove at least two dispatcher transitions on every path to an exit.
    fn worth_compiling(&self) -> bool {
        let successors = |index: usize| {
            let last = self.ops[self.region[index].end - 1];
            let targets = match last.opcode {
                JUMP => vec![last.dst as usize],
                BRANCH => vec![last.lhs as usize, last.rhs as usize],
                _ => return vec![None],
            };
            targets
                .into_iter()
                .map(|target| self.region.iter().position(|block| block.id == target))
                .collect::<Vec<_>>()
        };
        let edges: Vec<_> = (0..self.region.len()).map(successors).collect();
        fn cyclic(index: usize, edges: &[Vec<Option<usize>>], colors: &mut [u8]) -> bool {
            if colors[index] == 1 {
                return true;
            }
            if colors[index] == 2 {
                return false;
            }
            colors[index] = 1;
            for &next in edges[index].iter().flatten() {
                if cyclic(next, edges, colors) {
                    return true;
                }
            }
            colors[index] = 2;
            false
        }
        if cyclic(0, &edges, &mut vec![0; edges.len()]) {
            return true;
        }
        let mut distance = vec![usize::MAX; edges.len()];
        distance[0] = 0;
        let mut queue = std::collections::VecDeque::from([0]);
        while let Some(index) = queue.pop_front() {
            for &next in &edges[index] {
                match next {
                    None if distance[index] < 2 => return false,
                    Some(next) if distance[next] == usize::MAX => {
                        distance[next] = distance[index] + 1;
                        queue.push_back(next);
                    }
                    _ => {}
                }
            }
        }
        true
    }
}

/// Never persisted or cached: native code receives this stack borrow for one call.
/// Machine has paid the first block before constructing these counters.
#[repr(C)]
pub(in crate::execution) struct RunState {
    pub limit: u64,
    pub headroom: u64,
    pub remaining: u64,
    pub fixed: u64,
    pub blocks: u64,
    pub attempts: u64,
    pub block: u64,
    pub instruction: u64,
    #[cfg(test)]
    written: [u64; 2],
}
impl RunState {
    pub(in crate::execution) fn new(
        limit: u64,
        headroom: u64,
        remaining: u32,
        fixed: u64,
        blocks: u64,
    ) -> Self {
        Self {
            limit,
            headroom,
            remaining: remaining.into(),
            fixed,
            blocks,
            attempts: 0,
            block: 0,
            instruction: 0,
            #[cfg(test)]
            written: [0; 2],
        }
    }
}
pub(in crate::execution) struct RegionRun {
    pub state: RunState,
    pub exhausted: bool,
    pub failure: Option<InstructionFailure>,
}
impl CompiledRun {
    pub(in super::super) fn execute_region(
        &self,
        frame: &mut PreparedFrame<'_>,
        heap: HeapAccess<'_>,
        mut state: RunState,
    ) -> RegionRun {
        assert!(self.region && frame.jit_bytes().len() >= self.required_bytes as usize);
        let (status, failure) =
            self.invoke(frame, Some(heap), (&mut state as *mut RunState).cast());
        let reason = status >> 32;
        #[cfg(test)]
        for (pc, ranges) in self.writes.iter().enumerate() {
            if state.written[pc / 64] & (1 << (pc % 64)) != 0 {
                frame.jit_mark_writes(ranges);
            }
        }
        let failure = match reason {
            0 | 5 => None,
            2 => Some(InstructionFailure::Fault(VmFault::InvalidValueType)),
            3 => Some(InstructionFailure::Trap(GuestTrap::DivisionByZero)),
            4 => Some(failure.expect("checked callout failure")),
            6 => Some(InstructionFailure::Fault(VmFault::AccountingOverflow)),
            _ => unreachable!("private region status"),
        };
        RegionRun {
            state,
            exhausted: reason == 5,
            failure,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Exit {
    state: Value,
    counts: [Value; 4], // attempted delta, remaining, fixed absolute, entered absolute
    block: usize,
    instruction: usize,
}
impl Exit {
    pub(super) fn spill(self, b: &mut FunctionBuilder<'_>) {
        for (offset, value) in [
            (std::mem::offset_of!(RunState, attempts), self.counts[0]),
            (std::mem::offset_of!(RunState, remaining), self.counts[1]),
            (std::mem::offset_of!(RunState, fixed), self.counts[2]),
            (std::mem::offset_of!(RunState, blocks), self.counts[3]),
        ] {
            store(b, self.state, offset as u32, value);
        }
        for (offset, value) in [
            (std::mem::offset_of!(RunState, block), self.block),
            (
                std::mem::offset_of!(RunState, instruction),
                self.instruction,
            ),
        ] {
            let value = b.ins().iconst(types::I64, value as i64);
            store(b, self.state, offset as u32, value);
        }
    }
    fn finish(self, b: &mut FunctionBuilder<'_>) {
        self.spill(b);
        let status = b.ins().iconst(types::I64, 0);
        b.ins().return_(&[status]);
    }
    fn check(self, b: &mut FunctionBuilder<'_>, bad: Value, reason: u64) {
        guard(b, bad, reason, 0, Some(self));
    }
}

fn arguments(counts: [Value; 4]) -> [cranelift_codegen::ir::BlockArg; 4] {
    counts.map(Into::into)
}

fn edge(
    b: &mut FunctionBuilder<'_>,
    target: usize,
    exit: Exit,
    blocks: &[(usize, Block)],
    outside: &mut Vec<(Block, Exit)>,
) -> Block {
    if let Some((_, block)) = blocks.iter().find(|(id, _)| *id == target) {
        return *block;
    }
    let block = b.create_block();
    for _ in 0..4 {
        b.append_block_param(block, types::I64);
    }
    outside.push((
        block,
        Exit {
            block: target,
            instruction: 0,
            ..exit
        },
    ));
    block
}

pub(super) fn emit(b: &mut FunctionBuilder<'_>, emitter: &mut Emitter<'_>, state: Value) {
    let plan = emitter.plan;
    let limit = load(
        b,
        state,
        types::I64,
        std::mem::offset_of!(RunState, limit) as u32,
    );
    let headroom = load(
        b,
        state,
        types::I64,
        std::mem::offset_of!(RunState, headroom) as u32,
    );
    let initial = [
        std::mem::offset_of!(RunState, attempts),
        std::mem::offset_of!(RunState, remaining),
        std::mem::offset_of!(RunState, fixed),
        std::mem::offset_of!(RunState, blocks),
    ]
    .map(|offset| load(b, state, types::I64, offset as u32));
    let blocks: Vec<_> = plan
        .region
        .iter()
        .map(|region| {
            let block = b.create_block();
            for _ in 0..4 {
                b.append_block_param(block, types::I64);
            }
            (region.id, block)
        })
        .collect();
    let mut outside = Vec::new();
    let bodies: Vec<_> = plan
        .region
        .iter()
        .map(|_| {
            let block = b.create_block();
            for _ in 0..4 {
                b.append_block_param(block, types::I64);
            }
            block
        })
        .collect();
    // Machine has already paid this entry block. Backedges use its normal
    // header, so every later entry still checks and pays the original cost.
    b.ins().jump(bodies[0], &arguments(initial));
    for ((region, &(_, block)), &body) in plan.region.iter().zip(&blocks).zip(&bodies) {
        b.switch_to_block(block);
        let mut exit = Exit {
            state,
            counts: std::array::from_fn(|i| b.block_params(block)[i]),
            block: region.id,
            instruction: 0,
        };
        let stopped = b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, exit.counts[0], limit);
        exit.check(b, stopped, 5);
        let insufficient =
            b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThan, exit.counts[1], region.cost as i64);
        exit.check(b, insufficient, 5);
        let overflow = b.ins().icmp_imm_s(
            IntCC::UnsignedGreaterThan,
            exit.counts[2],
            (u64::MAX - region.cost as u64) as i64,
        );
        exit.check(b, overflow, 6);
        let overflow = b.ins().icmp_imm_s(IntCC::Equal, exit.counts[3], -1);
        exit.check(b, overflow, 6);
        exit.counts[1] = b.ins().iadd_imm_s(exit.counts[1], -(region.cost as i64));
        exit.counts[2] = b.ins().iadd_imm_s(exit.counts[2], region.cost as i64);
        exit.counts[3] = b.ins().iadd_imm_s(exit.counts[3], 1);
        b.ins().jump(body, &arguments(exit.counts));
        b.switch_to_block(body);
        exit.counts = std::array::from_fn(|i| b.block_params(body)[i]);
        let mut terminated = false;
        for pc in region.start..region.end {
            exit.instruction = pc - region.start;
            let stopped = b
                .ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, exit.counts[0], limit);
            exit.check(b, stopped, 5);
            let overflow =
                b.ins()
                    .icmp(IntCC::UnsignedGreaterThanOrEqual, exit.counts[0], headroom);
            exit.check(b, overflow, 6);
            exit.counts[0] = b.ins().iadd_imm_s(exit.counts[0], 1);
            let op = plan.ops[pc];
            if matches!(op.opcode, JUMP | BRANCH) {
                if op.opcode == JUMP {
                    let target = edge(b, op.dst as usize, exit, &blocks, &mut outside);
                    b.ins().jump(target, &arguments(exit.counts));
                } else {
                    let condition = load(b, emitter.base, types::I32, op.dst);
                    let bad = b.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, condition, 1);
                    guard(b, bad, 2, pc, Some(exit));
                    let yes = edge(b, op.lhs as usize, exit, &blocks, &mut outside);
                    let no = edge(b, op.rhs as usize, exit, &blocks, &mut outside);
                    b.ins().brif(
                        condition,
                        yes,
                        &arguments(exit.counts),
                        no,
                        &arguments(exit.counts),
                    );
                }
                terminated = true;
                break;
            }
            assert!(!emitter.operation(b, op, pc, Some(exit)));
            #[cfg(test)]
            {
                let offset = (std::mem::offset_of!(RunState, written) + (pc / 64) * 8) as u32;
                let value = load(b, state, types::I64, offset);
                let value = b.ins().bor_imm_s(value, (1_u64 << (pc % 64)) as i64);
                store(b, state, offset, value);
            }
        }
        if !terminated {
            exit.instruction = region.end - region.start;
            exit.finish(b);
        }
    }
    for (block, mut exit) in outside {
        b.switch_to_block(block);
        exit.counts = std::array::from_fn(|i| b.block_params(block)[i]);
        exit.finish(b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{
        fixtures, frame::FrameReservation, heap::Heap, prepared::tests::backed_arena,
    };

    #[test]
    fn short_acyclic_bridges_keep_prefixes_while_long_chains_and_cycles_compile() {
        let image = ExecutionImage::admit(
            fixtures::jit_large_region_artifact(false),
            fixtures::profile(),
        )
        .unwrap();
        assert!(Plan::region(&image, 22).is_none());
        assert!(Plan::region(&image, 21).is_some());
        let image =
            ExecutionImage::admit(fixtures::jit_numeric_loop_artifact(), fixtures::profile())
                .unwrap();
        assert!(Plan::region(&image, 0).is_some());
    }

    #[test]
    fn bounded_regions_resume_at_original_pc_when_graph_or_operation_cap_is_reached() {
        for wide in [false, true] {
            let image = ExecutionImage::admit(
                fixtures::jit_large_region_artifact(wide),
                fixtures::profile(),
            )
            .unwrap();
            let plan = Plan::region(&image, 0).unwrap();
            assert!(plan.region.len() <= 16 && plan.ops.len() <= MAX_OPS);
            if wide {
                assert_eq!(plan.ops.len(), MAX_OPS);
            } else {
                assert_eq!(plan.region.len(), 16);
            }
            let last = plan.region.last().unwrap();
            let op = plan.ops[last.end - 1];
            let expected = if op.opcode == JUMP {
                (op.dst as u64, 0)
            } else {
                (last.id as u64, (last.end - last.start) as u64)
            };
            let compiled = compile(plan, Reservation::acquire().unwrap()).unwrap();
            let mut arena = backed_arena(128);
            let mut frame = arena
                .prepared_frame(FrameReservation {
                    base: 0,
                    byte_len: 128,
                })
                .unwrap();
            let mut heap = Heap::new(&image.storage_plan()).unwrap();
            let result = compiled.execute_region(
                &mut frame,
                HeapAccess {
                    image: &image,
                    heap: &mut heap,
                },
                RunState::new(1000, 1000, 10000, 0, 0),
            );
            assert!(!result.exhausted && result.failure.is_none());
            assert_eq!((result.state.block, result.state.instruction), expected);
            assert_eq!(result.state.attempts, compiled.len as u64);
        }
    }

    #[test]
    fn connected_accounting_overflow_and_headroom_precede_guest_effects() {
        let image = ExecutionImage::admit(
            fixtures::jit_connected_loop_artifact(false),
            fixtures::profile(),
        )
        .unwrap();
        let compiled = compile(
            Plan::region(&image, 0).unwrap(),
            Reservation::acquire().unwrap(),
        )
        .unwrap();
        for (fixed, blocks) in [
            (0, 0),
            (u64::MAX, 0),
            (u64::MAX - 8, 0),
            (0, u64::MAX),
            (0, u64::MAX - 2),
        ] {
            for limit in 0..=24 {
                for headroom in 0..=24 {
                    let mut arena = backed_arena(128);
                    let mut frame = arena
                        .prepared_frame(FrameReservation {
                            base: 0,
                            byte_len: 128,
                        })
                        .unwrap();
                    let mut heap = Heap::new(&image.storage_plan()).unwrap();
                    let mut expected_arena = backed_arena(128);
                    let mut expected_frame = expected_arena
                        .prepared_frame(FrameReservation {
                            base: 0,
                            byte_len: 128,
                        })
                        .unwrap();
                    let mut expected = RunState::new(limit, headroom, 1000, fixed, blocks);
                    let mut expected_reason = 0;
                    let mut paid_entry = true;
                    loop {
                        let block = image.block(expected.block as usize).unwrap();
                        if expected.attempts >= limit {
                            expected_reason = 5;
                            break;
                        }
                        if expected.instruction == 0 && !paid_entry {
                            if expected.remaining < u64::from(block.fixed_cost) {
                                expected_reason = 5;
                                break;
                            }
                            let Some(fixed) = expected.fixed.checked_add(block.fixed_cost.into())
                            else {
                                expected_reason = 6;
                                break;
                            };
                            let Some(blocks) = expected.blocks.checked_add(1) else {
                                expected_reason = 6;
                                break;
                            };
                            expected.remaining -= u64::from(block.fixed_cost);
                            expected.fixed = fixed;
                            expected.blocks = blocks;
                        }
                        paid_entry = false;
                        let run = execute_run(
                            image.code(),
                            &mut expected_frame,
                            block.code_start,
                            expected.instruction as usize,
                            block.instruction_count,
                            RunBudget {
                                limit: limit - expected.attempts,
                                headroom: headroom - expected.attempts,
                            },
                            |_, _| unreachable!(),
                        );
                        expected.attempts += run.attempts;
                        expected.instruction = run.instruction as u64;
                        match run.end {
                            RunEnd::Branch(target) => {
                                expected.block = target as u64;
                                expected.instruction = 0;
                            }
                            RunEnd::Failure(InstructionFailure::Fault(
                                VmFault::AccountingOverflow,
                            )) => {
                                expected_reason = 6;
                                break;
                            }
                            RunEnd::Boundary => {
                                if expected.attempts == limit {
                                    expected_reason = 5;
                                }
                                break;
                            }
                            _ => unreachable!(),
                        }
                        if expected.block == 3 {
                            break;
                        }
                    }
                    let actual = compiled.execute_region(
                        &mut frame,
                        HeapAccess {
                            image: &image,
                            heap: &mut heap,
                        },
                        RunState::new(limit, headroom, 1000, fixed, blocks),
                    );
                    assert_eq!(actual.exhausted, expected_reason == 5);
                    assert_eq!(actual.failure.is_some(), expected_reason == 6);
                    if let Some(failure) = actual.failure {
                        assert!(matches!(
                            failure,
                            InstructionFailure::Fault(VmFault::AccountingOverflow)
                        ));
                    }
                    let fields = |s: &RunState| {
                        [
                            s.remaining,
                            s.fixed,
                            s.blocks,
                            s.attempts,
                            s.block,
                            s.instruction,
                        ]
                    };
                    assert_eq!(
                        fields(&actual.state),
                        fields(&expected),
                        "limit={limit} headroom={headroom} fixed={fixed} blocks={blocks}"
                    );
                    assert_eq!(frame.jit_bytes(), expected_frame.jit_bytes());
                }
            }
        }
    }
}
