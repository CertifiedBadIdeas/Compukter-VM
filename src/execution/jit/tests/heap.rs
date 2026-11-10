/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

use super::*;

fn heap_bytes(heap: &crate::execution::heap::Heap) -> Vec<u8> {
    use crate::execution::checkpoint::Checkpoint;
    let mut writer = Writer::new(1024 * 1024);
    heap.write(&mut writer).unwrap();
    writer.finish()
}

#[test]
fn native_mixed_heap_regions_preserve_checks_fault_order_and_prior_effects() {
    use crate::execution::{
        checkpoint::Checkpoint,
        heap::{AllocationRequest, Heap},
        layout::RuntimeTypeLayout,
        machine::prepared_execution::execute_heap,
        value::Ref32,
    };
    let mut artifacts = fixtures::primitive_field_roundtrip_cases();
    artifacts.extend(
        fixtures::primitive_array_roundtrip_cases()
            .into_iter()
            .map(|x| x.0),
    );
    artifacts.extend([
        fixtures::reference_field_roundtrip_artifact(),
        fixtures::reference_array_roundtrip_artifact(),
    ]);
    for storage in 1..=4 {
        artifacts.push(fixtures::packed_array_roundtrip_artifact(storage, -1, -1));
    }
    for artifact in artifacts {
        // Distinct compact packing widths plus the legacy layout, sharing code.
        for quota in [512, 1024, 1 << 31] {
            let mut profile = fixtures::profile();
            profile.heap_bytes = quota;
            let image = ExecutionImage::admit(artifact.clone(), profile).unwrap();
            let mut heap = Heap::new(
                &image
                    .storage_plan()
                    .with_heap_arena_bytes(u64::from(quota.min(4096))),
            )
            .unwrap();
            let field = image.code().fields.first();
            let (type_id, block_bytes, kind, expected_reference) = if let Some(field) = field {
                let type_id = (0..image.type_count())
                    .find(|&id| {
                        let key = image.type_key(id).unwrap();
                        image.is_assignable(key, field.owner)
                            && matches!(image.type_layout(key), Some(RuntimeTypeLayout::Object(_)))
                    })
                    .unwrap();
                let Some(RuntimeTypeLayout::Object(layout)) =
                    image.type_layout(image.type_key(type_id).unwrap())
                else {
                    unreachable!()
                };
                (
                    type_id,
                    layout.block_bytes,
                    field.value_type.kind,
                    field.value_type.nominal,
                )
            } else {
                let type_id = (0..image.type_count())
                    .find(|&id| {
                        matches!(
                            image.type_layout(image.type_key(id).unwrap()),
                            Some(RuntimeTypeLayout::Array { .. })
                        )
                    })
                    .unwrap();
                let ty = image
                    .array_element_type(image.type_key(type_id).unwrap())
                    .unwrap();
                (type_id, 64, ty.kind, ty.nominal)
            };
            let reservation = heap
                .reserve(AllocationRequest {
                    block_bytes,
                    type_id: type_id as u32,
                })
                .unwrap()
                .unwrap();
            let receiver = heap.commit(reservation).unwrap();
            if field.is_none() {
                heap.write_payload(receiver, 0, &2_i32.to_le_bytes())
                    .unwrap();
            }
            let value = if let Some(expected) = expected_reference {
                let type_id = image.type_id(expected).unwrap();
                let reservation = heap
                    .reserve(AllocationRequest {
                        block_bytes: 64,
                        type_id,
                    })
                    .unwrap()
                    .unwrap();
                heap.commit(reservation).unwrap().to_bits() as u64
            } else {
                match kind {
                    5 => 1,
                    6 => 0xd800,
                    _ => 0x7ff8_1234_ffff_ffff,
                }
            };
            let (set, get) = if field.is_some() {
                (FIELD_SET, FIELD_GET)
            } else {
                (ARRAY_STORE, ARRAY_LOAD)
            };
            let code = simple_code(&[
                Operation {
                    opcode: IMMEDIATE32,
                    dst: 32,
                    lhs: 123,
                    rhs: 0,
                },
                Operation {
                    opcode: set + kind as u32,
                    dst: 8,
                    lhs: 0,
                    rhs: if field.is_some() { 0 } else { 4 },
                },
                Operation {
                    opcode: get + kind as u32,
                    dst: 16,
                    lhs: 0,
                    rhs: if field.is_some() { 0 } else { 4 },
                },
                Operation {
                    opcode: if field.is_some() {
                        COPY32
                    } else {
                        ARRAY_LENGTH
                    },
                    dst: 24,
                    lhs: if field.is_some() { 16 } else { 0 },
                    rhs: 0,
                },
                // Failure after a successful heap mutation must retain that write.
                Operation {
                    opcode: COPY_BOOL,
                    dst: 40,
                    lhs: 44,
                    rhs: 0,
                },
            ]);
            let mut variants = vec![code];
            if kind == 7 {
                // Reads of zero-initialized non-null references must trap even
                // without a preceding store. This also exercises NON_NULL flags.
                variants.push(simple_code(&[
                    Operation {
                        opcode: IMMEDIATE32,
                        dst: 32,
                        lhs: 123,
                        rhs: 0,
                    },
                    Operation {
                        opcode: get + 7 | if field.is_none() { NON_NULL } else { 0 },
                        dst: 16,
                        lhs: 0,
                        rhs: if field.is_some() { 0 } else { 4 },
                    },
                    Operation {
                        opcode: COPY32,
                        dst: 24,
                        lhs: 16,
                        rhs: 0,
                    },
                    Operation {
                        opcode: COPY_BOOL,
                        dst: 40,
                        lhs: 44,
                        rhs: 0,
                    },
                ]));
            }
            let wrong = heap
                .reserve(AllocationRequest {
                    block_bytes: 32,
                    type_id: 0,
                })
                .unwrap()
                .unwrap();
            let wrong = heap.commit(wrong).unwrap();
            let small = heap
                .reserve(AllocationRequest {
                    block_bytes: 16,
                    type_id: type_id as u32,
                })
                .unwrap()
                .unwrap();
            let small = heap.commit(small).unwrap();
            if field.is_none() {
                heap.write_payload(small, 0, &2_i32.to_le_bytes()).unwrap();
            }
            let initial = heap_bytes(&heap);
            for code in variants {
                let length = if code.operation(4).is_some() { 5 } else { 4 };
                let compiled = native(&code, length);
                assert!(compiled.has_heap);
                for scenario in 0..19 {
                    let mut input = [0; 64];
                    let mut receiver_bits = receiver.to_bits();
                    let mut stored = value;
                    let mut index = 0_i32;
                    match scenario {
                        1 => receiver_bits = 0,
                        2 => receiver_bits = Ref32::image(0).unwrap().to_bits(),
                        3 => receiver_bits = Ref32::external(1).unwrap().to_bits(),
                        4 => receiver_bits = Ref32::reserved(1).unwrap().to_bits(),
                        5 => receiver_bits = Ref32::managed(9).unwrap().to_bits(),
                        6 => receiver_bits = Ref32::managed(4008).unwrap().to_bits(),
                        7 => index = -1,
                        8 => index = 2,
                        9 => index = i32::MAX,
                        10 => stored = 2,
                        11 => stored = 65536,
                        12 => stored = 0,
                        17 => receiver_bits = wrong.to_bits(),
                        18 => receiver_bits = small.to_bits(),
                        13 => stored = Ref32::external(1).unwrap().to_bits() as u64,
                        _ => {}
                    }
                    input[..4].copy_from_slice(&receiver_bits.to_le_bytes());
                    input[4..8].copy_from_slice(&index.to_le_bytes());
                    input[8..16].copy_from_slice(&stored.to_le_bytes());
                    input[44..48]
                        .copy_from_slice(&(if scenario == 14 { 2_u32 } else { 1 }).to_le_bytes());
                    // Exercise every partial prefix and both limiting counters.
                    for cut in 0..=length as u64 {
                        for (limit, headroom) in [(cut, u64::MAX), (u64::MAX, cut)] {
                            let clone_heap = || {
                                let mut reader =
                                    Reader::new(&initial, 1024 * 1024, 1024 * 1024).unwrap();
                                let mut heap = Heap::read(&mut reader).unwrap();
                                reader.finish().unwrap();
                                if scenario == 15 {
                                    assert!(heap.free(receiver).unwrap());
                                }
                                if scenario == 16 && field.is_none() {
                                    heap.write_payload(receiver, 0, &(-1_i32).to_le_bytes())
                                        .unwrap();
                                }
                                heap
                            };
                            let mut a = clone_heap();
                            let mut b = clone_heap();
                            let mut arena_a = backed_arena(64);
                            let mut arena_b = backed_arena(64);
                            let reservation = FrameReservation {
                                base: 0,
                                byte_len: 64,
                            };
                            let mut fa = arena_a.prepared_frame(reservation).unwrap();
                            let mut fb = arena_b.prepared_frame(reservation).unwrap();
                            fa.jit_bytes().copy_from_slice(&input);
                            fb.jit_bytes().copy_from_slice(&input);
                            let expected = execute_run(
                                &code,
                                &mut fa,
                                0,
                                0,
                                length,
                                RunBudget { limit, headroom },
                                |op, frame| execute_heap(op, frame, &image, &mut a),
                            );
                            let actual = if limit.min(headroom) >= length as u64 {
                                compiled.execute(
                                    &mut fb,
                                    0,
                                    Some(HeapAccess {
                                        image: &image,
                                        heap: &mut b,
                                    }),
                                )
                            } else {
                                execute_run(
                                    &code,
                                    &mut fb,
                                    0,
                                    0,
                                    length,
                                    RunBudget { limit, headroom },
                                    |op, frame| execute_heap(op, frame, &image, &mut b),
                                )
                            };
                            assert_eq!(
                                signature(&expected),
                                signature(&actual),
                                "kind={kind} scenario={scenario} quota={quota}"
                            );
                            assert_eq!(fa.jit_bytes(), fb.jit_bytes());
                            assert_eq!(heap_bytes(&a), heap_bytes(&b));
                        }
                    }
                }
            }
        }
    }
}
