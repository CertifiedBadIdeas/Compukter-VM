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

use super::*;
use crate::artifact::ByteRange;
use crate::execution::checkpoint::{checkpoint_enum, checkpoint_struct, CheckpointError, Result};

checkpoint_enum!(StringBacking {
    0 => Inline { units, start, length };
    1 => Literal(v0);
    2 => TypeName { index, length };
    3 => Managed { reference, length, encoding };
    4 => CharArray { reference, length };
});

checkpoint_enum!(PendingText {
    0 => Hash { value, index, hash, destination };
    1 => Equals { lhs, rhs, index, destination };
    2 => Compare { lhs, rhs, index, destination };
});

checkpoint_struct!(PendingConcat {
    lhs,
    lhs_start,
    lhs_length,
    rhs,
    rhs_start,
    rhs_length,
    destination,
    scan,
    latin1,
    reservation,
    layout,
    written,
    collection_attempted
});

checkpoint_enum!(HostReferenceLayout { 0 => String(v0); 1 => ByteArray(v0); });

checkpoint_struct!(PendingHostReference {
    destination,
    byte_array_type,
    scan,
    latin1,
    reservation,
    layout,
    written,
    collection_attempted
});

checkpoint_struct!(ResolvedLiteral { bytes, code_units });
checkpoint_struct!(ByteRange { start, end });

