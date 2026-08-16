/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

#[path = "support/rv32_elf.rs"]
mod rv32_elf_support;

use compukter_vm::rv32_machine::{
    Rv32DbtCodeAlignment, Rv32DbtRegisterProfile, Rv32ExecutionBackendConfig, Rv32Machine,
    Rv32MachineBuilder, Rv32MachineConfig, Rv32MachineOutcome, CONTROL_BASE, DEBUG_BASE,
    DEFAULT_DBT_CACHE_SETS, DEFAULT_DBT_CODE_ALIGNMENT, DEFAULT_DBT_CODE_BYTES,
    DEFAULT_DBT_MAX_INSTRUCTIONS, DEFAULT_DBT_REGISTER_PROFILE, DEFAULT_DBT_SCRATCH_BYTES,
    PLIC_BASE, STATUS_BOOTING, STATUS_HALTED, STATUS_PANIC, TIMER_BASE,
};
#[cfg(feature = "dbt-execution-profile")]
use compukter_vm::rv32_machine::{Rv32DbtExecutionProfile, Rv32DbtProfileEdgeKind};
#[cfg(feature = "dbt-execution-profile")]
use compukter_vm::rv32im::encoding::jalr;
use compukter_vm::rv32im::encoding::{
    add, addi, amoswap_w, andn, bne, clz, cpop, csrrs, csrrw, ctz, ebreak, ecall, fence_i, jal,
    lr_w, lui, lw, materialize, max, maxu, min, minu, mret, orc_b, orn, rev8, rol, ror, rori, sb,
    sc_w, sext_b, sext_h, sh, slli, sw, wfi, xnor, zext_h,
};
use compukter_vm::{
    bus::{MmioAccessWidth, MmioContext, MmioDevice},
    memory::MemoryFault,
};
use rv32_elf_support::{halting_machine_elf, machine_program_elf, Elf32Builder, LoadSegment};

const CSR_MTVEC: u16 = 0x305;
const CSR_MSTATUS: u16 = 0x300;
const CSR_MIE: u16 = 0x304;
const CSR_MEPC: u16 = 0x341;
const CSR_MCAUSE: u16 = 0x342;
const CSR_MTVAL: u16 = 0x343;
const MSTATUS_MIE: i32 = 1 << 3;
const MIE_MTIE: i32 = 1 << 7;
const MIE_MEIE: i32 = 1 << 11;

struct TestRegister(i32);

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

impl MmioDevice for TestRegister {
    fn size(&self) -> u32 {
        4
    }

    fn read(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
    ) -> Result<u64, MemoryFault> {
        if offset == 0 && width == MmioAccessWidth::Word {
            Ok(u64::from(self.0 as u32))
        } else {
            Err(MemoryFault::new("invalid test register read".to_string()))
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
            self.0 = value as u32 as i32;
            Ok(())
        } else {
            Err(MemoryFault::new("invalid test register write".to_string()))
        }
    }
}

#[test]
fn builder_installs_a_device_with_a_typed_host_handle() {
    let elf = halting_machine_elf(b'B');
    let mut builder = Rv32MachineBuilder::from_elf(
        &elf,
        config(Rv32ExecutionBackendConfig::Cached { sets: 8 }, 16),
    )
    .unwrap();
    let register = builder.add_mmio_device(0x1000_1000, TestRegister(7));

    let mut machine = builder.build().unwrap();

    assert_eq!(machine.device(register).unwrap().0, 7);
    machine.device_mut(register).unwrap().0 = 11;
    assert_eq!(machine.device(register).unwrap().0, 11);
}

#[test]
fn builder_assigns_dense_irq_sources_without_changing_plain_devices() {
    let elf = halting_machine_elf(b'I');
    let mut builder = Rv32MachineBuilder::from_elf(
        &elf,
        config(Rv32ExecutionBackendConfig::Cached { sets: 8 }, 0),
    )
    .unwrap();
    let plain = builder.add_mmio_device(0x1000_1000, TestRegister(0));
    let (first_device, first_source) =
        builder.add_mmio_device_with_irq(0x1000_1100, TestIrqDevice { level: false });
    let (_, second_source) =
        builder.add_mmio_device_with_irq(0x1000_1200, TestIrqDevice { level: false });

    assert_eq!(first_source.get(), 1);
    assert_eq!(second_source.get(), 2);
    let machine = builder.build().unwrap();
    assert!(machine.device(plain).is_some());
    assert!(machine.device(first_device).is_some());
}

#[test]
fn builder_rejects_more_than_1023_irq_sources() {
    let elf = halting_machine_elf(b'L');
    let mut builder = Rv32MachineBuilder::from_elf(
        &elf,
        config(Rv32ExecutionBackendConfig::Cached { sets: 8 }, 0),
    )
    .unwrap();
    for index in 0..1024_u32 {
        let _ = builder
            .add_mmio_device_with_irq(0x1100_0000 + index * 4, TestIrqDevice { level: false });
    }

    let error = match builder.build() {
        Ok(_) => panic!("source 1024 must be rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("at most 1023"));
}

#[test]
fn builder_device_is_visible_to_guest_mmio() {
    const REGISTER_BASE: u32 = 0x1000_1000;
    let [base_hi, base_lo] = materialize(1, REGISTER_BASE);
    let elf = machine_program_elf(&[
        base_hi,
        base_lo,
        addi(2, 0, 42),
        sw(1, 2, 0),
        lw(3, 1, 0),
        jal(0, 0),
    ]);
    let mut builder = Rv32MachineBuilder::from_elf(
        &elf,
        config(Rv32ExecutionBackendConfig::Cached { sets: 8 }, 0),
    )
    .unwrap();
    let register = builder.add_mmio_device(REGISTER_BASE, TestRegister(0));
    let mut machine = builder.build().unwrap();

    assert_eq!(
        machine.run(5).unwrap(),
        Rv32MachineOutcome::BudgetExhausted {
            retired_delta: 5,
            retired_total: 5,
        }
    );
    assert_eq!(machine.device(register).unwrap().0, 42);
}

#[test]
fn builder_rejects_invalid_complete_topologies() {
    let elf = halting_machine_elf(b'T');

    for base in [0x1000, CONTROL_BASE, DEBUG_BASE, TIMER_BASE, u32::MAX - 1] {
        let mut builder = Rv32MachineBuilder::from_elf(
            &elf,
            config(Rv32ExecutionBackendConfig::Cached { sets: 8 }, 0),
        )
        .unwrap();
        let _handle = builder.add_mmio_device(base, TestRegister(0));
        assert!(builder.build().is_err(), "base {base:#010x}");
    }

    let mut builder = Rv32MachineBuilder::from_elf(
        &elf,
        config(Rv32ExecutionBackendConfig::Cached { sets: 8 }, 0),
    )
    .unwrap();
    let _first = builder.add_mmio_device(0x1000_1000, TestRegister(0));
    let _second = builder.add_mmio_device(0x1000_1002, TestRegister(0));
    assert!(builder.build().is_err());
}

#[test]
fn typed_handle_outside_a_machine_topology_returns_none() {
    let elf = halting_machine_elf(b'H');
    let mut source = Rv32MachineBuilder::from_elf(
        &elf,
        config(Rv32ExecutionBackendConfig::Cached { sets: 8 }, 0),
    )
    .unwrap();
    let _first = source.add_mmio_device(0x1000_1000, TestRegister(1));
    let second = source.add_mmio_device(0x1000_1010, TestRegister(2));

    let mut target = Rv32MachineBuilder::from_elf(
        &elf,
        config(Rv32ExecutionBackendConfig::Cached { sets: 8 }, 0),
    )
    .unwrap();
    let _only = target.add_mmio_device(0x1000_1000, TestRegister(3));
    let mut machine = target.build().unwrap();

    assert!(machine.device(second).is_none());
    assert!(machine.device_mut(second).is_none());
}

#[test]
fn host_controls_wrapping_virtual_time_explicitly() {
    let elf = halting_machine_elf(b'T');
    let mut machine = Rv32Machine::from_elf(
        &elf,
        config(Rv32ExecutionBackendConfig::Cached { sets: 8 }, 0),
    )
    .unwrap();

    assert_eq!(machine.virtual_time(), 0);
    machine.advance_time(u64::MAX);
    assert_eq!(machine.virtual_time(), u64::MAX);
    machine.advance_time(2);
    assert_eq!(machine.virtual_time(), 1);
}

#[test]
fn all_backends_stop_at_wfi_without_spending_later_budgets() {
    let elf = machine_program_elf(&[wfi(), addi(1, 0, 1), jal(0, 0)]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 0)).unwrap();
        assert_eq!(
            machine.run(8).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt {
                retired_delta: 1,
                retired_total: 1,
            }
        );
        assert_eq!(machine.pc(), 0x1004);
        assert_eq!(
            machine.run(1_000).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt {
                retired_delta: 0,
                retired_total: 1,
            }
        );
        assert_eq!(machine.pc(), 0x1004);
    }
}

