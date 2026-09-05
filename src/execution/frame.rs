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

    use super::{align, FrameLayout, FrameLayoutError, SafepointMap};

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
}
