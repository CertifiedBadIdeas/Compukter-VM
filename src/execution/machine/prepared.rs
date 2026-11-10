/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 * You may obtain a copy at https://www.apache.org/licenses/LICENSE-2.0
 */

//! Checked non-allocating heap operations over a scoped frame and heap borrow.
use super::*;
use crate::execution::frame::PreparedFrame;
use crate::execution::prepared::{self as code, Operation};

#[inline]
pub(in crate::execution) fn execute_heap(
    operation: Operation,
    frame: &mut PreparedFrame<'_>,
    image: &ExecutionImage,
    heap: &mut Heap,
) -> Result<(), InstructionFailure> {
    execute_heap_opcode::<0>(operation, frame, image, heap)
}

// Zero selects the dynamic interpreter operation. JIT callouts select their
// verified opcode at compile time, keeping scalar decoding and all heap checks
// in this one implementation while removing repeated runtime type dispatch.
#[inline(always)]
pub(in crate::execution) fn execute_heap_opcode<const OPCODE: u32>(
    operation: Operation,
    frame: &mut PreparedFrame<'_>,
    image: &ExecutionImage,
    heap: &mut Heap,
) -> Result<(), InstructionFailure> {
    let op = if OPCODE == 0 {
        operation
    } else {
        Operation {
            opcode: OPCODE,
            ..operation
        }
    };
    let fault = InstructionFailure::Fault;
    macro_rules! read {
        ($ty:ty, $offset:expr) => {
            <$ty>::from_le_bytes(frame.read($offset).map_err(fault)?)
        };
    }
    macro_rules! write {
        ($offset:expr, $value:expr) => {
            frame
                .write($offset, ($value).to_le_bytes())
                .map_err(fault)?
        };
    }
    match op.opcode & 0xffff {
        opcode if (code::FIELD_GET + 1..=code::FIELD_GET + 7).contains(&opcode) => {
            let kind = (opcode - code::FIELD_GET) as u8;
            let receiver = read_reference(read!(u32, op.lhs))?;
            let field = *image
                .code()
                .fields
                .get(op.rhs as usize)
                .ok_or(fault(VmFault::InvalidResolvedId))?;
            let actual = heap.managed_type(receiver).map_err(fault)?;
            let actual = image
                .type_key(actual as usize)
                .ok_or(fault(VmFault::InvalidResolvedId))?;
            if !image.is_assignable(actual, field.owner) {
                return Err(fault(VmFault::InvalidReference));
            }
            let offset = field.offset.ok_or(fault(VmFault::InvalidResolvedId))?;
            let width = width_for_type(field.value_type).ok_or(fault(VmFault::InvalidValueType))?;
            let value = load_value(heap, receiver, offset, width).map_err(fault)?;
            if value == RuntimeValue::Null && !field.value_type.nullable {
                return Err(InstructionFailure::Trap(GuestTrap::NullReference));
            }
            write_value(frame, op.dst, kind, value).map_err(fault)?;
        }
        opcode if (code::FIELD_SET + 1..=code::FIELD_SET + 7).contains(&opcode) => {
            let kind = (opcode - code::FIELD_SET) as u8;
            // The old operation reads receiver and value before checking null.
            let receiver_bits = read!(u32, op.lhs);
            let value = read_value(frame, op.dst, kind).map_err(fault)?;
            let receiver = read_reference(receiver_bits)?;
            let field = *image
                .code()
                .fields
                .get(op.rhs as usize)
                .ok_or(fault(VmFault::InvalidResolvedId))?;
            let actual = heap.managed_type(receiver).map_err(fault)?;
            let actual = image
                .type_key(actual as usize)
                .ok_or(fault(VmFault::InvalidResolvedId))?;
            if !image.is_assignable(actual, field.owner)
                || !runtime_value_matches(image, heap, value, field.value_type)
            {
                return Err(fault(VmFault::InvalidReference));
            }
            let offset = field.offset.ok_or(fault(VmFault::InvalidResolvedId))?;
            let width = width_for_type(field.value_type).ok_or(fault(VmFault::InvalidValueType))?;
            store_value(heap, receiver, offset, width, value).map_err(fault)?;
        }
        code::ARRAY_LENGTH => {
            let array = read_value(frame, op.lhs, 7).map_err(fault)?;
            let (_, _, _, length) = resolve_array(image, heap, array)?;
            write!(op.dst, length);
        }
        opcode if (code::ARRAY_LOAD + 1..=code::ARRAY_LOAD + 7).contains(&opcode) => {
            let kind = (opcode - code::ARRAY_LOAD) as u8;
            let array = read_value(frame, op.lhs, 7).map_err(fault)?;
            let index = read!(i32, op.rhs);
            let (reference, _, element, length) = resolve_array(image, heap, array)?;
            let offset =
                array_element_offset(index, length, element).map_err(InstructionFailure::Trap)?;
            let value = load_value(heap, reference, offset, element).map_err(fault)?;
            if value == RuntimeValue::Null && op.opcode & code::NON_NULL != 0 {
                return Err(InstructionFailure::Trap(GuestTrap::NullReference));
            }
            write_value(frame, op.dst, kind, value).map_err(fault)?;
        }
        opcode if (code::ARRAY_STORE + 1..=code::ARRAY_STORE + 7).contains(&opcode) => {
            let kind = (opcode - code::ARRAY_STORE) as u8;
            let array = read_value(frame, op.lhs, 7).map_err(fault)?;
            let index = read!(i32, op.rhs);
            let value = read_value(frame, op.dst, kind).map_err(fault)?;
            let (reference, array_type, element, length) = resolve_array(image, heap, array)?;
            let element_type = image
                .array_element_type(array_type)
                .ok_or(fault(VmFault::InvalidResolvedId))?;
            if !runtime_value_matches(image, heap, value, element_type) {
                return Err(fault(VmFault::InvalidReference));
            }
            let offset =
                array_element_offset(index, length, element).map_err(InstructionFailure::Trap)?;
            store_value(heap, reference, offset, element, value).map_err(fault)?;
        }
        _ => return Err(fault(VmFault::UnsupportedInstruction)),
    }
    Ok(())
}