#[test]
fn all_backends_deliver_machine_external_interrupt() {
    const IRQ_DEVICE_BASE: u32 = 0x1000_1000;
    const VECTOR_BASE: u32 = 0x2000;
    const HANDLER: u32 = VECTOR_BASE + 4 * 11;
    const PLIC_PRIORITY_1: u32 = PLIC_BASE + 4;
    const PLIC_ENABLE_0: u32 = PLIC_BASE + 0x2000;
    const PLIC_THRESHOLD_0: u32 = PLIC_BASE + 0x200000;
    const PLIC_CLAIM_0: u32 = PLIC_BASE + 0x200004;

    let [priority_hi, priority_lo] = materialize(1, PLIC_PRIORITY_1);
    let [enable_hi, enable_lo] = materialize(1, PLIC_ENABLE_0);
    let [threshold_hi, threshold_lo] = materialize(1, PLIC_THRESHOLD_0);
    let [handler_hi, handler_lo] = materialize(3, VECTOR_BASE | 1);
    let [meie_hi, meie_lo] = materialize(4, MIE_MEIE as u32);
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
        handler_hi,
        handler_lo,
        csrrw(0, CSR_MTVEC, 3),
        meie_hi,
        meie_lo,
        csrrw(0, CSR_MIE, 4),
        addi(5, 0, MSTATUS_MIE),
        csrrw(0, CSR_MSTATUS, 5),
        wfi(),
        lui(10, 0x10000),
        addi(11, 0, STATUS_HALTED),
        sw(10, 11, 0),
    ];
    let [claim_hi, claim_lo] = materialize(8, PLIC_CLAIM_0);
    let [device_hi, device_lo] = materialize(6, IRQ_DEVICE_BASE);
    let [debug_hi, debug_lo] = materialize(6, DEBUG_BASE);
    let handler = [
        csrrs(7, CSR_MCAUSE, 0),
        claim_hi,
        claim_lo,
        lw(9, 8, 0),
        device_hi,
        device_lo,
        sw(6, 0, 0),
        sw(8, 9, 0),
        debug_hi,
        debug_lo,
        sw(6, 7, 0),
        sw(6, 9, 0),
        csrrw(0, CSR_MIE, 0),
        mret(),
    ];
    let words = |words: &[u32]| {
        words
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    };
    let elf = Elf32Builder::new(0x1000)
        .load(LoadSegment::rx(0x1000, words(&main)))
        .load(LoadSegment::rx(HANDLER, words(&handler)))
        .load(LoadSegment::rw_with_mem_size(0x3000, [], 0x1000))
        .finish();

    for execution in configs() {
        let mut builder = Rv32MachineBuilder::from_elf(&elf, config(execution, 2)).unwrap();
        let (device, source) =
            builder.add_mmio_device_with_irq(IRQ_DEVICE_BASE, TestIrqDevice { level: false });
        assert_eq!(source.get(), 1);
        let mut machine = builder.build().unwrap();

        assert_eq!(
            machine.run(20).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt {
                retired_delta: 20,
                retired_total: 20,
            }
        );
        machine.device_mut(device).unwrap().level = true;
        let outcome = machine.run(32).unwrap();
        assert_eq!(
            outcome,
            Rv32MachineOutcome::Halted {
                exit_code: 0,
                retired_delta: 17,
                retired_total: 37,
            },
            "backend {execution:?}: pc={:#010x}, debug={:?}, irq_level={}",
            machine.pc(),
            machine.debug_bytes(),
            machine.device(device).unwrap().level,
        );
        assert_eq!(machine.debug_bytes(), &[11, 1]);
        assert!(!machine.device(device).unwrap().level);
    }
}

#[test]
fn all_backends_wake_wfi_for_external_interrupt_without_global_mie() {
    const IRQ_DEVICE_BASE: u32 = 0x1000_1000;
    let [priority_hi, priority_lo] = materialize(1, PLIC_BASE + 4);
    let [enable_hi, enable_lo] = materialize(1, PLIC_BASE + 0x2000);
    let [threshold_hi, threshold_lo] = materialize(1, PLIC_BASE + 0x200000);
    let [meie_hi, meie_lo] = materialize(4, MIE_MEIE as u32);
    let elf = machine_program_elf(&[
        priority_hi,
        priority_lo,
        addi(2, 0, 1),
        sw(1, 2, 0),
        enable_hi,
        enable_lo,
        addi(2, 0, 1 << 1),
        sw(1, 2, 0),
        threshold_hi,
        threshold_lo,
        sw(1, 0, 0),
        meie_hi,
        meie_lo,
        csrrw(0, CSR_MIE, 4),
        wfi(),
        lui(10, 0x10000),
        addi(11, 0, STATUS_HALTED),
        sw(10, 11, 0),
    ]);

    for execution in configs() {
        let mut builder = Rv32MachineBuilder::from_elf(&elf, config(execution, 0)).unwrap();
        let (device, _) =
            builder.add_mmio_device_with_irq(IRQ_DEVICE_BASE, TestIrqDevice { level: false });
        let mut machine = builder.build().unwrap();

        assert_eq!(
            machine.run(15).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt {
                retired_delta: 15,
                retired_total: 15,
            }
        );
        machine.device_mut(device).unwrap().level = true;
        assert_eq!(
            machine.run(3).unwrap(),
            Rv32MachineOutcome::Halted {
                exit_code: 0,
                retired_delta: 3,
                retired_total: 18,
            }
        );
    }
}

