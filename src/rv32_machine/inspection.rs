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

use super::{Rv32DbtStats, Rv32TranslationStats};

/// Maximum number of interrupt sources represented by an RV32 PLIC inspection.
pub const RV32_PLIC_MAX_SOURCES: usize = 1023;

/// Copy-only view of the implemented RV32 machine CSRs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rv32CsrInspection {
    pub mstatus: u32,
    pub mie: u32,
    pub mip: u32,
    pub mtvec: u32,
    pub mscratch: u32,
    pub mepc: u32,
    pub mcause: u32,
    pub mtval: u32,
}

/// Copy-only view of one RV32 hart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rv32HartInspection {
    pub registers: [u32; 32],
    pub pc: u32,
    pub retired_instructions: u64,
    pub waiting_for_interrupt: bool,
    pub csrs: Rv32CsrInspection,
}

/// Copy-only view of the standard machine timer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rv32TimerInspection {
    pub time: u64,
    pub compare: u64,
    pub pending: bool,
}

/// Copy-only view of one PLIC interrupt source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rv32PlicSourceInspection {
    pub priority: u8,
    pub level: bool,
    pub pending: bool,
    pub in_flight: bool,
    pub enabled: bool,
}

/// Copy-only view of the standard single-context PLIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rv32PlicInspection {
    pub source_count: usize,
    pub sources: [Rv32PlicSourceInspection; RV32_PLIC_MAX_SOURCES],
    pub threshold: u8,
    pub best_eligible_source: u32,
}

impl Default for Rv32PlicInspection {
    fn default() -> Self {
        Self {
            source_count: 0,
            sources: [Rv32PlicSourceInspection::default(); RV32_PLIC_MAX_SOURCES],
            threshold: 0,
            best_eligible_source: 0,
        }
    }
}

/// Copy-only view of one immutable device-to-PLIC route.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rv32IrqRouteInspection {
    pub source: u32,
    pub level: bool,
}

/// Coherent read-only host inspection of one RV32 machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rv32MachineInspection {
    pub hart: Rv32HartInspection,
    pub timer: Rv32TimerInspection,
    pub plic: Rv32PlicInspection,
    pub irq_route_count: usize,
    pub irq_routes: [Rv32IrqRouteInspection; RV32_PLIC_MAX_SOURCES],
    pub control_status: i32,
    pub translation_stats: Option<Rv32TranslationStats>,
    pub dbt_stats: Option<Rv32DbtStats>,
}

impl Default for Rv32MachineInspection {
    fn default() -> Self {
        Self {
            hart: Rv32HartInspection::default(),
            timer: Rv32TimerInspection::default(),
            plic: Rv32PlicInspection::default(),
            irq_route_count: 0,
            irq_routes: [Rv32IrqRouteInspection::default(); RV32_PLIC_MAX_SOURCES],
            control_status: 0,
            translation_stats: None,
            dbt_stats: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_copy_eq<T: Copy + Eq>(_value: T) {}

    #[test]
    fn inspection_values_are_fixed_copyable_values() {
        let hart = Rv32HartInspection::default();
        let csrs = Rv32CsrInspection::default();
        let timer = Rv32TimerInspection::default();
        let plic_source = Rv32PlicSourceInspection::default();
        let plic = Rv32PlicInspection::default();
        let irq_route = Rv32IrqRouteInspection::default();
        let machine = Rv32MachineInspection::default();

        assert_copy_eq(hart);
        assert_copy_eq(csrs);
        assert_copy_eq(timer);
        assert_copy_eq(plic_source);
        assert_copy_eq(plic);
        assert_copy_eq(irq_route);
        assert_copy_eq(machine);
    }
}
