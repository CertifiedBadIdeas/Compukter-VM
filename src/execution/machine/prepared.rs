/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 * You may obtain a copy at https://www.apache.org/licenses/LICENSE-2.0
 */

//! Machine-owned effects of immutable prepared operations.
use super::*;
use crate::execution::prepared::{self as code, Operation};

impl Machine {
    #[inline]
    pub(super) fn execute_prepared_data(
        &mut self,
        operation: Operation,
        frame: Frame,
    ) -> Result<Option<usize>, InstructionFailure> {
        let op = operation;
        let reservation = FrameReservation {
            base: frame.base,
            byte_len: frame.byte_len,
        };
        let fault = InstructionFailure::Fault;
        macro_rules! read {
            ($ty:ty, $offset:expr) => {
                <$ty>::from_le_bytes(
                    self.frame_arena
                        .read_prepared(reservation, $offset)
                        .map_err(fault)?,
                )
            };
        }
        macro_rules! write {
            ($offset:expr, $value:expr) => {
                self.frame_arena
                    .write_prepared(reservation, $offset, ($value).to_le_bytes())
                    .map_err(fault)?
            };
        }
        match op.opcode & 0xffff {
            opcode if (code::FIELD_GET + 1..=code::FIELD_GET + 7).contains(&opcode) => {
                let kind = (opcode - code::FIELD_GET) as u8;
                let receiver = read_reference(read!(u32, op.lhs))?;
                let field = *self
                    .image
                    .code()
                    .fields
                    .get(op.rhs as usize)
                    .ok_or(fault(VmFault::InvalidResolvedId))?;
                let actual = self.heap.managed_type(receiver).map_err(fault)?;
                let actual = self
                    .image
                    .type_key(actual as usize)
                    .ok_or(fault(VmFault::InvalidResolvedId))?;
                if !self.image.is_assignable(actual, field.owner) {
                    return Err(fault(VmFault::InvalidReference));
                }
                let offset = field.offset.ok_or(fault(VmFault::InvalidResolvedId))?;
                let width =
                    width_for_type(field.value_type).ok_or(fault(VmFault::InvalidValueType))?;
                let value = load_value(&self.heap, receiver, offset, width).map_err(fault)?;
                if value == RuntimeValue::Null && !field.value_type.nullable {
                    return Err(InstructionFailure::Trap(GuestTrap::NullReference));
                }
                write_value(&mut self.frame_arena, reservation, op.dst, kind, value)
                    .map_err(fault)?;
            }
            opcode if (code::FIELD_SET + 1..=code::FIELD_SET + 7).contains(&opcode) => {
                let kind = (opcode - code::FIELD_SET) as u8;
                // The old operation reads receiver and value before checking null.
                let receiver_bits = read!(u32, op.lhs);
                let value =
                    read_value(&self.frame_arena, reservation, op.dst, kind).map_err(fault)?;
                let receiver = read_reference(receiver_bits)?;
                let field = *self
                    .image
                    .code()
                    .fields
                    .get(op.rhs as usize)
                    .ok_or(fault(VmFault::InvalidResolvedId))?;
                let actual = self.heap.managed_type(receiver).map_err(fault)?;
                let actual = self
                    .image
                    .type_key(actual as usize)
                    .ok_or(fault(VmFault::InvalidResolvedId))?;
                if !self.image.is_assignable(actual, field.owner)
                    || !self.runtime_value_matches(value, field.value_type)
                {
                    return Err(fault(VmFault::InvalidReference));
                }
                let offset = field.offset.ok_or(fault(VmFault::InvalidResolvedId))?;
                let width =
                    width_for_type(field.value_type).ok_or(fault(VmFault::InvalidValueType))?;
                store_value(&mut self.heap, receiver, offset, width, value).map_err(fault)?;
            }
            code::ARRAY_LENGTH => {
                let array = read_value(&self.frame_arena, reservation, op.lhs, 7).map_err(fault)?;
                let (_, _, _, length) = self.resolve_array(array)?;
                write!(op.dst, length);
            }
            opcode if (code::ARRAY_LOAD + 1..=code::ARRAY_LOAD + 7).contains(&opcode) => {
                let kind = (opcode - code::ARRAY_LOAD) as u8;
                let array = read_value(&self.frame_arena, reservation, op.lhs, 7).map_err(fault)?;
                let index = read!(i32, op.rhs);
                let (reference, _, element, length) = self.resolve_array(array)?;
                let offset = array_element_offset(index, length, element)
                    .map_err(InstructionFailure::Trap)?;
                let value = load_value(&self.heap, reference, offset, element).map_err(fault)?;
                if value == RuntimeValue::Null && op.opcode & code::NON_NULL != 0 {
                    return Err(InstructionFailure::Trap(GuestTrap::NullReference));
                }
                write_value(&mut self.frame_arena, reservation, op.dst, kind, value)
                    .map_err(fault)?;
            }
            opcode if (code::ARRAY_STORE + 1..=code::ARRAY_STORE + 7).contains(&opcode) => {
                let kind = (opcode - code::ARRAY_STORE) as u8;
                let array = read_value(&self.frame_arena, reservation, op.lhs, 7).map_err(fault)?;
                let index = read!(i32, op.rhs);
                let value =
                    read_value(&self.frame_arena, reservation, op.dst, kind).map_err(fault)?;
                let (reference, array_type, element, length) = self.resolve_array(array)?;
                let element_type = self
                    .image
                    .array_element_type(array_type)
                    .ok_or(fault(VmFault::InvalidResolvedId))?;
                if !self.runtime_value_matches(value, element_type) {
                    return Err(fault(VmFault::InvalidReference));
                }
                let offset = array_element_offset(index, length, element)
                    .map_err(InstructionFailure::Trap)?;
                store_value(&mut self.heap, reference, offset, element, value).map_err(fault)?;
            }
            _ => return Err(fault(VmFault::UnsupportedInstruction)),
        }
        Ok(None)
    }
}

