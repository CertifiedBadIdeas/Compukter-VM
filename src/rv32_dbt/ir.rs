/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

use crate::memory::{MemoryBus, MemoryFault};
use crate::rv32im::{decode_fields, DecodedFields, DecodedOp};
use std::cell::Cell;
use std::mem::MaybeUninit;

const PAGE_BYTES: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DbtIrOp {
    UpperImmediate,
    Jump,
    Branch,
    Load,
    Store,
    Immediate,
    Register,
    BitManip,
    Fence,
    Atomic,
    Csr,
    Trap,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DbtIrInstruction {
    word: u32,
    fields: DecodedFields,
    effects: DbtRegisterEffects,
}

const INSTRUCTIONS_PER_CACHE_CHUNK: usize = 16;

#[repr(C, align(64))]
#[derive(Debug, Clone, Copy)]
struct IrInstructionChunk {
    slots: [MaybeUninit<DbtIrInstruction>; INSTRUCTIONS_PER_CACHE_CHUNK],
}

impl IrInstructionChunk {
    const EMPTY: Self = Self {
        slots: [MaybeUninit::uninit(); INSTRUCTIONS_PER_CACHE_CHUNK],
    };
}

#[derive(Debug)]
struct AlignedInstructions {
    chunks: Vec<IrInstructionChunk>,
    len: usize,
    capacity: usize,
}

impl AlignedInstructions {
    fn new(capacity: usize) -> Self {
        debug_assert_eq!(
            std::mem::size_of::<IrInstructionChunk>(),
            INSTRUCTIONS_PER_CACHE_CHUNK * std::mem::size_of::<DbtIrInstruction>()
        );
        Self {
            chunks: vec![
                IrInstructionChunk::EMPTY;
                capacity.div_ceil(INSTRUCTIONS_PER_CACHE_CHUNK)
            ],
            len: 0,
            capacity,
        }
    }

    fn as_slice(&self) -> &[DbtIrInstruction] {
        // SAFETY: the first `len` slots are initialized by `push`, chunks are
        // contiguous, and the chunk size assertion excludes inter-chunk padding.
        unsafe {
            std::slice::from_raw_parts(self.chunks.as_ptr().cast::<DbtIrInstruction>(), self.len)
        }
    }

    fn push(&mut self, instruction: DbtIrInstruction) {
        assert!(self.len < self.capacity);
        // SAFETY: `len < capacity`, and the rounded chunk allocation contains
        // every logical slot through `capacity - 1`.
        unsafe {
            self.chunks
                .as_mut_ptr()
                .cast::<DbtIrInstruction>()
                .add(self.len)
                .write(instruction);
        }
        self.len += 1;
    }

    const fn len(&self) -> usize {
        self.len
    }

    const fn capacity(&self) -> usize {
        self.capacity
    }

    fn retained_bytes(&self) -> usize {
        self.chunks.capacity() * std::mem::size_of::<IrInstructionChunk>()
    }

    fn last(&self) -> Option<&DbtIrInstruction> {
        self.as_slice().last()
    }

    fn clear(&mut self) {
        self.len = 0;
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DbtRegisterEffects {
    reads: [u8; 2],
    write: u8,
    may_exit_before_write: bool,
}

const NO_REGISTER: u8 = u8::MAX;

impl DbtRegisterEffects {
    fn from_fields(fields: DecodedFields) -> Self {
        let (reads, write) = register_effects(fields);
        Self {
            reads: reads.map(|register| register.unwrap_or(NO_REGISTER)),
            write: write.unwrap_or(NO_REGISTER),
            may_exit_before_write: may_exit_before_write(fields.operation),
        }
    }

    pub(crate) fn reads(self) -> impl Iterator<Item = u8> {
        self.reads
            .into_iter()
            .filter(|register| *register != NO_REGISTER)
    }

    pub(crate) const fn write(self) -> Option<u8> {
        if self.write == NO_REGISTER {
            None
        } else {
            Some(self.write)
        }
    }

    pub(crate) const fn may_exit_before_write(self) -> bool {
        self.may_exit_before_write
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FutureValue {
    Unused,
    Read(u8),
    Dead(u8),
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegisterEventKind {
    Read,
    Write,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RegisterEvent {
    instruction: u8,
    kind: RegisterEventKind,
}

impl RegisterEvent {
    const EMPTY: Self = Self {
        instruction: 0,
        kind: RegisterEventKind::Read,
    };
}

impl DbtIrInstruction {
    pub(crate) const fn word(self) -> u32 {
        self.word
    }

    pub(crate) const fn fields(self) -> DecodedFields {
        self.fields
    }

    pub(crate) const fn effects(self) -> DbtRegisterEffects {
        self.effects
    }

    pub(crate) const fn operation(self) -> DbtIrOp {
        match self.fields.operation {
            DecodedOp::Lui | DecodedOp::Auipc => DbtIrOp::UpperImmediate,
            DecodedOp::Jal | DecodedOp::Jalr => DbtIrOp::Jump,
            DecodedOp::Branch(_) => DbtIrOp::Branch,
            DecodedOp::Load(_) | DecodedOp::LoadReserved(_) => DbtIrOp::Load,
            DecodedOp::Store(_) => DbtIrOp::Store,
            DecodedOp::Immediate(_) => DbtIrOp::Immediate,
            DecodedOp::Register(_) => DbtIrOp::Register,
            DecodedOp::Zbb(_) => DbtIrOp::BitManip,
            DecodedOp::Fence | DecodedOp::FenceI => DbtIrOp::Fence,
            DecodedOp::StoreConditional(_) | DecodedOp::Atomic(_, _) => DbtIrOp::Atomic,
            DecodedOp::Csr { .. } => DbtIrOp::Csr,
            DecodedOp::Ecall | DecodedOp::Ebreak | DecodedOp::Mret => DbtIrOp::Trap,
        }
    }
}

#[derive(Debug)]
pub(crate) struct DbtIrBlock {
    instructions: AlignedInstructions,
    register_events: Vec<RegisterEvent>,
    register_event_counts: [u8; 32],
    register_ranges: [(u16, u16); 32],
    exits: Vec<u8>,
    capacity: usize,
    invalid_word: Option<u32>,
}

#[derive(Debug)]
pub(crate) struct DbtIrFutureCursor<'a> {
    ir: &'a DbtIrBlock,
    register_positions: [Cell<u16>; 32],
    exit_position: Cell<u16>,
}

impl<'a> DbtIrFutureCursor<'a> {
    pub(crate) fn new(ir: &'a DbtIrBlock) -> Self {
        Self {
            ir,
            register_positions: [const { Cell::new(0) }; 32],
            exit_position: Cell::new(0),
        }
    }

    pub(crate) fn future_value(&self, instruction: usize, guest: usize) -> FutureValue {
        let (start, end) = self.ir.register_ranges[guest];
        let position = &self.register_positions[guest];
        let mut next = position.get().max(start);
        while next < end
            && usize::from(self.ir.register_events[usize::from(next)].instruction) < instruction
        {
            next += 1;
        }
        position.set(next);
        let Some(event) = (next < end).then(|| self.ir.register_events[usize::from(next)]) else {
            return FutureValue::Unused;
        };
        let distance = event.instruction - instruction as u8;
        match event.kind {
            RegisterEventKind::Read => FutureValue::Read(distance),
            RegisterEventKind::Write => {
                let mut exit = self.exit_position.get();
                while usize::from(exit) < self.ir.exits.len()
                    && usize::from(self.ir.exits[usize::from(exit)]) < instruction
                {
                    exit += 1;
                }
                self.exit_position.set(exit);
                if self
                    .ir
                    .exits
                    .get(usize::from(exit))
                    .is_some_and(|exit| *exit <= event.instruction)
                {
                    FutureValue::Read(distance)
                } else {
                    FutureValue::Dead(distance)
                }
            }
        }
    }
}

impl DbtIrBlock {
    pub(crate) fn new(capacity: usize) -> Result<Self, String> {
        if capacity == 0 || capacity > 64 {
            return Err(format!("RV32 DBT IR capacity {capacity} is outside 1..=64"));
        }
        Ok(Self {
            instructions: AlignedInstructions::new(capacity),
            register_events: Vec::with_capacity(capacity * 3),
            register_event_counts: [0; 32],
            register_ranges: [(0, 0); 32],
            exits: Vec::with_capacity(capacity),
            capacity,
            invalid_word: None,
        })
    }

    pub(crate) fn instructions(&self) -> &[DbtIrInstruction] {
        self.instructions.as_slice()
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.instructions.retained_bytes() + self.analysis_retained_bytes()
    }

    pub(crate) fn analysis_retained_bytes(&self) -> usize {
        self.register_events.capacity() * std::mem::size_of::<RegisterEvent>()
            + self.exits.capacity() * std::mem::size_of::<u8>()
            + std::mem::size_of_val(&self.register_event_counts)
            + std::mem::size_of_val(&self.register_ranges)
    }

    pub(crate) fn attempted_instruction_count(&self) -> usize {
        self.instructions.len() + usize::from(self.invalid_word.is_some())
    }

    pub(crate) const fn invalid_word(&self) -> Option<u32> {
        self.invalid_word
    }

    pub(crate) fn reset(&mut self) {
        self.instructions.clear();
        self.register_events.clear();
        self.register_event_counts.fill(0);
        self.register_ranges.fill((0, 0));
        self.exits.clear();
        self.invalid_word = None;
    }

    #[cfg(test)]
    pub(crate) fn instruction_capacity(&self) -> usize {
        self.instructions.capacity()
    }

    #[cfg(test)]
    pub(crate) fn analysis_capacity(&self) -> usize {
        self.register_events.capacity()
    }

    pub(crate) fn lift_word(&mut self, word: u32) -> Result<(), String> {
        if self.instructions.len() == self.capacity {
            return Err(format!(
                "RV32 DBT IR block exceeded its {}-instruction capacity",
                self.capacity
            ));
        }
        let fields = decode_fields(word)?;
        let effects = DbtRegisterEffects::from_fields(fields);
        let instruction = self.instructions.len() as u8;
        for register in effects.reads() {
            self.register_event_counts[usize::from(register)] += 1;
        }
        if let Some(register) = effects.write() {
            self.register_event_counts[usize::from(register)] += 1;
        }
        if effects.may_exit_before_write() {
            self.exits.push(instruction);
        }
        self.instructions.push(DbtIrInstruction {
            word,
            fields,
            effects,
        });
        Ok(())
    }

    pub(crate) fn analyze_future_values(&mut self) {
        self.register_events.clear();
        let mut end = 0_u16;
        for (guest, count) in self.register_event_counts.into_iter().enumerate() {
            let start = end;
            end += u16::from(count);
            self.register_ranges[guest] = (start, end);
        }
        self.register_events
            .resize(usize::from(end), RegisterEvent::EMPTY);
        let mut cursors = self.register_ranges.map(|(start, _)| start);
        for (index, instruction) in self.instructions.as_slice().iter().copied().enumerate() {
            let effects = instruction.effects();
            for register in effects.reads() {
                insert_register_event(
                    &mut self.register_events,
                    &mut cursors,
                    register,
                    RegisterEvent {
                        instruction: index as u8,
                        kind: RegisterEventKind::Read,
                    },
                );
            }
            if let Some(register) = effects.write() {
                insert_register_event(
                    &mut self.register_events,
                    &mut cursors,
                    register,
                    RegisterEvent {
                        instruction: index as u8,
                        kind: RegisterEventKind::Write,
                    },
                );
            }
        }
    }

    pub(crate) fn future_value(&self, instruction: usize, guest: usize) -> FutureValue {
        let (start, end) = self.register_ranges[guest];
        let events = &self.register_events[usize::from(start)..usize::from(end)];
        let next = events.partition_point(|event| usize::from(event.instruction) < instruction);
        let Some(event) = events.get(next).copied() else {
            return FutureValue::Unused;
        };
        let distance = event.instruction - instruction as u8;
        match event.kind {
            RegisterEventKind::Read => FutureValue::Read(distance),
            RegisterEventKind::Write => {
                let next_exit = self
                    .exits
                    .partition_point(|exit| usize::from(*exit) < instruction);
                if self
                    .exits
                    .get(next_exit)
                    .is_some_and(|exit| *exit <= event.instruction)
                {
                    FutureValue::Read(distance)
                } else {
                    FutureValue::Dead(distance)
                }
            }
        }
    }
}

fn insert_register_event(
    events: &mut [RegisterEvent],
    cursors: &mut [u16; 32],
    register: u8,
    event: RegisterEvent,
) {
    let cursor = &mut cursors[usize::from(register)];
    events[usize::from(*cursor)] = event;
    *cursor += 1;
}

pub(crate) fn fill_ir_block(
    start_pc: u32,
    executable_end: u32,
    max_instructions: usize,
    bus: &dyn MemoryBus,
    block: &mut DbtIrBlock,
) -> Result<(), MemoryFault> {
    if max_instructions == 0 || max_instructions > block.capacity {
        return Err(MemoryFault::new(format!(
            "RV32 DBT IR block maximum {max_instructions} exceeds workspace capacity {}",
            block.capacity
        )));
    }
    let page_end = (start_pc & !(PAGE_BYTES - 1)).saturating_add(PAGE_BYTES);
    let block_end = executable_end.min(page_end);
    require_complete_word(start_pc, block_end)?;
    block.reset();

    let mut instruction_pc = start_pc;
    while block.attempted_instruction_count() < max_instructions {
        if instruction_pc
            .checked_add(4)
            .is_none_or(|instruction_end| instruction_end > block_end)
        {
            break;
        }
        let word = bus.load_i32(instruction_pc)? as u32;
        match block.lift_word(word) {
            Ok(()) => {
                if ends_ir_block(block.instructions.last().unwrap().fields.operation) {
                    break;
                }
            }
            Err(_) => {
                block.invalid_word = Some(word);
                break;
            }
        }
        instruction_pc = instruction_pc.wrapping_add(4);
    }
    block.analyze_future_values();
    Ok(())
}

fn require_complete_word(instruction_pc: u32, block_end: u32) -> Result<(), MemoryFault> {
    if instruction_pc
        .checked_add(4)
        .is_none_or(|instruction_end| instruction_end > block_end)
    {
        return Err(MemoryFault::at(
            instruction_pc,
            format!(
                "RV32 instruction at {instruction_pc:#010x} crosses DBT IR block boundary {block_end:#010x}"
            ),
        ));
    }
    Ok(())
}

fn ends_ir_block(operation: DecodedOp) -> bool {
    matches!(
        operation,
        DecodedOp::Jal
            | DecodedOp::Jalr
            | DecodedOp::Branch(_)
            | DecodedOp::Ecall
            | DecodedOp::Ebreak
            | DecodedOp::Mret
            | DecodedOp::FenceI
    )
}

pub(crate) fn register_effects(fields: DecodedFields) -> ([Option<u8>; 2], Option<u8>) {
    match fields.operation {
        DecodedOp::Lui | DecodedOp::Auipc | DecodedOp::Jal => ([None, None], Some(fields.rd)),
        DecodedOp::Jalr | DecodedOp::Load(_) | DecodedOp::LoadReserved(_) => {
            ([Some(fields.rs1), None], Some(fields.rd))
        }
        DecodedOp::Branch(_) | DecodedOp::Store(_) => ([Some(fields.rs1), Some(fields.rs2)], None),
        DecodedOp::Immediate(_) => ([Some(fields.rs1), None], Some(fields.rd)),
        DecodedOp::Register(_) | DecodedOp::StoreConditional(_) | DecodedOp::Atomic(_, _) => {
            ([Some(fields.rs1), Some(fields.rs2)], Some(fields.rd))
        }
        DecodedOp::Zbb(op) => (
            [
                Some(fields.rs1),
                op.uses_register_rhs().then_some(fields.rs2),
            ],
            Some(fields.rd),
        ),
        DecodedOp::Csr {
            immediate_source, ..
        } => (
            [(!immediate_source).then_some(fields.rs1), None],
            Some(fields.rd),
        ),
        DecodedOp::Fence
        | DecodedOp::FenceI
        | DecodedOp::Ecall
        | DecodedOp::Ebreak
        | DecodedOp::Mret => ([None, None], None),
    }
}

pub(crate) fn may_exit_before_write(operation: DecodedOp) -> bool {
    matches!(
        operation,
        DecodedOp::Jal
            | DecodedOp::Jalr
            | DecodedOp::Load(_)
            | DecodedOp::Store(_)
            | DecodedOp::LoadReserved(_)
            | DecodedOp::StoreConditional(_)
            | DecodedOp::Atomic(_, _)
            | DecodedOp::Csr { .. }
            | DecodedOp::FenceI
            | DecodedOp::Ecall
            | DecodedOp::Ebreak
            | DecodedOp::Mret
    )
}

#[cfg(test)]
mod tests {
    use super::{
        fill_ir_block, DbtIrBlock, DbtIrFutureCursor, DbtIrInstruction, DbtIrOp, FutureValue,
    };
    use crate::memory::MachineMemory;
    use crate::rv32im::encoding::{add, addi, ecall, jal};

    fn memory(words: &[u32]) -> MachineMemory {
        let mut memory = MachineMemory::zeroed(words.len().max(1) * 4).unwrap();
        for (index, word) in words.iter().copied().enumerate() {
            memory.store_i32(index as u32 * 4, word as i32).unwrap();
        }
        memory
    }

    #[test]
    fn lifting_records_compact_semantics_and_register_effects() {
        let mut block = DbtIrBlock::new(2).unwrap();

        block.lift_word(addi(1, 0, 7)).unwrap();
        block.lift_word(add(2, 1, 1)).unwrap();

        assert_eq!(block.instructions().len(), 2);
        assert_eq!(block.instructions()[0].operation(), DbtIrOp::Immediate);
        assert_eq!(
            block.instructions()[0]
                .effects()
                .reads()
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(block.instructions()[0].effects().write(), Some(1));
        assert_eq!(block.instructions()[1].operation(), DbtIrOp::Register);
        assert_eq!(
            block.instructions()[1]
                .effects()
                .reads()
                .collect::<Vec<_>>(),
            vec![1, 1]
        );
        assert_eq!(block.instructions()[1].effects().write(), Some(2));
    }

    #[test]
    fn ir_instruction_stays_small_enough_for_a_complete_block_to_fit_in_l1() {
        assert!(std::mem::size_of::<DbtIrInstruction>() <= 20);
    }

    #[test]
    fn reverse_analysis_distinguishes_reads_dead_writes_and_exits() {
        let mut block = DbtIrBlock::new(4).unwrap();
        block.lift_word(addi(3, 0, 1)).unwrap();
        block.lift_word(addi(4, 3, 1)).unwrap();
        block.lift_word(ecall()).unwrap();
        block.lift_word(addi(3, 0, 2)).unwrap();

        block.analyze_future_values();

        assert_eq!(block.future_value(0, 3), FutureValue::Dead(0));
        assert_eq!(block.future_value(1, 3), FutureValue::Read(0));
        assert_eq!(block.future_value(2, 3), FutureValue::Read(1));
        assert_eq!(block.future_value(3, 3), FutureValue::Dead(0));
        assert_eq!(block.future_value(0, 31), FutureValue::Unused);
    }

    #[test]
    fn monotonic_cursor_matches_random_access_future_queries() {
        let mut block = DbtIrBlock::new(4).unwrap();
        block.lift_word(addi(3, 0, 1)).unwrap();
        block.lift_word(addi(4, 3, 1)).unwrap();
        block.lift_word(ecall()).unwrap();
        block.lift_word(addi(3, 0, 2)).unwrap();
        block.analyze_future_values();
        let cursor = DbtIrFutureCursor::new(&block);

        for instruction in 0..block.instructions().len() {
            for guest in 0..32 {
                assert_eq!(
                    cursor.future_value(instruction, guest),
                    block.future_value(instruction, guest)
                );
            }
        }
    }

    #[test]
    fn sparse_analysis_storage_scales_with_register_events_not_register_file_snapshots() {
        let block = DbtIrBlock::new(64).unwrap();

        assert!(block.analysis_retained_bytes() <= 640);
    }

    #[test]
    fn ir_workspace_starts_on_a_cache_line() {
        for capacity in [8, 16, 32, 64] {
            let block = DbtIrBlock::new(capacity).unwrap();
            assert_eq!(block.instructions().as_ptr() as usize % 64, 0);
        }
    }

    #[test]
    fn fused_builder_stops_at_control_flow_and_reuses_its_storage() {
        let first = memory(&[addi(1, 1, 1), jal(0, 0), addi(2, 2, 1)]);
        let second = memory(&[addi(3, 3, 1)]);
        let mut block = DbtIrBlock::new(8).unwrap();
        let instruction_capacity = block.instruction_capacity();
        let analysis_capacity = block.analysis_capacity();

        fill_ir_block(0, 12, 8, &first, &mut block).unwrap();
        assert_eq!(block.instructions().len(), 2);
        assert_eq!(block.attempted_instruction_count(), 2);
        assert_eq!(block.invalid_word(), None);

        fill_ir_block(0, 4, 8, &second, &mut block).unwrap();
        assert_eq!(block.instructions().len(), 1);
        assert_eq!(block.instruction_capacity(), instruction_capacity);
        assert_eq!(block.analysis_capacity(), analysis_capacity);
    }

    #[test]
    fn fused_builder_retains_an_invalid_terminal_word_without_a_decoded_enum() {
        let memory = memory(&[addi(1, 1, 1), 0xffff_ffff, addi(2, 2, 1)]);
        let mut block = DbtIrBlock::new(8).unwrap();

        fill_ir_block(0, 12, 8, &memory, &mut block).unwrap();

        assert_eq!(block.instructions().len(), 1);
        assert_eq!(block.attempted_instruction_count(), 2);
        assert_eq!(block.invalid_word(), Some(0xffff_ffff));
    }
}