#[test]
fn all_backends_wake_wfi_and_take_machine_timer_interrupt() {
    let [timer_hi, timer_lo] = materialize(1, TIMER_BASE);
    let [handler_hi, handler_lo] = materialize(3, 0x2000);
    let main = [
        timer_hi,
        timer_lo,
        addi(2, 0, 5),
        sw(1, 2, 0),
        sw(1, 0, 4),
        handler_hi,
        handler_lo,
        csrrw(0, CSR_MTVEC, 3),
        addi(4, 0, MIE_MTIE),
        csrrw(0, CSR_MIE, 4),
        addi(5, 0, MSTATUS_MIE),
        csrrw(0, CSR_MSTATUS, 5),
        wfi(),
        lui(10, 0x10000),
        addi(11, 0, STATUS_HALTED),
        sw(10, 11, 0),
    ];
    let [debug_hi, debug_lo] = materialize(6, DEBUG_BASE);
    let handler = [
        csrrs(7, CSR_MCAUSE, 0),
        debug_hi,
        debug_lo,
        sb(6, 7, 0),
        csrrw(0, CSR_MIE, 0),
        mret(),
    ];
    let words = |words: &[u32]| {
        words
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    };
    let elf = Elf32Builder::new(0x1000)
        .load(LoadSegment::rx(0x1000, words(&main)))
        .load(LoadSegment::rx(0x2000, words(&handler)))
        .load(LoadSegment::rw_with_mem_size(0x3000, [], 0x1000))
        .finish();

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 1)).unwrap();
        assert_eq!(
            machine.run(13).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt {
                retired_delta: 13,
                retired_total: 13,
            }
        );

        machine.advance_time(4);
        assert_eq!(
            machine.run(16).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt {
                retired_delta: 0,
                retired_total: 13,
            }
        );
        machine.advance_time(1);
        assert_eq!(
            machine.run(16).unwrap(),
            Rv32MachineOutcome::Halted {
                exit_code: 0,
                retired_delta: 9,
                retired_total: 22,
            }
        );
        assert_eq!(machine.debug_bytes(), &[7]);
    }
}

#[test]
fn all_backends_wake_wfi_without_trapping_when_global_mie_is_clear() {
    let [timer_hi, timer_lo] = materialize(1, TIMER_BASE);
    let elf = machine_program_elf(&[
        timer_hi,
        timer_lo,
        addi(2, 0, 5),
        sw(1, 2, 0),
        sw(1, 0, 4),
        addi(4, 0, MIE_MTIE),
        csrrw(0, CSR_MIE, 4),
        wfi(),
        lui(10, 0x10000),
        addi(11, 0, STATUS_HALTED),
        sw(10, 11, 0),
    ]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 0)).unwrap();
        assert!(matches!(
            machine.run(8).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt {
                retired_delta: 8,
                retired_total: 8,
            }
        ));
        machine.advance_time(5);
        assert_eq!(
            machine.run(3).unwrap(),
            Rv32MachineOutcome::Halted {
                exit_code: 0,
                retired_delta: 3,
                retired_total: 11,
            }
        );
    }
}

#[test]
fn all_backends_keep_wfi_asleep_when_pending_timer_is_individually_masked() {
    let [timer_hi, timer_lo] = materialize(1, TIMER_BASE);
    let elf = machine_program_elf(&[
        timer_hi,
        timer_lo,
        addi(2, 0, 5),
        sw(1, 2, 0),
        sw(1, 0, 4),
        wfi(),
        jal(0, 0),
    ]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 0)).unwrap();
        assert!(matches!(
            machine.run(6).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt {
                retired_delta: 6,
                retired_total: 6,
            }
        ));
        machine.advance_time(5);
        assert_eq!(
            machine.run(1_000).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt {
                retired_delta: 0,
                retired_total: 6,
            }
        );
    }
}

#[test]
fn all_backends_route_machine_timer_interrupt_through_vectored_mtvec() {
    let [timer_hi, timer_lo] = materialize(1, TIMER_BASE);
    let [vector_hi, vector_lo] = materialize(3, 0x2001);
    let main = [
        timer_hi,
        timer_lo,
        addi(2, 0, 1),
        sw(1, 2, 0),
        sw(1, 0, 4),
        vector_hi,
        vector_lo,
        csrrw(0, CSR_MTVEC, 3),
        addi(4, 0, MIE_MTIE),
        csrrw(0, CSR_MIE, 4),
        addi(5, 0, MSTATUS_MIE),
        csrrw(0, CSR_MSTATUS, 5),
        wfi(),
    ];
    let handler = [lui(10, 0x10000), addi(11, 0, STATUS_HALTED), sw(10, 11, 0)];
    let words = |words: &[u32]| {
        words
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    };
    let elf = Elf32Builder::new(0x1000)
        .load(LoadSegment::rx(0x1000, words(&main)))
        .load(LoadSegment::rx(0x201c, words(&handler)))
        .load(LoadSegment::rw_with_mem_size(0x3000, [], 0x1000))
        .finish();

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 0)).unwrap();
        assert!(matches!(
            machine.run(13).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt { .. }
        ));
        machine.advance_time(1);
        assert_eq!(
            machine.run(3).unwrap(),
            Rv32MachineOutcome::Halted {
                exit_code: 0,
                retired_delta: 3,
                retired_total: 16,
            }
        );
    }
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
            register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
        },
    ]
}

#[test]
fn cached_dbt_is_the_default_execution_backend() {
    assert_eq!(
        DEFAULT_DBT_CODE_ALIGNMENT,
        Rv32DbtCodeAlignment::BlockBase(64)
    );
    assert_eq!(
        Rv32ExecutionBackendConfig::default(),
        Rv32ExecutionBackendConfig::CachedDbt {
            sets: 256,
            max_instructions: 16,
            scratch_bytes: 8 * 1024,
            cache_bytes: 128 * 1024,
            code_alignment: Rv32DbtCodeAlignment::BlockBase(64),
            register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
        }
    );
}

#[test]
fn cached_dbt_register_profiles_preserve_execution() {
    assert_eq!(
        DEFAULT_DBT_REGISTER_PROFILE,
        Rv32DbtRegisterProfile::RcxOverflow8
    );
    let elf = halting_machine_elf(b'R');

    for register_profile in [
        Rv32DbtRegisterProfile::Stable7,
        Rv32DbtRegisterProfile::RcxOverflow8,
    ] {
        let execution = Rv32ExecutionBackendConfig::CachedDbt {
            sets: DEFAULT_DBT_CACHE_SETS,
            max_instructions: DEFAULT_DBT_MAX_INSTRUCTIONS,
            scratch_bytes: DEFAULT_DBT_SCRATCH_BYTES,
            cache_bytes: DEFAULT_DBT_CODE_BYTES,
            code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
            register_profile,
        };
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 16)).unwrap();

        assert_eq!(
            machine.run(0).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 0,
                retired_total: 0,
            }
        );
        assert_eq!(
            machine.run(3).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 4,
                retired_total: 4,
            }
        );
        assert_eq!(
            machine.run(64).unwrap(),
            Rv32MachineOutcome::Halted {
                exit_code: 0,
                retired_delta: 3,
                retired_total: 7,
            }
        );
    }
}

#[test]
fn cached_dbt_initializes_one_context_per_run_call() {
    let elf = machine_program_elf(&[addi(1, 1, 1), jal(0, -4)]);
    let mut machine = Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::CachedDbt {
                sets: 8,
                max_instructions: 8,
                scratch_bytes: 4096,
                cache_bytes: 4096,
                code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            },
            0,
        ),
    )
    .unwrap();

    assert!(matches!(
        machine.run(16).unwrap(),
        Rv32MachineOutcome::BudgetExhausted {
            retired_delta: 16,
            retired_total: 16,
        }
    ));
    let first = machine.dbt_stats().unwrap();
    assert_eq!(first.native_dispatches, 1);
    #[cfg(not(feature = "dbt-chain-stats"))]
    assert_eq!(first.chain_transitions, None);
    #[cfg(feature = "dbt-chain-stats")]
    assert!(first.chain_transitions.unwrap() > 0);
    assert_eq!(first.context_initializations, 1);

    machine.run(16).unwrap();
    assert_eq!(machine.dbt_stats().unwrap().context_initializations, 2);
}

