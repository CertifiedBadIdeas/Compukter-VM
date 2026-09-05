use super::error::{AdmissionError, ResidentStorageComponent};

const BLOCK_HEADER_BYTES: u32 = 16;
const MANAGED_HEADER_BYTES: u32 = 8;
const INDEXED_HEADER_BYTES: u32 = 8;
const BLOCK_ALIGNMENT: u32 = 16;
const MINIMUM_BLOCK_BYTES: u32 = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ValueWidth {
    Bool,
    Char,
    I32,
    F32,
    I64,
    F64,
    Ref,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FieldSpec {
    pub field: u32,
    pub width: ValueWidth,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FieldLayout {
    pub field: u32,
    pub width: ValueWidth,
    pub offset: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ObjectLayout {
    pub payload_bytes: u32,
    pub block_bytes: u32,
    pub fields: Box<[FieldLayout]>,
    pub reference_offsets: Box<[u32]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ArrayLayout {
    pub element_bytes: u32,
    pub length: u32,
    pub payload_bytes: u32,
    pub block_bytes: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StringEncoding {
    Latin1,
    Utf16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StringLayout {
    pub encoding: StringEncoding,
    pub length: u32,
    pub payload_bytes: u32,
    pub block_bytes: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RuntimeTypeLayout {
    Object(ObjectLayout),
    Array { element: ValueWidth },
    NonHeap,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct StorageCharges {
    pub heap_arena_bytes: u64,
    pub heap_allocator_bytes: u64,
    pub frame_arena_bytes: u64,
    pub frame_record_bytes: u64,
    pub static_bytes: u64,
    pub type_initialization_bytes: u64,
    pub external_root_bytes: u64,
    pub pending_state_bytes: u64,
    pub machine_fixed_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct StoragePlan {
    pub heap_arena_bytes: u64,
    pub heap_allocator_bytes: u64,
    pub frame_arena_bytes: u64,
    pub frame_record_bytes: u64,
    pub static_bytes: u64,
    pub type_initialization_bytes: u64,
    pub external_root_bytes: u64,
    pub pending_state_bytes: u64,
    pub machine_fixed_bytes: u64,
    mutable_resident_bytes: u64,
}

impl StoragePlan {
    #[cfg(test)]
    pub(super) fn heap_only(heap_arena_bytes: u64) -> Self {
        Self::checked(StorageCharges {
            heap_arena_bytes,
            ..StorageCharges::default()
        })
        .expect("test heap storage plan must fit")
    }

    #[cfg(test)]
    pub(super) fn with_heap_arena_bytes(self, heap_arena_bytes: u64) -> Self {
        Self::checked(StorageCharges {
            heap_arena_bytes,
            heap_allocator_bytes: self.heap_allocator_bytes,
            frame_arena_bytes: self.frame_arena_bytes,
            frame_record_bytes: self.frame_record_bytes,
            static_bytes: self.static_bytes,
            type_initialization_bytes: self.type_initialization_bytes,
            external_root_bytes: self.external_root_bytes,
            pending_state_bytes: self.pending_state_bytes,
            machine_fixed_bytes: self.machine_fixed_bytes,
        })
        .expect("test storage plan must fit")
    }

    pub(super) fn checked(charges: StorageCharges) -> Result<Self, AdmissionError> {
        let StorageCharges {
            heap_arena_bytes,
            heap_allocator_bytes,
            frame_arena_bytes,
            frame_record_bytes,
            static_bytes,
            type_initialization_bytes,
            external_root_bytes,
            pending_state_bytes,
            machine_fixed_bytes,
        } = charges;
        let components = [
            (ResidentStorageComponent::HeapArena, heap_arena_bytes),
            (
                ResidentStorageComponent::HeapAllocator,
                heap_allocator_bytes,
            ),
            (ResidentStorageComponent::FrameArena, frame_arena_bytes),
            (ResidentStorageComponent::FrameRecords, frame_record_bytes),
            (ResidentStorageComponent::Statics, static_bytes),
            (
                ResidentStorageComponent::TypeInitialization,
                type_initialization_bytes,
            ),
            (ResidentStorageComponent::ExternalRoots, external_root_bytes),
            (ResidentStorageComponent::PendingState, pending_state_bytes),
            (
                ResidentStorageComponent::MachineFixedState,
                machine_fixed_bytes,
            ),
        ];
        let mutable_resident_bytes =
            components
                .into_iter()
                .try_fold(0_u64, |total, (component, bytes)| {
                    total
                        .checked_add(bytes)
                        .ok_or(AdmissionError::ResidentStorageOverflow { component })
                })?;
        Ok(Self {
            heap_arena_bytes,
            heap_allocator_bytes,
            frame_arena_bytes,
            frame_record_bytes,
            static_bytes,
            type_initialization_bytes,
            external_root_bytes,
            pending_state_bytes,
            machine_fixed_bytes,
            mutable_resident_bytes,
        })
    }

    pub(super) const fn mutable_resident_bytes(self) -> u64 {
        self.mutable_resident_bytes
    }
}

pub(super) fn empty_object_layout() -> Result<ObjectLayout, AdmissionError> {
    object_layout(None, &[])
}

pub(super) fn object_layout(
    superclass: Option<&ObjectLayout>,
    declared_fields: &[FieldSpec],
) -> Result<ObjectLayout, AdmissionError> {
    let inherited_count = superclass.map_or(0, |layout| layout.fields.len());
    let field_capacity = inherited_count
        .checked_add(declared_fields.len())
        .ok_or(AdmissionError::StoragePlanOverflow)?;
    let inherited_references = superclass.map_or(0, |layout| layout.reference_offsets.len());
    let declared_references = declared_fields
        .iter()
        .filter(|field| field.width == ValueWidth::Ref)
        .count();
    let reference_capacity = inherited_references
        .checked_add(declared_references)
        .ok_or(AdmissionError::StoragePlanOverflow)?;

    let mut fields = Vec::new();
    fields
        .try_reserve_exact(field_capacity)
        .map_err(|_| AdmissionError::AllocationFailed)?;
    let mut reference_offsets = Vec::new();
    reference_offsets
        .try_reserve_exact(reference_capacity)
        .map_err(|_| AdmissionError::AllocationFailed)?;

    let mut offset = 0;
    if let Some(superclass) = superclass {
        fields.extend_from_slice(&superclass.fields);
        reference_offsets.extend_from_slice(&superclass.reference_offsets);
        offset = superclass.payload_bytes;
    }

    let mut ordered_fields = Vec::new();
    ordered_fields
        .try_reserve_exact(declared_fields.len())
        .map_err(|_| AdmissionError::AllocationFailed)?;
    ordered_fields.extend(declared_fields.iter().copied().enumerate());
    ordered_fields.sort_unstable_by_key(|(declaration, field)| {
        (core::cmp::Reverse(field.width.bytes()), *declaration)
    });

    for (_, field) in ordered_fields {
        offset = align_up(offset, field.width.bytes())?;
        fields.push(FieldLayout {
            field: field.field,
            width: field.width,
            offset,
        });
        if field.width == ValueWidth::Ref {
            reference_offsets.push(offset);
        }
        offset = offset
            .checked_add(field.width.bytes())
            .ok_or(AdmissionError::StoragePlanOverflow)?;
    }

    Ok(ObjectLayout {
        payload_bytes: offset,
        block_bytes: block_bytes(offset)?,
        fields: fields.into_boxed_slice(),
        reference_offsets: reference_offsets.into_boxed_slice(),
    })
}

pub(super) fn array_layout(
    element: ValueWidth,
    length: i32,
) -> Result<ArrayLayout, AdmissionError> {
    let length = u32::try_from(length).map_err(|_| AdmissionError::StoragePlanOverflow)?;
    let element_bytes = element.bytes();
    let elements_bytes = element_bytes
        .checked_mul(length)
        .ok_or(AdmissionError::StoragePlanOverflow)?;
    let payload_bytes = INDEXED_HEADER_BYTES
        .checked_add(elements_bytes)
        .ok_or(AdmissionError::StoragePlanOverflow)?;
    Ok(ArrayLayout {
        element_bytes,
        length,
        payload_bytes,
        block_bytes: block_bytes(payload_bytes)?,
    })
}

pub(super) fn string_layout(
    encoding: StringEncoding,
    length: u32,
) -> Result<StringLayout, AdmissionError> {
    let characters_bytes = encoding
        .element_bytes()
        .checked_mul(length)
        .ok_or(AdmissionError::StoragePlanOverflow)?;
    let payload_bytes = INDEXED_HEADER_BYTES
        .checked_add(characters_bytes)
        .ok_or(AdmissionError::StoragePlanOverflow)?;
    Ok(StringLayout {
        encoding,
        length,
        payload_bytes,
        block_bytes: block_bytes(payload_bytes)?,
    })
}

impl ValueWidth {
    pub(super) const fn bytes(self) -> u32 {
        match self {
            Self::Bool => 1,
            Self::Char => 2,
            Self::I32 | Self::F32 | Self::Ref => 4,
            Self::I64 | Self::F64 => 8,
        }
    }
}

impl StringEncoding {
    const fn element_bytes(self) -> u32 {
        match self {
            Self::Latin1 => 1,
            Self::Utf16 => 2,
        }
    }
}

fn block_bytes(payload_bytes: u32) -> Result<u32, AdmissionError> {
    let unaligned = BLOCK_HEADER_BYTES
        .checked_add(MANAGED_HEADER_BYTES)
        .and_then(|value| value.checked_add(payload_bytes))
        .ok_or(AdmissionError::StoragePlanOverflow)?;
    Ok(align_up(unaligned, BLOCK_ALIGNMENT)?.max(MINIMUM_BLOCK_BYTES))
}

fn align_up(value: u32, alignment: u32) -> Result<u32, AdmissionError> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|sum| sum & !(alignment - 1))
        .ok_or(AdmissionError::StoragePlanOverflow)
}

#[cfg(test)]
mod tests {
    use super::{AdmissionError, ResidentStorageComponent, StorageCharges, StoragePlan};

    #[test]
    fn resident_total_overflow_names_the_component_that_crosses_the_bound() {
        assert_eq!(
            Err(AdmissionError::ResidentStorageOverflow {
                component: ResidentStorageComponent::HeapAllocator,
            }),
            StoragePlan::checked(StorageCharges {
                heap_arena_bytes: u64::MAX,
                heap_allocator_bytes: 1,
                ..StorageCharges::default()
            }),
        );
    }
}