impl StringBacking {
    pub(in crate::execution) fn validate_checkpoint(
        self,
        image: &ExecutionImage,
        heap: &Heap,
    ) -> Result<()> {
        let invalid = || CheckpointError::InvalidState;
        let valid = match self {
            Self::Inline { start, length, .. } => {
                usize::from(start) + usize::from(length) <= INLINE_SCALAR_UNITS
            }
            Self::Literal(literal) => {
                (0..)
                    .map_while(|index| image.literal(index))
                    .any(|expected| {
                        expected.bytes == literal.bytes && expected.code_units == literal.code_units
                    })
            }
            Self::TypeName { index, length } => image
                .type_name(index)
                .is_some_and(|units| units.len() == length as usize),
            Self::Managed {
                reference,
                length,
                encoding,
            } => match backing(image, heap, RuntimeValue::Reference(reference)) {
                Ok(Self::Managed {
                    length: actual,
                    encoding: actual_encoding,
                    ..
                }) => actual == length && encoding == actual_encoding,
                _ => false,
            },
            Self::CharArray { reference, length } => {
                heap.read_payload(reference, 0, 8).is_ok_and(|header| {
                    u32::from_le_bytes(header[..4].try_into().unwrap()) == length
                }) && length
                    .checked_mul(2)
                    .is_some_and(|bytes| heap.read_payload(reference, 8, bytes).is_ok())
            }
        };
        if valid {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}

impl PendingText {
    pub(in crate::execution) fn validate_checkpoint(
        self,
        image: &ExecutionImage,
        heap: &Heap,
    ) -> Result<u16> {
        let (index, length, destination) = match self {
            Self::Hash {
                value,
                index,
                destination,
                ..
            } => {
                value.validate_checkpoint(image, heap)?;
                (index, value.length(), destination)
            }
            Self::Equals {
                lhs,
                rhs,
                index,
                destination,
            }
            | Self::Compare {
                lhs,
                rhs,
                index,
                destination,
            } => {
                lhs.validate_checkpoint(image, heap)?;
                rhs.validate_checkpoint(image, heap)?;
                (index, lhs.length().min(rhs.length()), destination)
            }
        };
        if index > length {
            return Err(CheckpointError::InvalidState);
        }
        Ok(destination)
    }
}

fn validate_string_allocation(
    heap: &Heap,
    length: u32,
    scan: u32,
    latin1: bool,
    reservation: Option<ReservedAllocation>,
    layout: Option<StringLayout>,
    written: u32,
) -> Result<()> {
    if scan > length || reservation.is_some() != layout.is_some() {
        return Err(CheckpointError::InvalidState);
    }
    if let (Some(reservation), Some(layout)) = (reservation, layout) {
        let encoding = if latin1 {
            StringEncoding::Latin1
        } else {
            StringEncoding::Utf16
        };
        let expected = string_layout(encoding, length, heap.header_format())
            .map_err(|_| CheckpointError::InvalidState)?;
        if scan != length
            || layout != expected
            || written
                > layout
                    .block_bytes
                    .checked_sub(heap.payload_offset())
                    .ok_or(CheckpointError::InvalidState)?
            || heap.checkpoint_reservation_capacity(reservation)? < layout.payload_bytes
        {
            return Err(CheckpointError::InvalidState);
        }
    } else if written != 0 {
        return Err(CheckpointError::InvalidState);
    }
    Ok(())
}

impl PendingConcat {
    pub(in crate::execution) fn validate_checkpoint(
        self,
        image: &ExecutionImage,
        heap: &Heap,
    ) -> Result<u16> {
        self.lhs.validate_checkpoint(image, heap)?;
        self.rhs.validate_checkpoint(image, heap)?;
        if self
            .lhs_start
            .checked_add(self.lhs_length)
            .is_none_or(|end| end > self.lhs.length())
            || self
                .rhs_start
                .checked_add(self.rhs_length)
                .is_none_or(|end| end > self.rhs.length())
        {
            return Err(CheckpointError::InvalidState);
        }
        let length = self
            .lhs_length
            .checked_add(self.rhs_length)
            .ok_or(CheckpointError::InvalidState)?;
        validate_string_allocation(
            heap,
            length,
            self.scan,
            self.latin1,
            self.reservation,
            self.layout,
            self.written,
        )?;
        Ok(self.destination)
    }
}

impl PendingHostReference {
    pub(in crate::execution) fn validate_checkpoint(
        self,
        image: &ExecutionImage,
        heap: &Heap,
        source: HostReferenceSource<'_>,
    ) -> Result<u16> {
        let length = u32::try_from(source.len()).map_err(|_| CheckpointError::InvalidState)?;
        if source.is_bytes() != self.byte_array_type.is_some()
            || self.scan > length
            || self.reservation.is_some() != self.layout.is_some()
        {
            return Err(CheckpointError::InvalidState);
        }
        if let Some(ty) = self.byte_array_type {
            if length as usize > crate::execution::MAXIMUM_HOST_BYTE_ARRAY_BYTES
                || !matches!(
                    image.type_layout(ty),
                    Some(super::super::layout::RuntimeTypeLayout::Array {
                        element: super::super::layout::ValueWidth::I8
                    })
                )
            {
                return Err(CheckpointError::InvalidState);
            }
        }
        if let (Some(reservation), Some(layout)) = (self.reservation, self.layout) {
            let expected = if source.is_bytes() {
                HostReferenceLayout::ByteArray(
                    super::super::layout::array_layout(
                        super::super::layout::ValueWidth::I8,
                        length as i32,
                        heap.header_format(),
                    )
                    .map_err(|_| CheckpointError::InvalidState)?,
                )
            } else {
                HostReferenceLayout::String(
                    string_layout(
                        if self.latin1 {
                            StringEncoding::Latin1
                        } else {
                            StringEncoding::Utf16
                        },
                        length,
                        heap.header_format(),
                    )
                    .map_err(|_| CheckpointError::InvalidState)?,
                )
            };
            if self.scan != length
                || layout != expected
                || self.written
                    > layout
                        .block_bytes()
                        .checked_sub(heap.payload_offset())
                        .ok_or(CheckpointError::InvalidState)?
                || heap.checkpoint_reservation_capacity(reservation)? < layout.payload_bytes()
            {
                return Err(CheckpointError::InvalidState);
            }
        } else if self.written != 0 {
            return Err(CheckpointError::InvalidState);
        }
        Ok(self.destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::fixtures;

    #[test]
    fn checkpoint_rejects_foreign_literal_and_invalid_inline_range() {
        let image = ExecutionImage::admit(
            fixtures::literal_string_concat_artifact(),
            fixtures::profile(),
        )
        .unwrap();
        let heap = Heap::new(&image.storage_plan()).unwrap();
        assert_eq!(
            Err(CheckpointError::InvalidState),
            StringBacking::Inline {
                units: [0; INLINE_SCALAR_UNITS],
                start: 23,
                length: 2,
            }
            .validate_checkpoint(&image, &heap)
        );
        assert_eq!(
            Err(CheckpointError::InvalidState),
            StringBacking::Literal(ResolvedLiteral {
                bytes: ByteRange {
                    start: usize::MAX,
                    end: usize::MAX
                },
                code_units: 1,
            })
            .validate_checkpoint(&image, &heap)
        );
        let mut pending = PendingHostReference::new(0);
        pending.scan = 2;
        assert_eq!(
            Err(CheckpointError::InvalidState),
            pending.validate_checkpoint(&image, &heap, HostReferenceSource::String(&[0x61]))
        );
        pending.scan = 0;
        pending.written = 1;
        assert_eq!(
            Err(CheckpointError::InvalidState),
            pending.validate_checkpoint(&image, &heap, HostReferenceSource::String(&[0x61]))
        );
    }
}
