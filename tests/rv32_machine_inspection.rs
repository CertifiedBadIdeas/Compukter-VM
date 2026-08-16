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

#[path = "support/rv32_elf.rs"]
#[allow(dead_code)]
mod rv32_elf_support;

use compukter_vm::bus::{MmioAccessWidth, MmioContext, MmioDevice};
use compukter_vm::memory::MemoryFault;
use compukter_vm::rv32_machine::{
    Rv32ExecutionBackendConfig, Rv32MachineBuilder, Rv32MachineConfig,
};
use compukter_vm::rv32im::encoding::{jal, wfi};
use rv32_elf_support::machine_program_elf;

struct TestIrqDevice {
    level: bool,
}

impl MmioDevice for TestIrqDevice {
    fn size(&self) -> u32 {
        4
    }

    fn interrupt_level(&self) -> bool {
        self.level
    }

    fn read(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
    ) -> Result<u64, MemoryFault> {
        if offset == 0 && width == MmioAccessWidth::Word {
            Ok(u64::from(self.level))
        } else {
            Err(MemoryFault::new("invalid test IRQ read".to_string()))
        }
    }

    fn write(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
        value: u64,
    ) -> Result<(), MemoryFault> {
        if offset == 0 && width == MmioAccessWidth::Word {
            self.level = value != 0;
            Ok(())
        } else {
            Err(MemoryFault::new("invalid test IRQ write".to_string()))
        }
    }
}

#[test]
fn machine_inspection_reports_wfi_and_routed_irq_without_mutation() {
    let elf = machine_program_elf(&[wfi(), jal(0, 0)]);
    let mut builder = Rv32MachineBuilder::from_elf(
        &elf,
        Rv32MachineConfig {
            ram_size: 16 * 1024,
            debug_limit: 0,
            execution: Rv32ExecutionBackendConfig::Cached { sets: 8 },
        },
    )
    .unwrap();
    let (irq, source) =
        builder.add_mmio_device_with_irq(0x1000_1000, TestIrqDevice { level: false });
    let mut machine = builder.build().unwrap();

    machine.run(1).unwrap();
    let waiting = machine.inspection_snapshot();
    assert!(waiting.hart.waiting_for_interrupt);
    assert_eq!(waiting.hart.pc, 0x1004);
    assert_eq!(waiting.irq_route_count, 1);
    assert_eq!(waiting.irq_routes[0].source, source.get());
    assert!(!waiting.irq_routes[0].level);

    machine.device_mut(irq).unwrap().level = true;
    machine.run(1).unwrap();
    let pending = machine.inspection_snapshot();
    assert!(pending.hart.waiting_for_interrupt);
    assert_eq!(pending.plic.source_count, 1);
    assert!(pending.plic.sources[0].level);
    assert!(pending.plic.sources[0].pending);
    assert!(pending.irq_routes[0].level);
    assert_eq!(pending, machine.inspection_snapshot());
}
