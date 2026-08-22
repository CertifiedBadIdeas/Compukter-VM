use super::{
    error::AdmissionError,
    fixtures,
    image::{deduplicate_literal_ranges, ExecutionImage},
    layout::{
        array_layout, empty_object_layout, object_layout, string_layout, FieldSpec,
        RuntimeTypeLayout, StoragePlan, StringEncoding, ValueWidth,
    },
    TypeKey,
};
use crate::artifact::ByteRange;

#[test]
fn portable_minimum_and_representative_layouts() -> Result<(), AdmissionError> {
    assert_eq!(32, empty_object_layout()?.block_bytes);
    assert_eq!(48, array_layout(ValueWidth::Char, 9)?.block_bytes);
    assert_eq!(48, array_layout(ValueWidth::Ref, 3)?.block_bytes);
    assert_eq!(32, string_layout(StringEncoding::Latin1, 8)?.block_bytes);
    assert_eq!(48, string_layout(StringEncoding::Utf16, 8)?.block_bytes);
    Ok(())
}

#[test]
fn portable_object_fields_use_stable_natural_alignment_groups() -> Result<(), AdmissionError> {
    let layout = object_layout(
        None,
        &[
            FieldSpec {
                field: 0,
                width: ValueWidth::Bool,
            },
            FieldSpec {
                field: 1,
                width: ValueWidth::Char,
            },
            FieldSpec {
                field: 2,
                width: ValueWidth::I64,
            },
            FieldSpec {
                field: 3,
                width: ValueWidth::Ref,
            },
        ],
    )?;

    let offsets: Vec<_> = layout
        .fields
        .iter()
        .map(|field| (field.field, field.offset))
        .collect();
    assert_eq!(vec![(2, 0), (3, 8), (1, 16), (0, 18)], offsets);
    assert_eq!(&[8], layout.reference_offsets.as_ref());
    assert_eq!(19, layout.payload_bytes);
    assert_eq!(48, layout.block_bytes);
    Ok(())
}

#[test]
fn portable_subclass_preserves_the_superclass_prefix() -> Result<(), AdmissionError> {
    let superclass = object_layout(
        None,
        &[
            FieldSpec {
                field: 10,
                width: ValueWidth::Char,
            },
            FieldSpec {
                field: 11,
                width: ValueWidth::Bool,
            },
        ],
    )?;
    let subclass = object_layout(
        Some(&superclass),
        &[
            FieldSpec {
                field: 20,
                width: ValueWidth::I64,
            },
            FieldSpec {
                field: 21,
                width: ValueWidth::Ref,
            },
        ],
    )?;

    assert_eq!(&superclass.fields[..], &subclass.fields[..2]);
    assert_eq!(8, subclass.fields[2].offset);
    assert_eq!(16, subclass.fields[3].offset);
    assert_eq!(&[16], subclass.reference_offsets.as_ref());
    assert_eq!(24, subclass.payload_bytes);
    assert_eq!(48, subclass.block_bytes);

    Ok(())
}

#[test]
fn portable_literal_deduplication_uses_exact_raw_bytes() -> Result<(), AdmissionError> {
    let bytes = b"a\0b\0a\0b\0c\0";
    let ranges = [
        ByteRange { start: 0, end: 4 },
        ByteRange { start: 4, end: 8 },
        ByteRange { start: 8, end: 10 },
    ];
    let (literals, ids) = deduplicate_literal_ranges(bytes, &ranges)?;

    assert_eq!(2, literals.len());
    assert_eq!(&[0, 0, 1], ids.as_ref());
    assert_eq!(2, literals[0].code_units);
    assert_eq!(1, literals[1].code_units);
    Ok(())
}

#[test]
fn portable_block_alignment_covers_15_16_17_byte_edges() -> Result<(), AdmissionError> {
    for (field_count, expected) in [(15, 32), (16, 32), (17, 48)] {
        let fields: Vec<_> = (0..field_count)
            .map(|field| FieldSpec {
                field,
                width: ValueWidth::Bool,
            })
            .collect();
        assert_eq!(expected, object_layout(None, &fields)?.block_bytes);
    }
    Ok(())
}

#[test]
fn portable_lengths_are_checked() {
    assert_eq!(
        Err(AdmissionError::StoragePlanOverflow),
        array_layout(ValueWidth::I64, -1)
    );
    assert_eq!(
        Err(AdmissionError::StoragePlanOverflow),
        array_layout(ValueWidth::I64, i32::MAX)
    );
    assert_eq!(
        Err(AdmissionError::StoragePlanOverflow),
        string_layout(StringEncoding::Utf16, u32::MAX)
    );
}

#[test]
fn portable_admission_publishes_exact_layout_metadata() -> Result<(), AdmissionError> {
    let mut profile = fixtures::profile();
    profile.heap_bytes = 1024;
    let image = ExecutionImage::admit(fixtures::portable_layout_artifact(), profile)?;

    assert_eq!(
        StoragePlan {
            heap_bytes: 1024,
            handle_capacity: 32,
            type_count: 4,
            field_count: 5,
            static_slot_count: 1,
            literal_count: 1,
            literal_id_count: 1,
            reference_offset_count: 1,
        },
        image.storage_plan()
    );

    let RuntimeTypeLayout::Object(subclass) = image
        .type_layout(TypeKey { module: 0, ty: 2 })
        .expect("subclass layout")
    else {
        panic!("subclass must have an object layout");
    };
    let offsets: Vec<_> = subclass
        .fields
        .iter()
        .map(|field| (field.field, field.offset))
        .collect();
    assert_eq!(vec![(0, 0), (1, 8), (3, 16), (2, 24)], offsets);
    assert_eq!(&[16], subclass.reference_offsets.as_ref());
    assert_eq!(48, subclass.block_bytes);

    let static_field = image.field(4).expect("resolved static field");
    assert_eq!(None, static_field.offset);
    assert_eq!(Some(0), static_field.static_slot);

    let literal = *image.literal(0).expect("resolved literal");
    assert_eq!(2, literal.code_units);
    assert_eq!(4, image.literal_bytes(literal).len());

    assert_eq!(
        Some(&RuntimeTypeLayout::Array {
            element: ValueWidth::Char,
        }),
        image.type_layout(TypeKey { module: 0, ty: 3 })
    );
    Ok(())
}

#[test]
fn portable_admission_rejects_unaligned_or_too_small_heap() {
    for heap_bytes in [0, 16, 31, 33, 47] {
        let mut profile = fixtures::profile();
        profile.heap_bytes = heap_bytes;
        assert_eq!(
            Err(AdmissionError::InvalidHeapSize {
                supplied: heap_bytes,
            }),
            ExecutionImage::admit(fixtures::scalar_artifact(), profile).map(|_| ())
        );
    }
}
