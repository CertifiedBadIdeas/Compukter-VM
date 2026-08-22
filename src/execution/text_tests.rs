use super::{
    error::{GuestTrap, Outcome},
    fixtures,
    image::deduplicate_literal_ranges,
    value::{ReferenceDomain, RuntimeValue},
};
use crate::artifact::ByteRange;

#[test]
fn string_literal_loads_as_an_immortal_reference() {
    let mut machine = fixtures::started_zero_arg(fixtures::literal_string_artifact());
    let Outcome::Halted(Some(RuntimeValue::Reference(reference))) =
        machine.run_slice(16, 0).unwrap()
    else {
        panic!("string program did not return a reference");
    };

    assert_eq!(ReferenceDomain::Literal, reference.domain());
    assert_eq!(0, reference.generation());
}

#[test]
fn string_length_counts_utf16_code_units() {
    let mut machine = fixtures::started_zero_arg(fixtures::literal_string_length_artifact());

    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::I32(2))),
        machine.run_slice(16, 0).unwrap()
    );
}

#[test]
fn string_get_returns_a_utf16_code_unit() {
    let mut machine = fixtures::started_zero_arg(fixtures::literal_string_get_artifact(1));

    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::Char(u16::from(b'i')))),
        machine.run_slice(16, 0).unwrap()
    );
}

#[test]
fn string_get_traps_outside_utf16_bounds() {
    for index in [-1, 2] {
        let mut machine = fixtures::started_zero_arg(fixtures::literal_string_get_artifact(index));
        assert_eq!(
            Outcome::Crashed(GuestTrap::IndexOutOfBounds),
            machine.run_slice(16, 0).unwrap()
        );
    }
}

#[test]
fn string_content_operations_use_kotlin_utf16_semantics() {
    let mut equals = fixtures::started_zero_arg(fixtures::literal_string_equals_artifact());
    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::Bool(true))),
        equals.run_slice(16, 0).unwrap()
    );

    let mut compare = fixtures::started_zero_arg(fixtures::literal_string_compare_artifact());
    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::I32(0))),
        compare.run_slice(16, 0).unwrap()
    );

    let mut hash = fixtures::started_zero_arg(fixtures::literal_string_hash_artifact());
    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::I32(2_337))),
        hash.run_slice(16, 0).unwrap()
    );
}

#[test]
fn string_hash_resumes_and_charges_one_unit_per_eight_code_units() {
    let code_units = vec![u16::from(b'a'); 25];
    let expected = code_units.iter().fold(0_i32, |hash, code_unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(*code_unit))
    });
    let mut machine =
        fixtures::started_zero_arg(fixtures::long_literal_string_hash_artifact(&code_units));

    assert_eq!(Outcome::SliceExhausted, machine.run_slice(3, 0).unwrap());
    assert_eq!(0, machine.consumed_dynamic_cost());
    assert_eq!(Outcome::SliceExhausted, machine.run_slice(3, 0).unwrap());
    assert_eq!(3, machine.consumed_dynamic_cost());
    assert_eq!(
        Outcome::Halted(Some(RuntimeValue::I32(expected))),
        machine.run_slice(3, 0).unwrap()
    );
    assert_eq!(4, machine.consumed_dynamic_cost());
}

#[test]
fn literal_backings_deduplicate_raw_utf16_across_modules() {
    let bytes = [0x00, 0xd8, 0x61, 0x00, 0x00, 0xd8];
    let ranges = [
        ByteRange { start: 0, end: 2 },
        ByteRange { start: 2, end: 4 },
        ByteRange { start: 4, end: 6 },
        ByteRange { start: 0, end: 0 },
    ];

    let (literals, ids) = deduplicate_literal_ranges(&bytes, &ranges).unwrap();
    assert_eq!(3, literals.len());
    assert_eq!(ids[0], ids[2]);
    assert_ne!(ids[0], ids[1]);
    assert_eq!(0, literals[ids[3]].code_units);
    assert_eq!(&[0x00, 0xd8], literals[ids[0]].bytes.slice(&bytes));
}

#[test]
fn string_concat_creates_a_fresh_compact_dynamic_string() {
    let mut machine = fixtures::started_zero_arg(fixtures::literal_string_concat_artifact());
    assert_eq!(Outcome::SliceExhausted, machine.run_slice(3, 0).unwrap());
    let Outcome::Halted(Some(RuntimeValue::Reference(reference))) =
        machine.run_slice(16, 0).unwrap()
    else {
        panic!("concat did not return a reference");
    };

    assert_eq!(ReferenceDomain::Managed, reference.domain());
    assert_eq!(4, machine.string_length(reference));
    assert_eq!(
        vec![0x48, 0x69, 0x48, 0x69],
        (0..4)
            .map(|index| machine.string_get(reference, index))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        Some(super::layout::StringEncoding::Latin1),
        machine.string_encoding(reference)
    );
}

