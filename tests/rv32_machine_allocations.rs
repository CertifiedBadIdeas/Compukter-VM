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
#[allow(dead_code)]
mod rv32_elf_support;

use compukter_vm::rv32_machine::{
    Rv32ExecutionBackendConfig, Rv32Machine, Rv32MachineBuilder, Rv32MachineConfig,
    Rv32MachineOutcome, PLIC_BASE, TIMER_BASE,
};
use compukter_vm::rv32im::encoding::{
    addi, andn, bne, clz, cpop, csrrs, csrrw, ecall, jal, lr_w, lw, materialize, mret, orc_b, rev8,
    rol, rori, sc_w, sw, wfi,
};
use compukter_vm::{
    bus::{MmioAccessWidth, MmioContext, MmioDevice},
    memory::MemoryFault,
};
use rv32_elf_support::{machine_program_elf, Elf32Builder, LoadSegment};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

const CSR_MTVEC: u16 = 0x305;
const CSR_MSTATUS: u16 = 0x300;
const CSR_MIE: u16 = 0x304;
const CSR_MEPC: u16 = 0x341;

struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

struct AllocationIrqDevice {
    level: bool,
}

impl MmioDevice for AllocationIrqDevice {
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
            Err(MemoryFault::new("invalid allocation IRQ read".to_string()))
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
            Err(MemoryFault::new("invalid allocation IRQ write".to_string()))
        }
    }
}

fn assert_steady_state_trap_entry_and_return_allocate_nothing() {
    let [vector_hi, vector_lo] = materialize(1, 0x2000);
    let main = [
        vector_hi,
        vector_lo,
        csrrw(0, CSR_MTVEC, 1),
        ecall(),
        jal(0, -4),
    ];
    let handler = [
        csrrs(2, CSR_MEPC, 0),
        addi(2, 2, 4),
        csrrw(0, CSR_MEPC, 2),
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

    for execution in [
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
            code_alignment: compukter_vm::rv32_machine::DEFAULT_DBT_CODE_ALIGNMENT,
            register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
        },
    ] {
        let mut machine = Rv32Machine::from_elf(
            &elf,
            Rv32MachineConfig {
                ram_size: 0x10_000,
                debug_limit: 0,
                execution,
            },
        )
        .unwrap();
        assert!(matches!(
            machine.run(128).unwrap(),
            Rv32MachineOutcome::BudgetExhausted { .. }
        ));

        ALLOCATIONS.store(0, Ordering::Relaxed);
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        let outcome = machine.run(4096).unwrap();
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        let allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);

        assert!(matches!(
            outcome,
            Rv32MachineOutcome::BudgetExhausted { .. }
        ));
        assert_eq!(allocations, 0, "{execution:?} allocated in steady state");
        assert_eq!(
            allocated_bytes, 0,
            "{execution:?} allocated bytes in steady state"
        );
    }
}

fn assert_steady_state_atomic_increment_loop_allocates_nothing() {
    let [data_hi, data_lo] = materialize(1, 0x3000);
    let words = [
        data_hi,
        data_lo,
        lr_w(2, 1, true, false),
        addi(2, 2, 1),
        sc_w(3, 1, 2, false, true),
        bne(3, 0, -12),
        jal(0, -16),
    ];
    let code = words
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    let elf = Elf32Builder::new(0x1000)
        .load(LoadSegment::rx(0x1000, code))
        .load(LoadSegment::rw_with_mem_size(0x3000, [], 0x1000))
        .finish();

    for execution in [
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
            code_alignment: compukter_vm::rv32_machine::DEFAULT_DBT_CODE_ALIGNMENT,
            register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
        },
    ] {
        let mut machine = Rv32Machine::from_elf(
            &elf,
            Rv32MachineConfig {
                ram_size: 0x10_000,
                debug_limit: 0,
                execution,
            },
        )
        .unwrap();
        assert!(matches!(
            machine.run(128).unwrap(),
            Rv32MachineOutcome::BudgetExhausted { .. }
        ));

        ALLOCATIONS.store(0, Ordering::Relaxed);
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        let outcome = machine.run(4096).unwrap();
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        let allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);

        assert!(matches!(
            outcome,
            Rv32MachineOutcome::BudgetExhausted { .. }
        ));
        assert_eq!(allocations, 0, "{execution:?} allocated in atomic loop");
        assert_eq!(
            allocated_bytes, 0,
            "{execution:?} allocated bytes in atomic loop"
        );
    }
}