#[cfg(feature = "dbt-code-audit")]
#[test]
fn cached_dbt_snapshot_owns_final_linked_code() {
    let elf = machine_program_elf(&[addi(1, 1, 1), jal(0, -4)]);
    let mut machine = Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::CachedDbt {
                sets: 8,
                max_instructions: 8,
                scratch_bytes: 4096,
                cache_bytes: 4096,
                code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            },
            0,
        ),
    )
    .unwrap();

    machine.run(16).unwrap();
    let snapshot = machine.dbt_code_snapshot().unwrap().unwrap();
    assert!(!snapshot.used_bytes.is_empty());
    assert_eq!(snapshot.support_code.len(), 1);
    assert_eq!(snapshot.support_code[0].offset, 0);
    assert!(snapshot.support_code[0].length > 0);
    assert!(snapshot
        .blocks
        .windows(2)
        .all(|pair| pair[0].offset < pair[1].offset));
    assert!(snapshot
        .blocks
        .iter()
        .flat_map(|block| &block.edges)
        .any(|edge| edge.linked));

    drop(machine);
    assert!(snapshot.blocks.iter().all(|block| {
        usize::try_from(block.offset)
            .ok()
            .zip(usize::try_from(block.length).ok())
            .and_then(|(offset, length)| offset.checked_add(length))
            .is_some_and(|end| end <= snapshot.used_bytes.len())
    }));
}

#[cfg(feature = "dbt-code-audit")]
#[test]
fn non_dbt_backend_has_no_code_snapshot() {
    let elf = machine_program_elf(&[jal(0, 0)]);
    let machine = Rv32Machine::from_elf(
        &elf,
        config(Rv32ExecutionBackendConfig::Cached { sets: 8 }, 0),
    )
    .unwrap();

    assert!(machine.dbt_code_snapshot().unwrap().is_none());
}

#[cfg(feature = "dbt-translation-timing")]
#[test]
fn dbt_phase_timing_is_disabled_by_default() {
    let elf = machine_program_elf(&[addi(1, 1, 1), jal(0, -4)]);
    let mut machine = Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::CachedDbt {
                sets: 8,
                max_instructions: 8,
                scratch_bytes: 4096,
                cache_bytes: 4096,
                code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            },
            0,
        ),
    )
    .unwrap();

    machine.run(16).unwrap();
    let stats = machine.dbt_stats().unwrap();
    assert_eq!(stats.lift_nanos, 0);
    assert_eq!(stats.lower_nanos, 0);
    assert_eq!(stats.publish_nanos, 0);
    assert_eq!(stats.timed_translations, 0);
    assert!(stats.translations > 0);
}

#[cfg(feature = "dbt-translation-timing")]
#[test]
fn dbt_phase_timing_accounts_for_every_translation() {
    let elf = machine_program_elf(&[addi(1, 1, 1), jal(0, -4)]);
    let mut machine = Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::CachedDbt {
                sets: 8,
                max_instructions: 8,
                scratch_bytes: 4096,
                cache_bytes: 4096,
                code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            },
            0,
        ),
    )
    .unwrap();

    machine.enable_dbt_translation_timing();
    machine.run(16).unwrap();
    let stats = machine.dbt_stats().unwrap();
    assert!(stats.lift_nanos > 0);
    assert!(stats.lower_nanos > 0);
    assert!(stats.publish_nanos > 0);
    assert_eq!(stats.timed_translations, stats.translations);
}

#[test]
fn cached_dbt_finishes_one_block_past_budget_without_debt() {
    let mut words = vec![addi(1, 1, 1); 11];
    words.push(jal(0, -(11 * 4)));
    let elf = machine_program_elf(&words);
    let mut machine = Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::CachedDbt {
                sets: 32,
                max_instructions: 16,
                scratch_bytes: 4096,
                cache_bytes: 4096,
                code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            },
            0,
        ),
    )
    .unwrap();

    assert!(matches!(
        machine.run(5).unwrap(),
        Rv32MachineOutcome::BudgetExhausted {
            retired_delta: 12,
            retired_total: 12,
        }
    ));
    assert!(matches!(
        machine.run(5).unwrap(),
        Rv32MachineOutcome::BudgetExhausted {
            retired_delta: 12,
            retired_total: 24,
        }
    ));
    let stats = machine.dbt_stats().unwrap();
    assert_eq!(stats.budget_overshoot, 14);
    assert_eq!(stats.max_budget_overshoot, 7);
}

#[test]
fn all_backends_run_from_elf_entry_under_budget_and_halt_through_mmio() {
    let elf = halting_machine_elf(b'R');

    for execution in configs() {
        let config = Rv32MachineConfig {
            ram_size: 0x10_000,
            debug_limit: 16,
            execution,
        };
        let mut machine = Rv32Machine::from_elf(&elf, config).unwrap();
        assert_eq!(machine.pc(), 0x1000);
        assert_eq!(machine.control_status(), STATUS_BOOTING);
        assert_eq!(
            machine.run(0).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 0,
                retired_total: 0,
            }
        );
        let first_retired = if matches!(execution, Rv32ExecutionBackendConfig::CachedDbt { .. }) {
            4
        } else {
            3
        };
        assert_eq!(
            machine.run(3).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: first_retired,
                retired_total: first_retired,
            }
        );
        assert_eq!(
            machine.run(64).unwrap(),
            Rv32MachineOutcome::Halted {
                exit_code: 0,
                retired_delta: 7 - first_retired,
                retired_total: 7,
            }
        );
        assert_eq!(machine.debug_bytes(), b"R");
        assert_eq!(machine.control_status(), STATUS_HALTED);
        assert_eq!(machine.retired_instructions(), 7);
    }
}

#[test]
fn precise_backends_match_cached_for_every_partial_budget_prefix() {
    let elf = halting_machine_elf(b'P');
    for budget in 0..=8 {
        let mut reference = Rv32Machine::from_elf(
            &elf,
            config(Rv32ExecutionBackendConfig::Cached { sets: 64 }, 16),
        )
        .unwrap();
        let expected = reference.run(budget).unwrap();
        let expected_pc = reference.pc();
        let expected_retired = reference.retired_instructions();
        let expected_debug = reference.debug_bytes().to_vec();
        let expected_status = reference.control_status();

        for execution in configs() {
            if matches!(execution, Rv32ExecutionBackendConfig::CachedDbt { .. }) {
                continue;
            }
            let mut machine = Rv32Machine::from_elf(&elf, config(execution, 16)).unwrap();
            assert_eq!(
                machine.run(budget).unwrap(),
                expected,
                "{execution:?} budget {budget}"
            );
            assert_eq!(machine.pc(), expected_pc, "{execution:?} budget {budget}");
            assert_eq!(
                machine.retired_instructions(),
                expected_retired,
                "{execution:?} budget {budget}"
            );
            assert_eq!(
                machine.debug_bytes(),
                expected_debug,
                "{execution:?} budget {budget}"
            );
            assert_eq!(
                machine.control_status(),
                expected_status,
                "{execution:?} budget {budget}"
            );
        }
    }
}

#[test]
fn all_backends_treat_fence_i_as_a_retired_execution_boundary() {
    let elf = machine_program_elf(&[fence_i(), jal(0, 0)]);
    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 0)).unwrap();
        assert_eq!(
            machine.run(1).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 1,
                retired_total: 1,
            }
        );
        assert_eq!(machine.pc(), 0x1004);
    }
}