#[test]
fn string_concat_selects_utf16_for_bmp_and_surrogate_code_units() {
    for code_units in [&[0x100][..], &[0xd800][..]] {
        let mut machine =
            fixtures::started_zero_arg(fixtures::literal_string_concat_units_artifact(code_units));
        assert_eq!(Outcome::SliceExhausted, machine.run_slice(3, 0).unwrap());
        let Outcome::Halted(Some(RuntimeValue::Reference(reference))) =
            machine.run_slice(16, 0).unwrap()
        else {
            panic!("concat did not return a reference");
        };
        assert_eq!(
            Some(super::layout::StringEncoding::Utf16),
            machine.string_encoding(reference)
        );
        assert_eq!(code_units[0], machine.string_get(reference, 0));
        assert_eq!(code_units[0], machine.string_get(reference, 1));
    }
}

#[test]
fn string_concat_resumes_without_publishing_a_prefix() {
    let code_units = vec![u16::from(b'x'); 25];
    let mut machine =
        fixtures::started_zero_arg(fixtures::literal_string_concat_units_artifact(&code_units));

    assert_eq!(Outcome::SliceExhausted, machine.run_slice(3, 0).unwrap());
    for expected_cost in [1, 4, 7, 10] {
        assert_eq!(Outcome::SliceExhausted, machine.run_slice(3, 0).unwrap());
        assert_eq!(expected_cost, machine.consumed_dynamic_cost());
    }
    let Outcome::Halted(Some(RuntimeValue::Reference(reference))) =
        machine.run_slice(3, 0).unwrap()
    else {
        panic!("concat did not publish its completed result");
    };
    assert_eq!(11, machine.consumed_dynamic_cost());
    assert_eq!(50, machine.string_length(reference));
}

#[test]
fn string_substring_preserves_full_identity_and_freshens_proper_ranges() {
    let mut full = fixtures::started_zero_arg(fixtures::literal_string_substring_artifact(0, 2));
    assert_eq!(Outcome::SliceExhausted, full.run_slice(4, 0).unwrap());
    let Outcome::Halted(Some(RuntimeValue::Reference(full_reference))) =
        full.run_slice(8, 0).unwrap()
    else {
        panic!("full substring did not return a reference");
    };
    assert_eq!(ReferenceDomain::Literal, full_reference.domain());

    let mut proper = fixtures::started_zero_arg(fixtures::literal_string_substring_artifact(0, 1));
    assert_eq!(Outcome::SliceExhausted, proper.run_slice(4, 0).unwrap());
    let Outcome::Halted(Some(RuntimeValue::Reference(proper_reference))) =
        proper.run_slice(8, 0).unwrap()
    else {
        panic!("proper substring did not return a reference");
    };
    assert_eq!(ReferenceDomain::Managed, proper_reference.domain());
    assert_eq!(1, proper.string_length(proper_reference));
    assert_eq!(0x48, proper.string_get(proper_reference, 0));
}

#[test]
fn string_substring_checks_order_and_utf16_bounds_before_allocation() {
    for (start, end) in [(-1, 1), (1, 0), (0, 3)] {
        let mut machine =
            fixtures::started_zero_arg(fixtures::literal_string_substring_artifact(start, end));
        assert_eq!(Outcome::SliceExhausted, machine.run_slice(4, 0).unwrap());
        assert_eq!(
            Outcome::Crashed(GuestTrap::IndexOutOfBounds),
            machine.run_slice(8, 0).unwrap()
        );
    }
}

#[test]
fn empty_substring_returns_the_canonical_empty_literal() {
    let mut machine = fixtures::started_zero_arg(fixtures::literal_string_substring_artifact(1, 1));
    assert_eq!(Outcome::SliceExhausted, machine.run_slice(4, 0).unwrap());
    let Outcome::Halted(Some(RuntimeValue::Reference(reference))) =
        machine.run_slice(8, 0).unwrap()
    else {
        panic!("empty substring did not return a reference");
    };
    assert_eq!(ReferenceDomain::Literal, reference.domain());
    assert_eq!(0, machine.string_length(reference));
}

#[test]
fn utf8_bridges_preserve_pairs_and_replace_or_reject_isolated_surrogates() {
    let mut utf8 = [0_u8; 16];
    let written = super::text::utf16_to_utf8(&[0xd83d, 0xde00], &mut utf8, true).unwrap();
    assert_eq!("😀".as_bytes(), &utf8[..written]);
    assert_eq!(
        super::text::Utf8Error::Invalid,
        super::text::utf16_to_utf8(&[0xd800], &mut utf8, true).unwrap_err()
    );
    let written = super::text::utf16_to_utf8(&[0xd800], &mut utf8, false).unwrap();
    assert_eq!("�".as_bytes(), &utf8[..written]);

    let mut utf16 = [0_u16; 8];
    let written = super::text::utf8_to_utf16("😀".as_bytes(), &mut utf16, true).unwrap();
    assert_eq!(&[0xd83d, 0xde00], &utf16[..written]);
    assert_eq!(
        super::text::Utf8Error::Invalid,
        super::text::utf8_to_utf16(&[0xf0, 0x28, 0x8c, 0x28], &mut utf16, true).unwrap_err()
    );
    let written = super::text::utf8_to_utf16(&[0xf0, 0x28, 0x8c, 0x28], &mut utf16, false).unwrap();
    assert_eq!(&[0xfffd, 0x28, 0xfffd, 0x28], &utf16[..written]);
}