fn assert_block_cache_evictions_allocate_nothing() {
    let mut words = vec![addi(1, 1, 1); 7];
    words.push(jal(0, 4));
    words.push(jal(0, 4));
    words.extend([addi(2, 2, 1); 4]);
    words.push(jal(0, -52));
    let code = words
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    let elf = Elf32Builder::new(0x1000)
        .load(LoadSegment::rx(0x1000, code))
        .load(LoadSegment::rw_with_mem_size(0x3000, [], 0x1000))
        .finish();
    let execution = Rv32ExecutionBackendConfig::BlockCached {
        sets: 1,
        max_instructions: 8,
    };
    let mut machine = Rv32Machine::from_elf(
        &elf,
        Rv32MachineConfig {
            ram_size: 0x10_000,
            debug_limit: 0,
            execution,
        },
    )
    .unwrap();
    assert!(matches!(
        machine.run(128).unwrap(),
        Rv32MachineOutcome::BudgetExhausted { .. }
    ));

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let outcome = machine.run(4096).unwrap();
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);

    assert!(matches!(
        outcome,
        Rv32MachineOutcome::BudgetExhausted { .. }
    ));
    assert_eq!(allocations, 0, "{execution:?} allocated while evicting");
    assert_eq!(
        allocated_bytes, 0,
        "{execution:?} allocated bytes while evicting"
    );
}

fn assert_cached_dbt_zbb_loop_allocates_nothing() {
    let words = [
        andn(3, 1, 2),
        clz(4, 3),
        cpop(5, 3),
        rol(6, 3, 2),
        rori(7, 6, 7),
        orc_b(8, 7),
        rev8(9, 8),
        jal(0, -28),
    ];
    let code = words
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    let elf = Elf32Builder::new(0x1000)
        .load(LoadSegment::rx(0x1000, code))
        .load(LoadSegment::rw_with_mem_size(0x3000, [], 0x1000))
        .finish();
    let execution = Rv32ExecutionBackendConfig::CachedDbt {
        sets: 32,
        max_instructions: 8,
        scratch_bytes: 4096,
        cache_bytes: 4096,
        code_alignment: compukter_vm::rv32_machine::DEFAULT_DBT_CODE_ALIGNMENT,
        register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
    };
    let mut machine = Rv32Machine::from_elf(
        &elf,
        Rv32MachineConfig {
            ram_size: 0x10_000,
            debug_limit: 0,
            execution,
        },
    )
    .unwrap();
    machine.run(128).unwrap();

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let outcome = machine.run(4096).unwrap();
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);

    assert!(matches!(
        outcome,
        Rv32MachineOutcome::BudgetExhausted { .. }
    ));
    assert_eq!(allocations, 0, "Cached DBT allocated in a Zbb loop");
    assert_eq!(
        allocated_bytes, 0,
        "Cached DBT allocated bytes in a Zbb loop"
    );
}

fn assert_waiting_and_timer_wakeup_allocate_nothing() {
    let [timer_hi, timer_lo] = materialize(1, TIMER_BASE);
    let [vector_hi, vector_lo] = materialize(3, 0x2000);
    let main = [
        timer_hi,
        timer_lo,
        addi(2, 0, 1),
        sw(1, 2, 0),
        sw(1, 0, 4),
        vector_hi,
        vector_lo,
        csrrw(0, CSR_MTVEC, 3),
        addi(4, 0, 1 << 7),
        csrrw(0, CSR_MIE, 4),
        addi(5, 0, 1 << 3),
        csrrw(0, CSR_MSTATUS, 5),
        wfi(),
        jal(0, 0),
    ];
    let handler = [csrrw(0, CSR_MIE, 0), mret()];
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

    for execution in [
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
            code_alignment: compukter_vm::rv32_machine::DEFAULT_DBT_CODE_ALIGNMENT,
            register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
        },
    ] {
        let mut machine = Rv32Machine::from_elf(
            &elf,
            Rv32MachineConfig {
                ram_size: 0x10_000,
                debug_limit: 0,
                execution,
            },
        )
        .unwrap();
        assert!(matches!(
            machine.run(13).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt { .. }
        ));

        ALLOCATIONS.store(0, Ordering::Relaxed);
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        for _ in 0..32 {
            assert!(matches!(
                machine.run(4096).unwrap(),
                Rv32MachineOutcome::WaitingForInterrupt { .. }
            ));
        }
        machine.advance_time(1);
        assert!(matches!(
            machine.run(8).unwrap(),
            Rv32MachineOutcome::BudgetExhausted { .. }
        ));
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        let allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);

        assert_eq!(allocations, 0, "{execution:?} allocated around WFI/timer");
        assert_eq!(
            allocated_bytes, 0,
            "{execution:?} allocated bytes around WFI/timer"
        );
    }
}