#[test]
fn all_backends_execute_the_same_rv32a_reservation_and_amo_program() {
    let [data_hi, data_lo] = materialize(1, 0x3000);
    let [debug_hi, debug_lo] = materialize(10, DEBUG_BASE);
    let elf = machine_program_elf(&[
        data_hi,
        data_lo,
        addi(2, 0, 41),
        amoswap_w(3, 1, 2, true, true),
        lr_w(4, 1, true, false),
        addi(5, 0, 42),
        sc_w(6, 1, 5, false, true),
        lr_w(7, 1, false, false),
        sw(1, 5, 0),
        sc_w(8, 1, 2, false, false),
        debug_hi,
        debug_lo,
        sb(10, 3, 0),
        sb(10, 4, 0),
        sb(10, 6, 0),
        sb(10, 7, 0),
        sb(10, 8, 0),
        addi(10, 10, -0x100),
        sw(10, 0, 8),
        addi(11, 0, STATUS_HALTED),
        sw(10, 11, 0),
    ]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 8)).unwrap();
        assert_eq!(
            machine.run(64).unwrap(),
            Rv32MachineOutcome::Halted {
                exit_code: 0,
                retired_delta: 21,
                retired_total: 21,
            }
        );
        assert_eq!(machine.debug_bytes(), &[0, 41, 0, 42, 1]);
    }
}

#[test]
fn all_backends_execute_every_zbb_operation() {
    let [lhs_hi, lhs_lo] = materialize(1, 0x8001_8080);
    let [rhs_hi, rhs_lo] = materialize(2, 5);
    let [debug_hi, debug_lo] = materialize(10, DEBUG_BASE);
    let operations = [
        andn(3, 1, 2),
        orn(3, 1, 2),
        xnor(3, 1, 2),
        min(3, 1, 2),
        minu(3, 1, 2),
        max(3, 1, 2),
        maxu(3, 1, 2),
        clz(3, 1),
        ctz(3, 1),
        cpop(3, 1),
        sext_b(3, 1),
        sext_h(3, 1),
        zext_h(3, 1),
        rol(3, 1, 2),
        ror(3, 1, 2),
        rori(3, 1, 7),
        orc_b(3, 1),
        rev8(3, 1),
    ];
    let mut words = vec![lhs_hi, lhs_lo, rhs_hi, rhs_lo, debug_hi, debug_lo];
    for operation in operations {
        words.push(operation);
        words.push(sb(10, 3, 0));
    }
    words.extend([
        addi(10, 10, -0x100),
        sw(10, 0, 8),
        addi(11, 0, STATUS_HALTED),
        sw(10, 11, 0),
    ]);
    let elf = machine_program_elf(&words);
    let expected = [
        0x80, 0xfa, 0x7a, 0x80, 0x05, 0x05, 0x80, 0x00, 0x07, 0x04, 0x80, 0x80, 0x80, 0x10, 0x04,
        0x01, 0xff, 0x80,
    ];

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, expected.len())).unwrap();
        assert!(matches!(
            machine.run(256).unwrap(),
            Rv32MachineOutcome::Halted { .. }
        ));
        assert_eq!(machine.debug_bytes(), expected, "{execution:?}");
    }
}

#[test]
fn all_backends_execute_aligned_ram_loads_and_stores_once() {
    let [data_hi, data_lo] = materialize(1, 0x3000);
    let [debug_hi, debug_lo] = materialize(10, DEBUG_BASE);
    let elf = machine_program_elf(&[
        data_hi,
        data_lo,
        addi(2, 0, 73),
        sw(1, 2, 0),
        lw(3, 1, 0),
        debug_hi,
        debug_lo,
        sb(10, 3, 0),
        addi(10, 10, -0x100),
        sw(10, 0, 8),
        addi(11, 0, STATUS_HALTED),
        sw(10, 11, 0),
    ]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 1)).unwrap();
        assert!(matches!(
            machine.run(32).unwrap(),
            Rv32MachineOutcome::Halted { .. }
        ));
        assert_eq!(machine.debug_bytes(), b"I");
        if matches!(
            execution,
            Rv32ExecutionBackendConfig::DirectDbt { .. }
                | Rv32ExecutionBackendConfig::CachedDbt { .. }
        ) {
            let stats = machine.dbt_stats().unwrap();
            assert!(stats.lowered_load_sites >= 1);
            assert!(stats.lowered_store_sites >= 1);
            assert!(stats.native_dispatches >= 1);
            assert!(machine.translation_bytes() > 4096);
        }
    }
}

#[test]
fn all_backends_trap_misaligned_ram_access_without_retiring_it() {
    let [data_hi, data_lo] = materialize(1, 0x3001);
    let elf = machine_program_elf(&[data_hi, data_lo, lw(2, 1, 0)]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 0)).unwrap();
        assert_eq!(
            machine.run(8).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 2,
                retired_total: 2,
            }
        );
        assert_eq!(machine.pc(), 0);
    }
}

#[test]
fn cached_dbt_fence_i_revokes_the_previous_generation() {
    let elf = machine_program_elf(&[fence_i(), jal(0, -4)]);
    let mut machine = Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::CachedDbt {
                sets: 4,
                max_instructions: 8,
                scratch_bytes: 4096,
                cache_bytes: 4096,
                code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            },
            0,
        ),
    )
    .unwrap();

    machine.run(2).unwrap();
    assert_eq!(machine.dbt_stats().unwrap().translations, 2);
    machine.run(1).unwrap();

    let stats = machine.dbt_stats().unwrap();
    assert_eq!(stats.translations, 3);
    assert_eq!(stats.native_dispatches, 3);
}

#[test]
fn cached_dbt_local_self_branch_flushes_loop_state_before_memory_fault() {
    let [vector_hi, vector_lo] = materialize(4, 0x102c);
    let [control_hi, control_lo] = materialize(10, CONTROL_BASE);
    let [address_hi, address_lo] = materialize(1, 0x0000_3ffc);
    let [limit_hi, limit_lo] = materialize(2, 0x0000_4004);
    let elf = machine_program_elf(&[
        vector_hi,
        vector_lo,
        csrrw(0, CSR_MTVEC, 4),
        address_hi,
        address_lo,
        limit_hi,
        limit_lo,
        jal(0, 4),
        lw(3, 1, 0),
        addi(1, 1, 4),
        bne(1, 2, -8),
        control_hi,
        control_lo,
        sw(10, 1, 8),
        addi(11, 0, STATUS_HALTED),
        sw(10, 11, 0),
    ]);
    let mut interpreted =
        Rv32Machine::from_elf(&elf, config(Rv32ExecutionBackendConfig::Predecoded, 0)).unwrap();
    let mut dbt = Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::CachedDbt {
                sets: 32,
                max_instructions: 8,
                scratch_bytes: 4096,
                cache_bytes: 4096,
                code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            },
            0,
        ),
    )
    .unwrap();

    let run_to_halt = |machine: &mut Rv32Machine| {
        for _ in 0..4 {
            let outcome = machine.run(20).unwrap();
            if matches!(outcome, Rv32MachineOutcome::Halted { .. }) {
                return outcome;
            }
        }
        panic!(
            "machine did not reach the fault handler: pc={:#010x}, retired={}, status={}",
            machine.pc(),
            machine.retired_instructions(),
            machine.control_status()
        );
    };
    let expected = run_to_halt(&mut interpreted);
    let actual = run_to_halt(&mut dbt);

    assert!(
        matches!(
            expected,
            Rv32MachineOutcome::Halted {
                exit_code: 0x0000_4000,
                ..
            }
        ),
        "unexpected reference outcome: {expected:?}"
    );
    assert_eq!(dbt.dbt_stats().unwrap().local_self_backedge_sites, 1);
    assert_eq!(actual, expected);
    assert_eq!(dbt.pc(), interpreted.pc());
    assert_eq!(
        dbt.retired_instructions(),
        interpreted.retired_instructions()
    );
}

