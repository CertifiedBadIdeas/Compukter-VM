/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

#[path = "../../../tests/support/rv32_elf.rs"]
#[allow(dead_code)]
mod rv32_elf_support;
#[path = "support/virtio.rs"]
mod virtio_support;

use compukter_vm::rv32_machine::{
    Rv32ExecutionBackendConfig, Rv32MachineBuilder, Rv32MachineConfig, Rv32MachineOutcome,
    DEBUG_BASE, PLIC_BASE,
};
use compukter_vm::rv32im::encoding::{
    addi, andi, bne, csrrw, jal, lbu, lhu, lw, materialize, mret, sb, sw, wfi,
};
use compukter_vm_devices::virtio::VirtioMmioDevice;
use rv32_elf_support::{Elf32Builder, LoadSegment};
use virtio_support::{
    queue_image, EchoDevice, AVAILABLE_ADDRESS, DESCRIPTOR_ADDRESS, OUTPUT_ADDRESS, QUEUE_SIZE,
    TEST_DEVICE_ID, USED_ADDRESS, VIRTIO_BASE,
};

const CSR_MSTATUS: u16 = 0x300;
const CSR_MIE: u16 = 0x304;
const CSR_MTVEC: u16 = 0x305;
const MSTATUS_MIE: u32 = 1 << 3;
const MIE_MEIE: u32 = 1 << 11;

#[test]
fn rv32_guest_completes_a_virtio_request_through_plic() {
    let elf = virtio_interrupt_elf();
    let mut builder = Rv32MachineBuilder::from_elf(
        &elf,
        Rv32MachineConfig {
            ram_size: 0x1_0000,
            debug_limit: 2,
            execution: Rv32ExecutionBackendConfig::default(),
        },
    )
    .unwrap();
    let (transport, source) =
        builder.add_mmio_device_with_irq(VIRTIO_BASE, VirtioMmioDevice::new(EchoDevice).unwrap());
    assert_eq!(source.get(), 1);
    let mut machine = builder.build().unwrap();

    let outcome = machine.run(2_000).unwrap();
    assert!(
        matches!(outcome, Rv32MachineOutcome::WaitingForInterrupt { .. }),
        "outcome={outcome:?}, debug={:?}",
        machine.debug_bytes()
    );
    assert_eq!(machine.debug_bytes(), &[b'V', 1]);
    assert!(!machine.device(transport).unwrap().interrupt_pending());
}

fn virtio_interrupt_elf() -> Vec<u8> {
    const HANDLER: u32 = 0x2000;
    let mut main = Vec::new();
    let mut failure_branches = Vec::new();

    emit_materialize(&mut main, 1, VIRTIO_BASE);
    main.push(lw(2, 1, 0x000));
    emit_materialize(&mut main, 3, 0x7472_6976);
    emit_checked_bne(&mut main, &mut failure_branches, 2, 3);
    main.push(lw(2, 1, 0x004));
    main.push(addi(3, 0, 2));
    emit_checked_bne(&mut main, &mut failure_branches, 2, 3);
    main.push(lw(2, 1, 0x008));
    emit_materialize(&mut main, 3, TEST_DEVICE_ID);
    emit_checked_bne(&mut main, &mut failure_branches, 2, 3);

    emit_materialize(&mut main, 4, PLIC_BASE + 4);
    main.push(addi(5, 0, 3));
    main.push(sw(4, 5, 0));
    emit_materialize(&mut main, 4, PLIC_BASE + 0x2000);
    main.push(addi(5, 0, 1 << 1));
    main.push(sw(4, 5, 0));
    emit_materialize(&mut main, 4, PLIC_BASE + 0x20_0000);
    main.push(sw(4, 0, 0));
    emit_materialize(&mut main, 3, HANDLER);
    main.push(csrrw(0, CSR_MTVEC, 3));
    emit_materialize(&mut main, 3, MIE_MEIE);
    main.push(csrrw(0, CSR_MIE, 3));
    main.push(addi(3, 0, MSTATUS_MIE as i32));
    main.push(csrrw(0, CSR_MSTATUS, 3));

    main.push(addi(2, 0, 1));
    main.push(sw(1, 2, 0x014));
    main.push(lw(3, 1, 0x010));
    main.push(andi(3, 3, 1));
    emit_checked_bne(&mut main, &mut failure_branches, 2, 3);
    main.push(sw(1, 2, 0x024));
    main.push(sw(1, 2, 0x020));
    main.push(addi(2, 0, 1 | 2 | 8));
    main.push(sw(1, 2, 0x070));
    main.push(lw(3, 1, 0x070));
    emit_checked_bne(&mut main, &mut failure_branches, 2, 3);

    main.push(addi(2, 0, i32::from(QUEUE_SIZE)));
    main.push(sw(1, 2, 0x038));
    emit_materialize(&mut main, 2, DESCRIPTOR_ADDRESS);
    main.push(sw(1, 2, 0x080));
    emit_materialize(&mut main, 2, AVAILABLE_ADDRESS);
    main.push(sw(1, 2, 0x090));
    emit_materialize(&mut main, 2, USED_ADDRESS);
    main.push(sw(1, 2, 0x0a0));
    main.push(addi(2, 0, 1));
    main.push(sw(1, 2, 0x044));
    main.push(addi(2, 0, 1 | 2 | 4 | 8));
    main.push(sw(1, 2, 0x070));
    main.push(sw(1, 0, 0x050));
    main.push(wfi());
    main.push(jal(0, -4));

    let failure = main.len();
    emit_materialize(&mut main, 4, DEBUG_BASE);
    main.push(addi(5, 0, i32::from(b'X')));
    main.push(sb(4, 5, 0));
    main.push(wfi());
    main.push(jal(0, -4));
    for (branch, rs1, rs2) in failure_branches {
        main[branch] = bne(rs1, rs2, ((failure - branch) * 4) as i32);
    }

    let mut handler = Vec::new();
    emit_materialize(&mut handler, 8, PLIC_BASE + 0x20_0004);
    handler.push(lw(9, 8, 0));
    emit_materialize(&mut handler, 6, VIRTIO_BASE);
    handler.push(lw(11, 6, 0x060));
    handler.push(sw(6, 11, 0x064));
    emit_materialize(&mut handler, 10, DEBUG_BASE);
    emit_materialize(&mut handler, 7, OUTPUT_ADDRESS);
    handler.push(lbu(12, 7, 0));
    handler.push(sb(10, 12, 0));
    emit_materialize(&mut handler, 7, USED_ADDRESS);
    handler.push(lhu(12, 7, 2));
    handler.push(sb(10, 12, 0));
    handler.push(sw(8, 9, 0));
    handler.push(mret());

    Elf32Builder::new(0x1000)
        .load(LoadSegment::rx(0x1000, words(&main)))
        .load(LoadSegment::rx(HANDLER, words(&handler)))
        .load(LoadSegment::rw_with_mem_size(
            DESCRIPTOR_ADDRESS,
            queue_image(b"V"),
            0x1000,
        ))
        .finish()
}

fn emit_materialize(words: &mut Vec<u32>, register: u8, value: u32) {
    let [high, low] = materialize(register, value);
    words.extend([high, low]);
}

fn emit_checked_bne(words: &mut Vec<u32>, branches: &mut Vec<(usize, u8, u8)>, rs1: u8, rs2: u8) {
    branches.push((words.len(), rs1, rs2));
    words.push(0);
}

fn words(words: &[u32]) -> Vec<u8> {
    words.iter().copied().flat_map(u32::to_le_bytes).collect()
}