fn read_reference(bits: u32) -> Result<Ref32, InstructionFailure> {
    Ref32::from_bits(bits).ok_or(InstructionFailure::Trap(GuestTrap::NullReference))
}

fn read_value(
    arena: &FrameArena,
    frame: FrameReservation,
    offset: u32,
    kind: u8,
) -> Result<RuntimeValue, VmFault> {
    macro_rules! read {
        ($ty:ty) => {
            <$ty>::from_le_bytes(arena.read_prepared(frame, offset)?)
        };
    }
    Ok(match kind {
        1 => RuntimeValue::I32(read!(i32)),
        2 => RuntimeValue::I64(read!(i64)),
        3 => RuntimeValue::F32(read!(u32)),
        4 => RuntimeValue::F64(read!(u64)),
        5 => RuntimeValue::Bool(match read!(u32) {
            0 => false,
            1 => true,
            _ => return Err(VmFault::InvalidValueType),
        }),
        6 => RuntimeValue::Char(u16::try_from(read!(u32)).map_err(|_| VmFault::InvalidValueType)?),
        7 => Ref32::from_bits(read!(u32)).map_or(RuntimeValue::Null, RuntimeValue::Reference),
        _ => return Err(VmFault::InvalidValueType),
    })
}

fn write_value(
    arena: &mut FrameArena,
    frame: FrameReservation,
    offset: u32,
    kind: u8,
    value: RuntimeValue,
) -> Result<(), VmFault> {
    macro_rules! write {
        ($value:expr) => {
            arena.write_prepared(frame, offset, ($value).to_le_bytes())
        };
    }
    match (kind, value) {
        (1, RuntimeValue::I32(value)) => write!(value),
        (2, RuntimeValue::I64(value)) => write!(value),
        (3, RuntimeValue::F32(value)) => write!(value),
        (4, RuntimeValue::F64(value)) => write!(value),
        (5, RuntimeValue::Bool(value)) => write!(u32::from(value)),
        (6, RuntimeValue::Char(value)) => write!(u32::from(value)),
        (7, RuntimeValue::Null) => write!(0u32),
        (7, RuntimeValue::Reference(value)) => write!(value.to_bits()),
        _ => Err(VmFault::InvalidValueType),
    }
}