#[test]
fn cached_dbt_reconciles_temporary_state_before_a_later_loop_fault() {
    let [vector_hi, vector_lo] = materialize(4, 0x2000);
    let [address_hi, address_lo] = materialize(5, 0x3ffc);
    let [second_hi, second_lo] = materialize(28, 0x3ffc);
    let [destination_hi, destination_lo] = materialize(18, 0x3000);
    let [limit_hi, limit_lo] = materialize(15, 0x4004);
    let main = [
        vector_hi,
        vector_lo,
        csrrw(0, CSR_MTVEC, 4),
        address_hi,
        address_lo,
        second_hi,
        second_lo,
        destination_hi,
        destination_lo,
        limit_hi,
        limit_lo,
        addi(14, 0, 1),
        addi(6, 0, 123),
        sw(5, 6, 0),
        lw(20, 5, 0),
        lw(24, 28, 0),
        addi(5, 5, 4),
        addi(28, 28, 4),
        slli(25, 20, 5),
        slli(26, 24, 4),
        add(20, 20, 14),
        add(24, 26, 24),
        add(20, 25, 20),
        add(20, 20, 24),
        sw(18, 20, 0),
        addi(18, 18, 4),
        bne(5, 15, -48),
    ];
    let [control_hi, control_lo] = materialize(10, CONTROL_BASE);
    let handler = [
        control_hi,
        control_lo,
        sw(10, 20, 8),
        addi(11, 0, STATUS_HALTED),
        sw(10, 11, 0),
    ];
    let words = |words: &[u32]| {
        words
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    };
    let elf = Elf32Builder::new(0x1000)
        .load(LoadSegment::rx(0x1000, words(&main)))
        .load(LoadSegment::rx(0x2000, words(&handler)))
        .load(LoadSegment::rw_with_mem_size(0x3000, [], 0x1000))
        .finish();
    let run = |execution| {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 0)).unwrap();
        for _ in 0..4 {
            let outcome = machine.run(64).unwrap();
            if matches!(outcome, Rv32MachineOutcome::Halted { .. }) {
                return (outcome, machine.dbt_stats());
            }
        }
        panic!("machine did not reach the loop fault handler");
    };

    let (expected, _) = run(Rv32ExecutionBackendConfig::Predecoded);
    let (actual, stats) = run(Rv32ExecutionBackendConfig::CachedDbt {
        sets: 32,
        max_instructions: 16,
        scratch_bytes: 4096,
        cache_bytes: 4096,
        code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
        register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
    });

    assert!(matches!(
        expected,
        Rv32MachineOutcome::Halted {
            exit_code: 6151,
            ..
        }
    ));
    assert_eq!(stats.unwrap().local_self_backedge_sites, 1);
    assert_eq!(actual, expected);

    #[cfg(feature = "dbt-tier1-prototype")]
    {
        let (tier1, stats) = run(Rv32ExecutionBackendConfig::CachedDbtTier1Prototype {
            sets: 32,
            max_instructions: 16,
            scratch_bytes: 4096,
            cache_bytes: 4096,
            code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
            register_profile: DEFAULT_DBT_REGISTER_PROFILE,
        });
        let stats = stats.unwrap();
        assert!(stats.tier1_regions >= 1);
        assert_eq!(tier1, expected);
    }
}

#[test]
fn all_backends_trap_atomic_mmio_without_device_side_effects() {
    let [debug_hi, debug_lo] = materialize(1, DEBUG_BASE);
    let elf = machine_program_elf(&[
        debug_hi,
        debug_lo,
        addi(2, 0, i32::from(b'A')),
        amoswap_w(3, 1, 2, false, false),
    ]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 8)).unwrap();
        assert_eq!(
            machine.run(8).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 3,
                retired_total: 3,
            }
        );
        assert_eq!(machine.debug_bytes(), b"");
        assert_eq!(machine.pc(), 0);
    }
}

#[test]
fn all_backends_trap_atomic_updates_to_rx_memory() {
    let [code_hi, code_lo] = materialize(1, 0x1000);
    let elf = machine_program_elf(&[
        code_hi,
        code_lo,
        addi(2, 0, 42),
        amoswap_w(3, 1, 2, false, false),
    ]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 0)).unwrap();
        assert_eq!(
            machine.run(8).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 3,
                retired_total: 3,
            }
        );
        assert_eq!(machine.pc(), 0);
    }
}

fn config(execution: Rv32ExecutionBackendConfig, debug_limit: usize) -> Rv32MachineConfig {
    Rv32MachineConfig {
        ram_size: 0x10_000,
        debug_limit,
        execution,
    }
}

#[cfg(feature = "dbt-execution-profile")]
fn cached_dbt_machine(words: &[u32], sets: usize) -> Rv32Machine {
    let elf = machine_program_elf(words);
    Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::CachedDbt {
                sets,
                max_instructions: 8,
                scratch_bytes: 4096,
                cache_bytes: 4096,
                code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            },
            0,
        ),
    )
    .unwrap()
}

#[cfg(feature = "dbt-execution-profile")]
#[test]
fn exact_profile_must_be_enabled_once_before_translation() {
    let mut machine = cached_dbt_machine(&[addi(1, 1, 1), jal(0, -4)], 8);
    machine.enable_dbt_execution_profile(128).unwrap();
    assert!(machine.enable_dbt_execution_profile(128).is_err());
    machine.run(1).unwrap();
    let snapshot = machine.dbt_execution_profile().unwrap().unwrap();
    assert_eq!(snapshot.capacity, 128);

    let mut late = cached_dbt_machine(&[addi(1, 1, 1), jal(0, -4)], 8);
    late.run(1).unwrap();
    assert!(late.enable_dbt_execution_profile(128).is_err());
}

#[cfg(feature = "dbt-execution-profile")]
#[test]
fn exact_profile_rejects_unsupported_backends_and_geometry() {
    for capacity in [0, 3] {
        let mut machine = cached_dbt_machine(&[jal(0, 0)], 8);
        assert!(machine.enable_dbt_execution_profile(capacity).is_err());
    }
    let elf = machine_program_elf(&[jal(0, 0)]);
    let mut interpreted = Rv32Machine::from_elf(
        &elf,
        config(Rv32ExecutionBackendConfig::Cached { sets: 16 }, 0),
    )
    .unwrap();
    assert!(interpreted.enable_dbt_execution_profile(128).is_err());
    assert!(interpreted.dbt_execution_profile().is_err());
}

#[cfg(feature = "dbt-execution-profile")]
fn block_count(profile: &Rv32DbtExecutionProfile, pc: u32) -> u64 {
    profile
        .blocks
        .iter()
        .find(|block| block.pc == pc)
        .map_or(0, |block| block.executions)
}

#[cfg(feature = "dbt-execution-profile")]
fn edge_count(
    profile: &Rv32DbtExecutionProfile,
    source_pc: u32,
    target_pc: u32,
    kind: Rv32DbtProfileEdgeKind,
) -> u64 {
    profile
        .static_edges
        .iter()
        .find(|edge| {
            edge.source_pc == source_pc && edge.target_pc == target_pc && edge.kind == kind
        })
        .map_or(0, |edge| edge.executions)
}

