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

use compukter_vm::rv32_machine::{
    Rv32ExecutionBackendConfig, Rv32MachineBuilder, Rv32MachineConfig, Rv32MachineOutcome,
    DEBUG_BASE, DEFAULT_DBT_CODE_ALIGNMENT, DEFAULT_DBT_REGISTER_PROFILE, PLIC_BASE,
};
use compukter_vm::rv32im::encoding::{addi, csrrw, jal, lbu, lw, materialize, mret, sb, sw, wfi};
use compukter_vm_devices::Uart16550;
use rv32_elf_support::{Elf32Builder, LoadSegment};

const UART_BASE: u32 = 0x1000_1000;
const CSR_MSTATUS: u16 = 0x300;
const CSR_MIE: u16 = 0x304;
const CSR_MTVEC: u16 = 0x305;
const MSTATUS_MIE: u32 = 1 << 3;
const MIE_MEIE: u32 = 1 << 11;

#[test]
fn every_backend_wakes_for_uart_rx_and_transmits_a_response() {
    let elf = uart_interrupt_elf();

    for execution in configs() {
        let mut builder = Rv32MachineBuilder::from_elf(
            &elf,
            Rv32MachineConfig {
                ram_size: 0x10_000,
                debug_limit: 2,
                execution,
            },
        )
        .unwrap();
        let (uart, source) = builder.add_mmio_device_with_irq(UART_BASE, Uart16550::new());
        assert_eq!(source.get(), 1);
        let mut machine = builder.build().unwrap();
        machine.device_mut(uart).unwrap().connect();

        assert!(matches!(
            machine.run(256).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt { .. }
        ));
        assert_eq!(
            machine
                .device_mut(uart)
                .unwrap()
                .inject_rx(&[b'A'])
                .transferred,
            1
        );
        assert!(matches!(
            machine.run(256).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt { .. }
        ));

        let mut output = [0_u8; 1];
        assert_eq!(machine.device_mut(uart).unwrap().drain_tx(&mut output), 1);
        assert_eq!(output, [b'B']);
        assert_eq!(machine.debug_bytes(), &[1, 4], "backend {execution:?}");
    }
}

fn uart_interrupt_elf() -> Vec<u8> {
    const HANDLER: u32 = 0x2000;
    let [priority_hi, priority_lo] = materialize(1, PLIC_BASE + 4);
    let [enable_hi, enable_lo] = materialize(1, PLIC_BASE + 0x2000);
    let [threshold_hi, threshold_lo] = materialize(1, PLIC_BASE + 0x20_0000);
    let [uart_hi, uart_lo] = materialize(6, UART_BASE);
    let [handler_hi, handler_lo] = materialize(3, HANDLER);
    let [meie_hi, meie_lo] = materialize(4, MIE_MEIE);
    let main = [
        priority_hi,
        priority_lo,
        addi(2, 0, 3),
        sw(1, 2, 0),
        enable_hi,
        enable_lo,
        addi(2, 0, 1 << 1),
        sw(1, 2, 0),
        threshold_hi,
        threshold_lo,
        sw(1, 0, 0),
        uart_hi,
        uart_lo,
        addi(2, 0, 1),
        sb(6, 2, 1),
        handler_hi,
        handler_lo,
        csrrw(0, CSR_MTVEC, 3),
        meie_hi,
        meie_lo,
        csrrw(0, CSR_MIE, 4),
        addi(5, 0, MSTATUS_MIE as i32),
        csrrw(0, CSR_MSTATUS, 5),
        wfi(),
        jal(0, -4),
    ];

    let [claim_hi, claim_lo] = materialize(8, PLIC_BASE + 0x20_0004);
    let [debug_hi, debug_lo] = materialize(10, DEBUG_BASE);
    let handler = [
        claim_hi,
        claim_lo,
        lw(9, 8, 0),
        debug_hi,
        debug_lo,
        sb(10, 9, 0),
        uart_hi,
        uart_lo,
        lbu(11, 6, 2),
        sb(10, 11, 0),
        lbu(12, 6, 0),
        addi(12, 12, 1),
        sb(6, 12, 0),
        sw(8, 9, 0),
        mret(),
    ];
    let words = |words: &[u32]| {
        words
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    };
    Elf32Builder::new(0x1000)
        .load(LoadSegment::rx(0x1000, words(&main)))
        .load(LoadSegment::rx(HANDLER, words(&handler)))
        .load(LoadSegment::rw_with_mem_size(0x3000, [], 0x1000))
        .finish()
}

fn configs() -> [Rv32ExecutionBackendConfig; 5] {
    [
        Rv32ExecutionBackendConfig::Cached { sets: 64 },
        Rv32ExecutionBackendConfig::Predecoded,
        Rv32ExecutionBackendConfig::BlockCached {
            sets: 32,
            max_instructions: 8,
        },
        Rv32ExecutionBackendConfig::DirectDbt {
            max_instructions: 8,
            scratch_bytes: 4096,
        },
        Rv32ExecutionBackendConfig::CachedDbt {
            sets: 32,
            max_instructions: 8,
            scratch_bytes: 4096,
            cache_bytes: 4096,
            code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
            register_profile: DEFAULT_DBT_REGISTER_PROFILE,
        },
    ]
}
