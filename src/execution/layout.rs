use super::error::AdmissionError;

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
pub(super) struct StoragePlan {
    pub heap_bytes: u32,
    pub handle_capacity: u32,
    pub type_count: u32,
    pub field_count: u32,
    pub static_slot_count: u32,
    pub literal_count: u32,
    pub literal_id_count: u32,
    pub reference_offset_count: u32,
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