#[test]
fn utf8_bridges_fail_atomically_when_output_is_too_small() {
    let mut utf8 = [0xaa; 3];
    assert_eq!(
        super::text::Utf8Error::InsufficientCapacity,
        super::text::utf16_to_utf8(&[0xd83d, 0xde00], &mut utf8, true).unwrap_err()
    );
    assert_eq!([0xaa; 3], utf8);

    let mut utf16 = [0xbbbb; 1];
    assert_eq!(
        super::text::Utf8Error::InsufficientCapacity,
        super::text::utf8_to_utf16("😀".as_bytes(), &mut utf16, true).unwrap_err()
    );
    assert_eq!([0xbbbb; 1], utf16);
}

#[test]
fn utf8_cursors_resume_at_eight_utf16_units_without_publishing_prefixes() {
    use super::text::{ConversionStatus, Utf16ToUtf8Cursor, Utf8ToUtf16Cursor};

    let input = vec![u16::from(b'a'); 17];
    let mut encoded = [0_u8; 64];
    let mut encoder = Utf16ToUtf8Cursor::new(true);
    assert_eq!(
        ConversionStatus::Pending,
        encoder.step(&input, &mut encoded, 1).status
    );
    assert_eq!(
        ConversionStatus::Pending,
        encoder.step(&input, &mut encoded, 1).status
    );
    assert_eq!(
        ConversionStatus::Complete(17),
        encoder.step(&input, &mut encoded, 1).status
    );
    assert_eq!(&[b'a'; 17], &encoded[..17]);

    let encoded_faces = "😀😀😀😀😀".as_bytes();
    let mut decoded = [0_u16; 16];
    let mut decoder = Utf8ToUtf16Cursor::new(true);
    assert_eq!(
        ConversionStatus::Pending,
        decoder.step(encoded_faces, &mut decoded, 1).status
    );
    assert_eq!(
        ConversionStatus::Complete(10),
        decoder.step(encoded_faces, &mut decoded, 1).status
    );
    assert_eq!(
        &[0xd83d, 0xde00, 0xd83d, 0xde00, 0xd83d, 0xde00, 0xd83d, 0xde00, 0xd83d, 0xde00,],
        &decoded[..10]
    );
}

#[test]
fn strict_utf8_cursor_failure_never_reports_a_publishable_length() {
    use super::text::{ConversionStatus, Utf16ToUtf8Cursor};

    let mut output = [0_u8; 32];
    let mut cursor = Utf16ToUtf8Cursor::new(true);
    let step = cursor.step(&[u16::from(b'a'), 0xd800], &mut output, 1);
    assert_eq!(1, step.units);
    assert_eq!(
        ConversionStatus::Failed(super::text::Utf8Error::Invalid),
        step.status
    );
}

#[test]
fn dynamic_string_compare_orders_utf16_units_as_unsigned_values() {
    let mut machine =
        fixtures::started_zero_arg(fixtures::unsigned_dynamic_string_compare_artifact());
    let mut outcome = machine.run_slice(4, 0).unwrap();
    while outcome == Outcome::SliceExhausted {
        outcome = machine.run_slice(8, 0).unwrap();
    }
    assert_eq!(Outcome::Halted(Some(RuntimeValue::I32(-1))), outcome);
}

#[test]
fn literal_load_allocates_nothing() {
    let mut machine = fixtures::started_zero_arg(fixtures::literal_string_artifact());
    super::tests::allocation_counter::reset_and_enable();
    let outcome = machine.run_slice(16, 0).unwrap();
    let allocations = super::tests::allocation_counter::disable_and_read();

    assert!(matches!(
        outcome,
        Outcome::Halted(Some(RuntimeValue::Reference(_)))
    ));
    assert_eq!(0, allocations);
}

#[test]
fn dynamic_string_execution_allocates_nothing_natively() {
    let code_units = vec![u16::from(b'x'); 25];
    let mut machine =
        fixtures::started_zero_arg(fixtures::literal_string_concat_units_artifact(&code_units));
    super::tests::allocation_counter::reset_and_enable();
    let mut outcome = machine.run_slice(3, 0).unwrap();
    while outcome == Outcome::SliceExhausted {
        outcome = machine.run_slice(3, 0).unwrap();
    }
    let allocations = super::tests::allocation_counter::disable_and_read();

    assert!(matches!(
        outcome,
        Outcome::Halted(Some(RuntimeValue::Reference(_)))
    ));
    assert_eq!(0, allocations);
}

#[test]
fn equal_dynamic_strings_are_fresh_but_content_equal() {
    for content_equality in [false, true] {
        let mut machine =
            fixtures::started_zero_arg(fixtures::repeated_concat_artifact(content_equality));
        let mut outcome = machine.run_slice(3, 0).unwrap();
        while outcome == Outcome::SliceExhausted {
            outcome = machine.run_slice(8, 0).unwrap();
        }
        assert_eq!(Outcome::Halted(Some(RuntimeValue::Bool(true))), outcome);
    }
}
