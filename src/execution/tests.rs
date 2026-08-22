use super::{
    error::RunError,
    fixtures,
    image::ExecutionImage,
    machine::Machine,
    value::{EntryArgument, RuntimeValue},
};

#[test]
fn start_validates_all_arguments_before_mutation() {
    let image =
        ExecutionImage::admit(fixtures::typed_entry_artifact(), fixtures::profile()).unwrap();
    let mut machine = Machine::new(image).unwrap();
    let before = machine.test_snapshot();
    assert!(machine
        .start(&[EntryArgument(RuntimeValue::I64(1))])
        .is_err());
    assert_eq!(before, machine.test_snapshot());
    machine
        .start(&[EntryArgument(RuntimeValue::I32(1))])
        .unwrap();
    assert_eq!(1, machine.frame_depth());
    assert_eq!(RuntimeValue::I32(1), machine.test_register(0).unwrap());
}

#[test]
fn references_require_matching_image_type_liveness_and_generation() {
    let (image, valid, foreign, dead, stale) = fixtures::reference_entry_case();
    assert!(Machine::new(image.clone())
        .unwrap()
        .start(&[EntryArgument(valid)])
        .is_ok());
    assert!(Machine::new(image.clone())
        .unwrap()
        .start(&[EntryArgument(foreign)])
        .is_err());
    assert!(Machine::new(image.clone())
        .unwrap()
        .start(&[EntryArgument(dead)])
        .is_err());
    assert!(Machine::new(image)
        .unwrap()
        .start(&[EntryArgument(stale)])
        .is_err());
}

#[test]
fn failed_start_is_retryable_but_successful_start_is_one_shot() {
    let image =
        ExecutionImage::admit(fixtures::typed_entry_artifact(), fixtures::profile()).unwrap();
    let mut machine = Machine::new(image).unwrap();
    assert!(machine.start(&[]).is_err());
    machine
        .start(&[EntryArgument(RuntimeValue::I32(7))])
        .unwrap();
    assert_eq!(
        Err(RunError::AlreadyStarted),
        machine.start(&[EntryArgument(RuntimeValue::I32(8))])
    );
}