#[cfg(feature = "dbt-execution-profile")]
#[test]
fn exact_profile_counts_initial_chained_and_static_edge_paths() {
    let [control_hi, control_lo] = materialize(2, CONTROL_BASE);
    let mut machine = cached_dbt_machine(
        &[
            addi(1, 0, 4),
            addi(1, 1, -1),
            bne(1, 0, -4),
            control_hi,
            control_lo,
            sw(2, 0, 8),
            addi(3, 0, STATUS_HALTED),
            sw(2, 3, 0),
        ],
        32,
    );
    machine.enable_dbt_execution_profile(256).unwrap();

    assert!(matches!(
        machine.run(64).unwrap(),
        Rv32MachineOutcome::Halted { .. }
    ));
    let profile = machine.dbt_execution_profile().unwrap().unwrap();
    assert_eq!(block_count(&profile, 0x1000), 1);
    assert_eq!(block_count(&profile, 0x1004), 3);
    assert_eq!(
        edge_count(&profile, 0x1000, 0x1004, Rv32DbtProfileEdgeKind::Taken),
        1
    );
    assert_eq!(
        edge_count(&profile, 0x1004, 0x1004, Rv32DbtProfileEdgeKind::Taken),
        2
    );
    assert_eq!(
        edge_count(
            &profile,
            0x1004,
            0x100c,
            Rv32DbtProfileEdgeKind::Fallthrough
        ),
        1
    );
}

#[cfg(feature = "dbt-execution-profile")]
#[test]
fn exact_profile_local_self_branch_stops_before_next_body_at_budget() {
    let mut machine = cached_dbt_machine(&[addi(1, 0, 4), addi(1, 1, -1), bne(1, 0, -4)], 32);
    machine.enable_dbt_execution_profile(128).unwrap();

    assert_eq!(
        machine.run(7).unwrap(),
        Rv32MachineOutcome::BudgetExhausted {
            retired_delta: 7,
            retired_total: 7,
        }
    );
    assert_eq!(machine.pc(), 0x1004);
    let profile = machine.dbt_execution_profile().unwrap().unwrap();
    assert_eq!(block_count(&profile, 0x1000), 1);
    assert_eq!(block_count(&profile, 0x1004), 2);
    assert_eq!(
        edge_count(&profile, 0x1004, 0x1004, Rv32DbtProfileEdgeKind::Taken),
        2
    );
    assert_eq!(profile.dynamic_exits.budget, 1);
}

#[cfg(feature = "dbt-execution-profile")]
#[test]
fn exact_profile_counts_dynamic_exit_categories() {
    let [target_hi, target_lo] = materialize(1, 0x100c);
    let [control_hi, control_lo] = materialize(2, CONTROL_BASE);
    let mut machine = cached_dbt_machine(
        &[
            target_hi,
            target_lo,
            jalr(0, 1, 0),
            control_hi,
            control_lo,
            sw(2, 0, 8),
            addi(3, 0, STATUS_HALTED),
            sw(2, 3, 0),
        ],
        32,
    );
    machine.enable_dbt_execution_profile(256).unwrap();

    assert!(matches!(
        machine.run(64).unwrap(),
        Rv32MachineOutcome::Halted { .. }
    ));
    let exits = machine
        .dbt_execution_profile()
        .unwrap()
        .unwrap()
        .dynamic_exits;
    assert_eq!(exits.jalr, 1);
    assert_eq!(exits.memory_access, 2);
    assert_eq!(exits.trap_or_terminal, 1);
}

#[cfg(feature = "dbt-execution-profile")]
#[test]
fn exact_profile_counts_chained_budget_exit() {
    let mut machine = cached_dbt_machine(&[jal(0, 0)], 16);
    machine.enable_dbt_execution_profile(64).unwrap();

    assert!(matches!(
        machine.run(5).unwrap(),
        Rv32MachineOutcome::BudgetExhausted {
            retired_delta: 5,
            ..
        }
    ));
    let profile = machine.dbt_execution_profile().unwrap().unwrap();
    assert_eq!(profile.dynamic_exits.budget, 1);
    assert_eq!(profile.dynamic_exits.trap_or_terminal, 0);
    assert_eq!(block_count(&profile, 0x1000), 5);
}

#[cfg(feature = "dbt-execution-profile")]
#[test]
fn exact_profile_survives_cache_eviction_and_retranslation() {
    let mut machine = cached_dbt_machine(&[jal(0, 8), jal(0, -4), jal(0, -4)], 1);
    machine.enable_dbt_execution_profile(64).unwrap();

    assert!(matches!(
        machine.run(12).unwrap(),
        Rv32MachineOutcome::BudgetExhausted { .. }
    ));
    let stats = machine.dbt_stats().unwrap();
    assert!(stats.evictions > 0);
    assert!(stats.translations > 3);
    let profile = machine.dbt_execution_profile().unwrap().unwrap();
    assert_eq!(block_count(&profile, 0x1000), 4);
    assert_eq!(block_count(&profile, 0x1004), 4);
    assert_eq!(block_count(&profile, 0x1008), 4);
    assert_eq!(profile.blocks.len(), 3);
}

#[cfg(feature = "dbt-execution-profile")]
#[test]
fn exact_profile_survives_fence_i_generation_invalidation() {
    let mut machine = cached_dbt_machine(&[fence_i(), jal(0, -4)], 4);
    machine.enable_dbt_execution_profile(64).unwrap();

    machine.run(2).unwrap();
    let before = machine.dbt_execution_profile().unwrap().unwrap();
    assert_eq!(block_count(&before, 0x1000), 1);
    assert_eq!(block_count(&before, 0x1004), 1);

    machine.run(2).unwrap();
    let after = machine.dbt_execution_profile().unwrap().unwrap();
    assert_eq!(block_count(&after, 0x1000), 2);
    assert_eq!(block_count(&after, 0x1004), 2);
    assert_eq!(after.blocks.len(), 2);
    assert!(machine.dbt_stats().unwrap().translations > 2);
}

#[cfg(all(feature = "dbt-execution-profile", feature = "dbt-code-audit"))]
#[test]
fn disabled_profile_keeps_cached_dbt_code_identical() {
    let words = [addi(1, 1, 1), jal(0, -4)];
    let mut first = cached_dbt_machine(&words, 8);
    let mut second = cached_dbt_machine(&words, 8);
    first.run(16).unwrap();
    second.run(16).unwrap();

    let first_snapshot = first.dbt_code_snapshot().unwrap().unwrap();
    let second_snapshot = second.dbt_code_snapshot().unwrap().unwrap();
    assert_eq!(first_snapshot, second_snapshot);

    let mut profiled = cached_dbt_machine(&words, 8);
    profiled.enable_dbt_execution_profile(64).unwrap();
    profiled.run(16).unwrap();
    let profiled_snapshot = profiled.dbt_code_snapshot().unwrap().unwrap();
    assert!(profiled_snapshot.used_bytes.len() > first_snapshot.used_bytes.len());
    assert!(profiled.dbt_stats().unwrap().emitted_bytes > first.dbt_stats().unwrap().emitted_bytes);
}

#[test]
fn all_backends_turn_execution_from_rw_memory_into_bounded_traps() {
    let elf = machine_program_elf(&[jal(0, 0x2000)]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 8)).unwrap();
        assert_eq!(
            machine.run(2).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 1,
                retired_total: 1,
            }
        );
        assert_eq!(machine.pc(), 0);
        assert_eq!(
            machine.run(3).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 0,
                retired_total: 1,
            }
        );
    }
}

#[test]
fn all_backends_trap_guest_writes_to_rx_memory_without_retiring_the_store() {
    let [address_hi, address_lo] = materialize(1, 0x1000);
    let elf = machine_program_elf(&[address_hi, address_lo, addi(2, 0, 7), sw(1, 2, 0)]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 8)).unwrap();
        assert_eq!(
            machine.run(8).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 3,
                retired_total: 3,
            }
        );
        assert_eq!(machine.pc(), 0);
    }
}