fn read_reference(bits: u32) -> Result<Ref32, InstructionFailure> {
    Ref32::from_bits(bits).ok_or(InstructionFailure::Trap(GuestTrap::NullReference))
}

fn read_value(frame: &PreparedFrame<'_>, offset: u32, kind: u8) -> Result<RuntimeValue, VmFault> {
    macro_rules! read {
        ($ty:ty) => {
            <$ty>::from_le_bytes(frame.read(offset)?)
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
    frame: &mut PreparedFrame<'_>,
    offset: u32,
    kind: u8,
    value: RuntimeValue,
) -> Result<(), VmFault> {
    macro_rules! write {
        ($value:expr) => {
            frame.write(offset, ($value).to_le_bytes())
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

pub(super) fn reference_type(
    image: &ExecutionImage,
    heap: &Heap,
    reference: Ref32,
) -> Result<TypeKey, VmFault> {
    match reference.domain() {
        ReferenceDomain::Managed => heap.managed_type(reference).and_then(|type_id| {
            image
                .type_key(type_id as usize)
                .ok_or(VmFault::InvalidResolvedId)
        }),
        ReferenceDomain::External => image
            .reference_type(reference)
            .ok_or(VmFault::InvalidReference),
        ReferenceDomain::Image => image
            .reference_type(reference)
            .ok_or(VmFault::InvalidReference),
        ReferenceDomain::Reserved => Err(VmFault::InvalidReference),
    }
}

pub(super) fn runtime_value_matches(
    image: &ExecutionImage,
    heap: &Heap,
    value: RuntimeValue,
    expected: ResolvedValueType,
) -> bool {
    match value {
        RuntimeValue::I32(_) => expected.kind == 1,
        RuntimeValue::I64(_) => expected.kind == 2,
        RuntimeValue::F32(_) => expected.kind == 3,
        RuntimeValue::F64(_) => expected.kind == 4,
        RuntimeValue::Bool(_) => expected.kind == 5,
        RuntimeValue::Char(_) => expected.kind == 6,
        RuntimeValue::Null => expected.kind == 7 && expected.nullable,
        RuntimeValue::Reference(reference) if expected.kind == 7 => expected
            .nominal
            .and_then(|target| {
                reference_type(image, heap, reference)
                    .ok()
                    .map(|actual| (actual, target))
            })
            .is_some_and(|(actual, target)| image.is_assignable(actual, target)),
        RuntimeValue::Reference(_) => false,
    }
}

pub(super) fn resolve_array(
    image: &ExecutionImage,
    heap: &Heap,
    value: RuntimeValue,
) -> Result<(Ref32, TypeKey, ValueWidth, i32), InstructionFailure> {
    let reference = match value {
        RuntimeValue::Null => return Err(InstructionFailure::Trap(GuestTrap::NullReference)),
        RuntimeValue::Reference(reference) => reference,
        _ => return Err(InstructionFailure::Fault(VmFault::InvalidValueType)),
    };
    let ty = heap
        .managed_type(reference)
        .map_err(InstructionFailure::Fault)
        .and_then(|type_id| {
            image
                .type_key(type_id as usize)
                .ok_or(InstructionFailure::Fault(VmFault::InvalidResolvedId))
        })?;
    let element = match image.type_layout(ty) {
        Some(RuntimeTypeLayout::Array { element }) => *element,
        _ => return Err(InstructionFailure::Fault(VmFault::InvalidReference)),
    };
    let length =
        match load_value(heap, reference, 0, ValueWidth::I32).map_err(InstructionFailure::Fault)? {
            RuntimeValue::I32(length) if length >= 0 => length,
            _ => return Err(InstructionFailure::Fault(VmFault::CorruptHeap)),
        };
    Ok((reference, ty, element, length))
}