fn assert_external_irq_wakeup_allocate_nothing() {
    const IRQ_DEVICE_BASE: u32 = 0x1000_1000;
    let [priority_hi, priority_lo] = materialize(1, PLIC_BASE + 4);
    let [enable_hi, enable_lo] = materialize(1, PLIC_BASE + 0x2000);
    let [threshold_hi, threshold_lo] = materialize(1, PLIC_BASE + 0x200000);
    let [vector_hi, vector_lo] = materialize(3, 0x2000);
    let [meie_hi, meie_lo] = materialize(4, 1 << 11);
    let main = [
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
        vector_hi,
        vector_lo,
        csrrw(0, CSR_MTVEC, 3),
        meie_hi,
        meie_lo,
        csrrw(0, CSR_MIE, 4),
        addi(5, 0, 1 << 3),
        csrrw(0, CSR_MSTATUS, 5),
        wfi(),
        jal(0, -4),
    ];
    let [claim_hi, claim_lo] = materialize(8, PLIC_BASE + 0x200004);
    let [device_hi, device_lo] = materialize(6, IRQ_DEVICE_BASE);
    let handler = [
        claim_hi,
        claim_lo,
        lw(9, 8, 0),
        device_hi,
        device_lo,
        sw(6, 0, 0),
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
    let elf = Elf32Builder::new(0x1000)
        .load(LoadSegment::rx(0x1000, words(&main)))
        .load(LoadSegment::rx(0x2000, words(&handler)))
        .load(LoadSegment::rw_with_mem_size(0x3000, [], 0x1000))
        .finish();

    for execution in [
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
            code_alignment: compukter_vm::rv32_machine::DEFAULT_DBT_CODE_ALIGNMENT,
            register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
        },
    ] {
        let mut builder = Rv32MachineBuilder::from_elf(
            &elf,
            Rv32MachineConfig {
                ram_size: 0x10_000,
                debug_limit: 0,
                execution,
            },
        )
        .unwrap();
        let (device, _) =
            builder.add_mmio_device_with_irq(IRQ_DEVICE_BASE, AllocationIrqDevice { level: false });
        let mut machine = builder.build().unwrap();
        assert!(matches!(
            machine.run(20).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt { .. }
        ));
        machine.device_mut(device).unwrap().level = true;
        assert!(matches!(
            machine.run(64).unwrap(),
            Rv32MachineOutcome::WaitingForInterrupt { .. }
        ));

        ALLOCATIONS.store(0, Ordering::Relaxed);
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        for _ in 0..32 {
            machine.device_mut(device).unwrap().level = true;
            assert!(matches!(
                machine.run(64).unwrap(),
                Rv32MachineOutcome::WaitingForInterrupt { .. }
            ));
        }
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        let allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);

        assert_eq!(allocations, 0, "{execution:?} allocated around PLIC/WFI");
        assert_eq!(
            allocated_bytes, 0,
            "{execution:?} allocated bytes around PLIC/WFI"
        );
    }
}

#[cfg(feature = "dbt-execution-profile")]
fn assert_profiled_cached_dbt_steady_state_allocates_nothing() {
    let words = [addi(1, 1, 1), jal(0, -4)];
    let code = words
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    let elf = Elf32Builder::new(0x1000)
        .load(LoadSegment::rx(0x1000, code))
        .load(LoadSegment::rw_with_mem_size(0x3000, [], 0x1000))
        .finish();
    let mut machine = Rv32Machine::from_elf(
        &elf,
        Rv32MachineConfig {
            ram_size: 0x10_000,
            debug_limit: 0,
            execution: Rv32ExecutionBackendConfig::CachedDbt {
                sets: 32,
                max_instructions: 8,
                scratch_bytes: 4096,
                cache_bytes: 4096,
                code_alignment: compukter_vm::rv32_machine::DEFAULT_DBT_CODE_ALIGNMENT,
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            },
        },
    )
    .unwrap();
    machine.enable_dbt_execution_profile(64).unwrap();
    machine.run(128).unwrap();

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    let outcome = machine.run(4096).unwrap();
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);

    assert!(matches!(
        outcome,
        Rv32MachineOutcome::BudgetExhausted { .. }
    ));
    assert_eq!(
        allocations, 0,
        "profiled Cached DBT allocated in steady state"
    );
    assert_eq!(
        allocated_bytes, 0,
        "profiled Cached DBT allocated bytes in steady state"
    );
    assert!(!machine
        .dbt_execution_profile()
        .unwrap()
        .unwrap()
        .blocks
        .is_empty());
}

#[test]
fn steady_state_machine_paths_allocate_nothing() {
    assert_steady_state_trap_entry_and_return_allocate_nothing();
    assert_steady_state_atomic_increment_loop_allocates_nothing();
    assert_block_cache_evictions_allocate_nothing();
    assert_cached_dbt_zbb_loop_allocates_nothing();
    assert_waiting_and_timer_wakeup_allocate_nothing();
    assert_external_irq_wakeup_allocate_nothing();
    assert_repeated_machine_inspection_allocates_nothing();
    #[cfg(feature = "dbt-execution-profile")]
    assert_profiled_cached_dbt_steady_state_allocates_nothing();
}

fn assert_repeated_machine_inspection_allocates_nothing() {
    let elf = machine_program_elf(&[jal(0, 0)]);
    let machine = Rv32Machine::from_elf(
        &elf,
        Rv32MachineConfig {
            ram_size: 0x10_000,
            debug_limit: 0,
            execution: Rv32ExecutionBackendConfig::CachedDbt {
                sets: 32,
                max_instructions: 8,
                scratch_bytes: 4096,
                cache_bytes: 4096,
                code_alignment: compukter_vm::rv32_machine::DEFAULT_DBT_CODE_ALIGNMENT,
                register_profile: compukter_vm::rv32_machine::DEFAULT_DBT_REGISTER_PROFILE,
            },
        },
    )
    .unwrap();

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    for _ in 0..1_000 {
        std::hint::black_box(machine.inspection_snapshot());
    }

    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(ALLOCATED_BYTES.load(Ordering::Relaxed), 0);
}
