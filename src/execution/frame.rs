/*
 * The Compukters Developers
 *
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use crate::artifact::{FunctionValue, PhysicalAtom, ValueComponent};

use super::{error::VmFault, value::Ref32};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ComponentLayout {
    pub offset: u32,
    pub atom: PhysicalAtom,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ValueLayout {
    pub components: Box<[ComponentLayout]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FrameLayout {
    pub byte_len: u32,
    pub alignment: u8,
    pub values: Box<[ValueLayout]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SafepointMap {
    pub reference_offsets: Box<[u32]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FrameReservation {
    pub base: u32,
    pub byte_len: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PhysicalValue {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Ref32(Option<Ref32>),
}

impl PhysicalValue {
    const fn atom(self) -> PhysicalAtom {
        match self {
            Self::I32(_) => PhysicalAtom::I32,
            Self::I64(_) => PhysicalAtom::I64,
            Self::F32(_) => PhysicalAtom::F32,
            Self::F64(_) => PhysicalAtom::F64,
            Self::Ref32(_) => PhysicalAtom::Ref32,
        }
    }
}

pub(super) struct FrameArena {
    bytes: Box<[u8]>,
    used: u32,
    #[cfg(test)]
    initialized: Box<[bool]>,
}

impl FrameArena {
    pub(super) fn new(capacity: u32) -> Result<Self, VmFault> {
        let length = usize::try_from(capacity).map_err(|_| VmFault::InvalidStoragePlan)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| VmFault::InvalidStoragePlan)?;
        bytes.resize(length, 0);
        Ok(Self {
            bytes: bytes.into_boxed_slice(),
            used: 0,
            #[cfg(test)]
            initialized: vec![false; length].into_boxed_slice(),
        })
    }

    pub(super) fn push(&mut self, layout: &FrameLayout) -> Result<FrameReservation, VmFault> {
        let byte_len = align(layout.byte_len, 8).map_err(|_| VmFault::InvalidStoragePlan)?;
        let end = self
            .used
            .checked_add(byte_len)
            .filter(|end| *end <= self.bytes.len() as u32)
            .ok_or(VmFault::InvalidStoragePlan)?;
        let reservation = FrameReservation {
            base: self.used,
            byte_len,
        };
        #[cfg(test)]
        self.initialized[reservation.base as usize..end as usize].fill(false);
        for value in &layout.values {
            for component in &value.components {
                if component.atom == PhysicalAtom::Ref32 {
                    let range = self.component_range(reservation.base, component)?;
                    self.bytes[range].fill(0);
                }
            }
        }
        self.used = end;
        Ok(reservation)
    }

    pub(super) fn pop(&mut self, frame: FrameReservation) -> Result<(), VmFault> {
        if frame
            .base
            .checked_add(frame.byte_len)
            .filter(|end| *end == self.used)
            .is_none()
        {
            return Err(VmFault::CorruptLifecycle);
        }
        self.used = frame.base;
        Ok(())
    }

    pub(super) fn read_i32(
        &self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
    ) -> Result<i32, VmFault> {
        Ok(i32::from_le_bytes(self.read4(
            base,
            layout,
            value,
            component,
            PhysicalAtom::I32,
        )?))
    }

    pub(super) fn read_i64(
        &self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
    ) -> Result<i64, VmFault> {
        Ok(i64::from_le_bytes(self.read8(
            base,
            layout,
            value,
            component,
            PhysicalAtom::I64,
        )?))
    }

    pub(super) fn read_f32(
        &self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
    ) -> Result<u32, VmFault> {
        Ok(u32::from_le_bytes(self.read4(
            base,
            layout,
            value,
            component,
            PhysicalAtom::F32,
        )?))
    }

    pub(super) fn read_f64(
        &self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
    ) -> Result<u64, VmFault> {
        Ok(u64::from_le_bytes(self.read8(
            base,
            layout,
            value,
            component,
            PhysicalAtom::F64,
        )?))
    }

    pub(super) fn read_ref32(
        &self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
    ) -> Result<Option<Ref32>, VmFault> {
        Ok(Ref32::from_bits(u32::from_le_bytes(self.read4(
            base,
            layout,
            value,
            component,
            PhysicalAtom::Ref32,
        )?)))
    }

    pub(super) fn write_i32(
        &mut self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
        stored: i32,
    ) -> Result<(), VmFault> {
        self.write_component(base, layout, value, component, PhysicalValue::I32(stored))
    }

    pub(super) fn write_i64(
        &mut self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
        stored: i64,
    ) -> Result<(), VmFault> {
        self.write_component(base, layout, value, component, PhysicalValue::I64(stored))
    }

    pub(super) fn write_f32(
        &mut self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
        stored: u32,
    ) -> Result<(), VmFault> {
        self.write_component(base, layout, value, component, PhysicalValue::F32(stored))
    }

    pub(super) fn write_f64(
        &mut self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
        stored: u64,
    ) -> Result<(), VmFault> {
        self.write_component(base, layout, value, component, PhysicalValue::F64(stored))
    }

    pub(super) fn write_ref32(
        &mut self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
        stored: Option<Ref32>,
    ) -> Result<(), VmFault> {
        self.write_component(base, layout, value, component, PhysicalValue::Ref32(stored))
    }

    pub(super) fn write_value(
        &mut self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        stored: &[PhysicalValue],
    ) -> Result<(), VmFault> {
        let components = &layout
            .values
            .get(value)
            .ok_or(VmFault::InvalidStoragePlan)?
            .components;
        if components.len() != stored.len() {
            return Err(VmFault::InvalidValueType);
        }
        for (component, stored) in components.iter().zip(stored) {
            if component.atom != stored.atom() {
                return Err(VmFault::InvalidValueType);
            }
            self.component_range(base, component)?;
        }
        for (component, stored) in components.iter().zip(stored) {
            let range = self.component_range(base, component)?;
            match stored {
                PhysicalValue::I32(value) => {
                    self.bytes[range].copy_from_slice(&value.to_le_bytes())
                }
                PhysicalValue::I64(value) => {
                    self.bytes[range].copy_from_slice(&value.to_le_bytes())
                }
                PhysicalValue::F32(value) => {
                    self.bytes[range].copy_from_slice(&value.to_le_bytes())
                }
                PhysicalValue::F64(value) => {
                    self.bytes[range].copy_from_slice(&value.to_le_bytes())
                }
                PhysicalValue::Ref32(value) => self.bytes[range]
                    .copy_from_slice(&value.map_or(0, Ref32::to_bits).to_le_bytes()),
            }
            #[cfg(test)]
            {
                let initialized = self.component_range(base, component)?;
                self.initialized[initialized].fill(true);
            }
        }
        Ok(())
    }

    fn write_component(
        &mut self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
        stored: PhysicalValue,
    ) -> Result<(), VmFault> {
        let component = self.component(layout, value, component, stored.atom())?;
        let range = self.component_range(base, component)?;
        match stored {
            PhysicalValue::I32(value) => self.bytes[range].copy_from_slice(&value.to_le_bytes()),
            PhysicalValue::I64(value) => self.bytes[range].copy_from_slice(&value.to_le_bytes()),
            PhysicalValue::F32(value) => self.bytes[range].copy_from_slice(&value.to_le_bytes()),
            PhysicalValue::F64(value) => self.bytes[range].copy_from_slice(&value.to_le_bytes()),
            PhysicalValue::Ref32(value) => {
                self.bytes[range].copy_from_slice(&value.map_or(0, Ref32::to_bits).to_le_bytes())
            }
        }
        #[cfg(test)]
        {
            let initialized = self.component_range(base, component)?;
            self.initialized[initialized].fill(true);
        }
        Ok(())
    }

    fn read4(
        &self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
        atom: PhysicalAtom,
    ) -> Result<[u8; 4], VmFault> {
        let component = self.component(layout, value, component, atom)?;
        self.bytes[self.component_range(base, component)?]
            .try_into()
            .map_err(|_| VmFault::InvalidStoragePlan)
    }

    fn read8(
        &self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
        component: usize,
        atom: PhysicalAtom,
    ) -> Result<[u8; 8], VmFault> {
        let component = self.component(layout, value, component, atom)?;
        self.bytes[self.component_range(base, component)?]
            .try_into()
            .map_err(|_| VmFault::InvalidStoragePlan)
    }

    fn component<'a>(
        &self,
        layout: &'a FrameLayout,
        value: usize,
        component: usize,
        atom: PhysicalAtom,
    ) -> Result<&'a ComponentLayout, VmFault> {
        layout
            .values
            .get(value)
            .and_then(|value| value.components.get(component))
            .filter(|component| component.atom == atom)
            .ok_or(VmFault::InvalidValueType)
    }

    fn component_range(
        &self,
        base: u32,
        component: &ComponentLayout,
    ) -> Result<core::ops::Range<usize>, VmFault> {
        let start = base
            .checked_add(component.offset)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(VmFault::InvalidStoragePlan)?;
        let end = start
            .checked_add(component.atom.byte_size() as usize)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(VmFault::InvalidStoragePlan)?;
        Ok(start..end)
    }

    #[cfg(test)]
    pub(super) fn reserved_bytes(&self) -> usize {
        self.bytes.len()
    }

    #[cfg(test)]
    pub(super) fn is_value_initialized(
        &self,
        base: u32,
        layout: &FrameLayout,
        value: usize,
    ) -> bool {
        layout.values.get(value).is_some_and(|value| {
            !value.components.is_empty()
                && value.components.iter().all(|component| {
                    self.component_range(base, component)
                        .is_ok_and(|range| self.initialized[range].iter().all(|byte| *byte))
                })
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FrameLayoutError {
    Overflow,
    InvalidRoot,
}

impl PhysicalAtom {
    pub(super) const fn byte_size(self) -> u32 {
        match self {
            Self::I32 | Self::F32 | Self::Ref32 => 4,
            Self::I64 | Self::F64 => 8,
        }
    }

    const fn alignment(self) -> u32 {
        self.byte_size()
    }
}

impl FrameLayout {
    pub(super) fn derive(values: &[FunctionValue]) -> Result<Self, FrameLayoutError> {
        let mut offset = 0_u32;
        let mut frame_alignment = 1_u32;
        let mut layouts = Vec::new();
        layouts
            .try_reserve_exact(values.len())
            .map_err(|_| FrameLayoutError::Overflow)?;
        for value in values {
            let mut components = Vec::new();
            components
                .try_reserve_exact(value.components.len())
                .map_err(|_| FrameLayoutError::Overflow)?;
            for atom in &value.components {
                let alignment = atom.alignment();
                frame_alignment = frame_alignment.max(alignment);
                offset = align(offset, alignment)?;
                components.push(ComponentLayout {
                    offset,
                    atom: *atom,
                });
                offset = offset
                    .checked_add(atom.byte_size())
                    .ok_or(FrameLayoutError::Overflow)?;
            }
            layouts.push(ValueLayout {
                components: components.into_boxed_slice(),
            });
        }
        Ok(Self {
            byte_len: align(offset, frame_alignment)?,
            alignment: frame_alignment as u8,
            values: layouts.into_boxed_slice(),
        })
    }
}

impl SafepointMap {
    pub(super) fn derive(
        frame: &FrameLayout,
        references: &[ValueComponent],
    ) -> Result<Self, FrameLayoutError> {
        let mut offsets = Vec::new();
        offsets
            .try_reserve_exact(references.len())
            .map_err(|_| FrameLayoutError::Overflow)?;
        for reference in references {
            let component = frame
                .values
                .get(reference.value as usize)
                .and_then(|value| value.components.get(reference.component as usize))
                .filter(|component| component.atom == PhysicalAtom::Ref32)
                .ok_or(FrameLayoutError::InvalidRoot)?;
            if offsets
                .last()
                .is_some_and(|previous| *previous >= component.offset)
            {
                return Err(FrameLayoutError::InvalidRoot);
            }
            offsets.push(component.offset);
        }
        Ok(Self {
            reference_offsets: offsets.into_boxed_slice(),
        })
    }
}

fn align(value: u32, alignment: u32) -> Result<u32, FrameLayoutError> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or(FrameLayoutError::Overflow)
}

#[cfg(test)]
mod tests {
    use crate::artifact::{FunctionValue, PhysicalAtom, TypeId, ValueComponent, ValueType};

    use super::{align, FrameArena, FrameLayout, FrameLayoutError, PhysicalValue, SafepointMap};
    use crate::execution::{error::VmFault, value::Ref32};

    fn value(components: Vec<PhysicalAtom>) -> FunctionValue {
        FunctionValue {
            semantic_type: ValueType {
                kind: 1,
                flags: 0,
                nominal_type: TypeId(u32::MAX),
            },
            components,
        }
    }

    #[test]
    fn frame_layout_aligns_mixed_values_and_keeps_ref32_four_bytes_wide() {
        let layout = FrameLayout::derive(&[
            value(vec![PhysicalAtom::I32]),
            value(vec![PhysicalAtom::I64]),
            value(vec![PhysicalAtom::Ref32]),
        ])
        .unwrap();

        assert_eq!(layout.byte_len, 24);
        assert_eq!(layout.alignment, 8);
        assert_eq!(layout.values[0].components[0].offset, 0);
        assert_eq!(layout.values[1].components[0].offset, 8);
        assert_eq!(layout.values[2].components[0].offset, 16);
        assert_eq!(layout.values[2].components[0].atom.byte_size(), 4);
    }

    #[test]
    fn safepoint_map_derives_offsets_for_reference_components_only() {
        let layout =
            FrameLayout::derive(&[value(vec![PhysicalAtom::I32, PhysicalAtom::Ref32])]).unwrap();

        let map = SafepointMap::derive(
            &layout,
            &[ValueComponent {
                value: 0,
                component: 1,
            }],
        )
        .unwrap();

        assert_eq!(&*map.reference_offsets, &[4]);
        assert!(SafepointMap::derive(
            &layout,
            &[ValueComponent {
                value: 0,
                component: 0,
            }],
        )
        .is_err());
    }

    #[test]
    fn frame_offset_arithmetic_rejects_overflow() {
        assert_eq!(align(u32::MAX, 8), Err(FrameLayoutError::Overflow));
    }

    #[test]
    fn arena_push_pop_reuses_bytes_and_clears_reference_components() {
        let layout = FrameLayout::derive(&[
            value(vec![PhysicalAtom::I32]),
            value(vec![PhysicalAtom::I64]),
            value(vec![PhysicalAtom::Ref32]),
        ])
        .unwrap();
        let mut arena = FrameArena::new(24).unwrap();
        let frame = arena.push(&layout).unwrap();
        assert_eq!(None, arena.read_ref32(frame.base, &layout, 2, 0).unwrap());
        arena.write_i32(frame.base, &layout, 0, 0, 41).unwrap();
        arena
            .write_i64(frame.base, &layout, 1, 0, i64::MIN)
            .unwrap();
        arena
            .write_ref32(frame.base, &layout, 2, 0, Ref32::managed(16))
            .unwrap();
        assert_eq!(41, arena.read_i32(frame.base, &layout, 0, 0).unwrap());
        assert_eq!(i64::MIN, arena.read_i64(frame.base, &layout, 1, 0).unwrap());
        assert_eq!(
            Some(Ref32::managed(16).unwrap()),
            arena.read_ref32(frame.base, &layout, 2, 0).unwrap()
        );

        arena.pop(frame).unwrap();
        let reused = arena.push(&layout).unwrap();
        assert_eq!(frame, reused);
        assert_eq!(None, arena.read_ref32(reused.base, &layout, 2, 0).unwrap());
    }

    #[test]
    fn arena_rejects_wrong_shapes_and_overflowing_pushes() {
        let layout = FrameLayout::derive(&[value(vec![PhysicalAtom::I32])]).unwrap();
        let mut arena = FrameArena::new(8).unwrap();
        let frame = arena.push(&layout).unwrap();
        assert_eq!(
            Err(VmFault::InvalidValueType),
            arena.read_i64(frame.base, &layout, 0, 0)
        );
        assert_eq!(Err(VmFault::InvalidStoragePlan), arena.push(&layout));
    }

    #[test]
    fn multi_component_write_is_atomic() {
        let layout =
            FrameLayout::derive(&[value(vec![PhysicalAtom::I32, PhysicalAtom::Ref32])]).unwrap();
        let mut arena = FrameArena::new(8).unwrap();
        let frame = arena.push(&layout).unwrap();
        arena
            .write_value(
                frame.base,
                &layout,
                0,
                &[PhysicalValue::I32(7), PhysicalValue::Ref32(None)],
            )
            .unwrap();

        assert_eq!(
            Err(VmFault::InvalidValueType),
            arena.write_value(
                frame.base,
                &layout,
                0,
                &[PhysicalValue::I32(9), PhysicalValue::I64(11)],
            )
        );
        assert_eq!(7, arena.read_i32(frame.base, &layout, 0, 0).unwrap());
    }
}