#[test]
fn all_backends_turn_invalid_ecall_and_ebreak_into_non_retiring_traps() {
    for word in [0xffff_ffff, ecall(), ebreak()] {
        let elf = machine_program_elf(&[word]);
        for execution in configs() {
            let mut machine = Rv32Machine::from_elf(&elf, config(execution, 8)).unwrap();
            assert_eq!(
                machine.run(1).unwrap(),
                Rv32MachineOutcome::BudgetExhausted {
                    retired_delta: 0,
                    retired_total: 0,
                }
            );
            assert_eq!(machine.pc(), 0);
        }
    }
}

#[test]
fn bounded_debug_overflow_is_a_guest_trap() {
    let elf = machine_program_elf(&[
        lui(1, 0x10000),
        addi(2, 1, 0x100),
        addi(3, 0, i32::from(b'A')),
        sb(2, 3, 0),
        addi(3, 0, i32::from(b'B')),
        sb(2, 3, 0),
    ]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 1)).unwrap();
        assert_eq!(
            machine.run(16).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 5,
                retired_total: 5,
            }
        );
        assert_eq!(machine.debug_bytes(), b"A");
    }
}

#[test]
fn faulting_halfword_mmio_stores_have_no_partial_device_effects() {
    let [debug_hi, debug_lo] = materialize(1, DEBUG_BASE);
    let debug_elf =
        machine_program_elf(&[debug_hi, debug_lo, addi(2, 0, i32::from(b'A')), sh(1, 2, 0)]);
    let [control_hi, control_lo] = materialize(1, CONTROL_BASE);
    let control_elf = machine_program_elf(&[
        control_hi,
        control_lo,
        addi(2, 0, STATUS_HALTED),
        sh(1, 2, 0),
    ]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&debug_elf, config(execution, 8)).unwrap();
        assert_eq!(
            machine.run(8).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 3,
                retired_total: 3,
            }
        );
        assert_eq!(machine.debug_bytes(), b"");

        let mut machine = Rv32Machine::from_elf(&control_elf, config(execution, 0)).unwrap();
        assert_eq!(
            machine.run(8).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 3,
                retired_total: 3,
            }
        );
        assert_eq!(machine.control_status(), STATUS_BOOTING);
    }
}

#[test]
fn all_backends_share_precise_trap_entry_and_attempt_budgeting() {
    let [vector_hi, vector_lo] = materialize(1, 0x2000);
    let main = [vector_hi, vector_lo, csrrw(0, CSR_MTVEC, 1), ecall()];
    let [debug_hi, debug_lo] = materialize(5, DEBUG_BASE);
    let handler = [
        csrrs(2, CSR_MEPC, 0),
        csrrs(3, CSR_MCAUSE, 0),
        csrrs(4, CSR_MTVAL, 0),
        debug_hi,
        debug_lo,
        sb(5, 2, 0),
        sb(5, 3, 0),
        sb(5, 4, 0),
        lui(1, 0x10000),
        sw(1, 0, 8),
        addi(6, 0, STATUS_HALTED),
        sw(1, 6, 0),
    ];
    let words = |words: &[u32]| {
        words
            .iter()
            .copied()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    };
    let elf = Elf32Builder::new(0x1000)
        .load(LoadSegment::rx(0x1000, words(&main)))
        .load(LoadSegment::rx(0x2000, words(&handler)))
        .load(LoadSegment::rw_with_mem_size(0x3000, [], 0x1000))
        .finish();

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 3)).unwrap();
        assert_eq!(
            machine.run(3).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 3,
                retired_total: 3,
            }
        );
        assert_eq!(machine.pc(), 0x100c);
        assert_eq!(
            machine.run(1).unwrap(),
            Rv32MachineOutcome::BudgetExhausted {
                retired_delta: 0,
                retired_total: 3,
            }
        );
        assert_eq!(machine.pc(), 0x2000);
        assert_eq!(
            machine.run(32).unwrap(),
            Rv32MachineOutcome::Halted {
                exit_code: 0,
                retired_delta: 12,
                retired_total: 15,
            }
        );
        assert_eq!(machine.debug_bytes(), &[0x0c, 11, 0]);
    }
}

#[test]
fn panic_status_returns_the_guest_panic_code() {
    let elf = machine_program_elf(&[
        lui(1, 0x10000),
        addi(2, 0, 99),
        sw(1, 2, 4),
        addi(3, 0, STATUS_PANIC),
        sw(1, 3, 0),
    ]);

    for execution in configs() {
        let mut machine = Rv32Machine::from_elf(&elf, config(execution, 0)).unwrap();
        assert_eq!(
            machine.run(16).unwrap(),
            Rv32MachineOutcome::Panicked {
                panic_code: 99,
                retired_delta: 5,
                retired_total: 5,
            }
        );
    }
}

#[test]
fn invalid_cache_and_ram_layouts_fail_before_machine_allocation() {
    let elf = halting_machine_elf(b'R');
    assert!(Rv32Machine::from_elf(
        &elf,
        config(Rv32ExecutionBackendConfig::Cached { sets: 3 }, 8)
    )
    .is_err());
    assert!(Rv32Machine::from_elf(
        &elf,
        Rv32MachineConfig {
            ram_size: CONTROL_BASE as usize + 1,
            debug_limit: 8,
            execution: Rv32ExecutionBackendConfig::Predecoded,
        }
    )
    .is_err());
    assert!(Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::BlockCached {
                sets: 3,
                max_instructions: 8,
            },
            8,
        )
    )
    .is_err());
    assert!(Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::BlockCached {
                sets: 32,
                max_instructions: 0,
            },
            8,
        )
    )
    .is_err());
    assert!(Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::BlockCached {
                sets: 32,
                max_instructions: 65,
            },
            8,
        )
    )
    .is_err());
    assert!(Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::DirectDbt {
                max_instructions: 0,
                scratch_bytes: 4096,
            },
            8,
        )
    )
    .is_err());
    assert!(Rv32Machine::from_elf(
        &elf,
        config(
            Rv32ExecutionBackendConfig::CachedDbt {
                sets: 0,
                max_instructions: 8,
                scratch_bytes: 4096,
                cache_bytes: 4096,
                code_alignment: DEFAULT_DBT_CODE_ALIGNMENT,
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            },
            8,
        )
    )
    .is_err());
}

#[test]
fn cached_dbt_rejects_invalid_code_alignments() {
    let elf = machine_program_elf(&[jal(0, 0)]);
    for code_alignment in [
        Rv32DbtCodeAlignment::BlockBase(0),
        Rv32DbtCodeAlignment::BlockBase(8),
        Rv32DbtCodeAlignment::BlockBase(24),
        Rv32DbtCodeAlignment::BlockBase(512),
        Rv32DbtCodeAlignment::ChainEntry(0),
        Rv32DbtCodeAlignment::ChainEntry(8),
        Rv32DbtCodeAlignment::ChainEntry(24),
        Rv32DbtCodeAlignment::ChainEntry(512),
    ] {
        let error = Rv32Machine::from_elf(
            &elf,
            config(
                Rv32ExecutionBackendConfig::CachedDbt {
                    sets: 8,
                    max_instructions: 8,
                    scratch_bytes: 4096,
                    cache_bytes: 4096,
                    code_alignment,
                    register_profile: DEFAULT_DBT_REGISTER_PROFILE,
                },
                8,
            ),
        )
        .err()
        .expect("invalid Cached DBT alignment must fail construction");
        assert!(
            error
                .to_string()
                .contains("alignment must be a power of two between 16 and 256 bytes"),
            "unexpected error for {code_alignment:?}: {error}"
        );
    }
}
