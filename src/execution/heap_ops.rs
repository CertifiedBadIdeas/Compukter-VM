use super::{
    error::VmFault,
    heap::{AllocationRequest, Heap, ReservedAllocation},
    layout::ValueWidth,
    value::{ReferenceValue, RuntimeValue},
};

#[derive(Clone, Copy, Debug)]
pub(super) struct PendingState {
    pub request: AllocationRequest,
    pub reservation: ReservedAllocation,
    pub destination: u16,
    pub logical_bytes: u32,
    pub initialized_bytes: u32,
    pub fixed_cost_paid: bool,
    pub collection_attempted: bool,
}

pub(super) fn load_value(
    heap: &Heap,
    reference: ReferenceValue,
    offset: u32,
    width: ValueWidth,
) -> Result<RuntimeValue, VmFault> {
    let bytes = heap.read_payload(reference, offset, width.bytes())?;
    Ok(match width {
        ValueWidth::Bool => RuntimeValue::Bool(bytes[0] != 0),
        ValueWidth::Char => RuntimeValue::Char(u16::from_le_bytes(bytes[..2].try_into().unwrap())),
        ValueWidth::I32 => RuntimeValue::I32(i32::from_le_bytes(bytes[..4].try_into().unwrap())),
        ValueWidth::F32 => RuntimeValue::F32(u32::from_le_bytes(bytes[..4].try_into().unwrap())),
        ValueWidth::I64 => RuntimeValue::I64(i64::from_le_bytes(bytes)),
        ValueWidth::F64 => RuntimeValue::F64(u64::from_le_bytes(bytes)),
        ValueWidth::Ref => {
            let bits = u64::from_le_bytes(bytes);
            if bits == 0 {
                RuntimeValue::Null
            } else {
                RuntimeValue::Reference(ReferenceValue::from_bits(bits))
            }
        }
    })
}

pub(super) fn store_value(
    heap: &mut Heap,
    reference: ReferenceValue,
    offset: u32,
    width: ValueWidth,
    value: RuntimeValue,
) -> Result<(), VmFault> {
    match (width, value) {
        (ValueWidth::Bool, RuntimeValue::Bool(value)) => {
            heap.write_payload(reference, offset, &[u8::from(value)])
        }
        (ValueWidth::Char, RuntimeValue::Char(value)) => {
            heap.write_payload(reference, offset, &value.to_le_bytes())
        }
        (ValueWidth::I32, RuntimeValue::I32(value)) => {
            heap.write_payload(reference, offset, &value.to_le_bytes())
        }
        (ValueWidth::F32, RuntimeValue::F32(value)) => {
            heap.write_payload(reference, offset, &value.to_le_bytes())
        }
        (ValueWidth::I64, RuntimeValue::I64(value)) => {
            heap.write_payload(reference, offset, &value.to_le_bytes())
        }
        (ValueWidth::F64, RuntimeValue::F64(value)) => {
            heap.write_payload(reference, offset, &value.to_le_bytes())
        }
        (ValueWidth::Ref, RuntimeValue::Null) => {
            heap.write_payload(reference, offset, &0_u64.to_le_bytes())
        }
        (ValueWidth::Ref, RuntimeValue::Reference(value)) => {
            heap.write_payload(reference, offset, &value.to_bits().to_le_bytes())
        }
        _ => Err(VmFault::InvalidValueType),
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum PendingAllocation {
    Object(PendingState),
    Array { state: PendingState, length: u32 },
}

impl PendingAllocation {
    pub(super) fn state(self) -> PendingState {
        match self {
            Self::Object(state) | Self::Array { state, .. } => state,
        }
    }

    pub(super) fn initialized_bytes(self) -> u32 {
        self.state().initialized_bytes
    }

    pub(super) fn units_for_budget(self, budget: u32) -> u32 {
        let state = self.state();
        let remaining_bytes = state.logical_bytes.saturating_sub(state.initialized_bytes);
        budget.min(remaining_bytes.saturating_add(15) / 16)
    }

    pub(super) fn advance(
        &mut self,
        heap: &mut Heap,
        budget: u32,
    ) -> Result<(u32, Option<ReferenceValue>), VmFault> {
        let array_length = match *self {
            Self::Array { length, .. } => Some(length),
            Self::Object(_) => None,
        };
        let units = self.units_for_budget(budget);
        let state = match self {
            Self::Object(state) | Self::Array { state, .. } => state,
        };
        if !state.fixed_cost_paid {
            return Err(VmFault::CorruptLifecycle);
        }
        let physical_payload = state
            .request
            .block_bytes
            .checked_sub(16)
            .ok_or(VmFault::InvalidStoragePlan)?;
        if units != 0 {
            let end = state
                .initialized_bytes
                .checked_add(units.checked_mul(16).ok_or(VmFault::AccountingOverflow)?)
                .ok_or(VmFault::AccountingOverflow)?
                .min(physical_payload);
            heap.zero_reserved_payload(
                state.reservation,
                state.initialized_bytes,
                end - state.initialized_bytes,
            )?;
            state.initialized_bytes = state
                .initialized_bytes
                .checked_add(units * 16)
                .ok_or(VmFault::AccountingOverflow)?
                .min(state.logical_bytes);
        } else if state.logical_bytes == 0 {
            heap.zero_reserved_payload(state.reservation, 0, physical_payload)?;
        }
        if state.initialized_bytes < state.logical_bytes {
            return Ok((units, None));
        }
        if let Some(length) = array_length {
            heap.write_reserved_u32(state.reservation, 0, length)?;
        }
        let reference = heap.commit(state.reservation)?;
        Ok((units, Some(reference)))
    }

    pub(super) fn abort(self, heap: &mut Heap) -> Result<(), VmFault> {
        heap.abort(self.state().reservation)
    }
}
