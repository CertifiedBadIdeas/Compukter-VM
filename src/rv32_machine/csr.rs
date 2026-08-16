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

use super::inspection::Rv32CsrInspection;
use crate::rv32im::CsrOperation;
use thiserror::Error;

pub(super) const CSR_MSTATUS: u16 = 0x300;
pub(super) const CSR_MISA: u16 = 0x301;
pub(super) const CSR_MIE: u16 = 0x304;
pub(super) const CSR_MTVEC: u16 = 0x305;
pub(super) const CSR_MSCRATCH: u16 = 0x340;
pub(super) const CSR_MEPC: u16 = 0x341;
pub(super) const CSR_MCAUSE: u16 = 0x342;
pub(super) const CSR_MTVAL: u16 = 0x343;
pub(super) const CSR_MIP: u16 = 0x344;
pub(super) const CSR_MHARTID: u16 = 0xf14;

pub(super) const MSTATUS_MIE: u32 = 1 << 3;
pub(super) const MSTATUS_MPIE: u32 = 1 << 7;
pub(super) const MSTATUS_MPP_MACHINE: u32 = 3 << 11;
const MSTATUS_WRITABLE: u32 = MSTATUS_MIE | MSTATUS_MPIE;
pub(super) const MIE_MTIE: u32 = 1 << 7;
pub(super) const MIP_MTIP: u32 = 1 << 7;
pub(super) const MIE_MEIE: u32 = 1 << 11;
pub(super) const MIP_MEIP: u32 = 1 << 11;
const MIE_WRITABLE: u32 = MIE_MTIE | MIE_MEIE;

const MISA_MXL_RV32: u32 = 1 << 30;
const MISA_A: u32 = 1 << 0;
const MISA_I: u32 = 1 << 8;
const MISA_M: u32 = 1 << 12;
const MISA_RV32IMA: u32 = MISA_MXL_RV32 | MISA_A | MISA_I | MISA_M;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(super) enum Rv32CsrError {
    #[error("RV32 machine CSR {0:#05x} is absent")]
    Absent(u16),
    #[error("RV32 machine CSR {0:#05x} is read-only")]
    ReadOnly(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Rv32MachineCsrs {
    mstatus: u32,
    mie: u32,
    mtvec: u32,
    mscratch: u32,
    mepc: u32,
    mcause: u32,
    mtval: u32,
    mip: u32,
}

impl Rv32MachineCsrs {
    pub(super) fn new() -> Self {
        Self {
            mstatus: MSTATUS_MPP_MACHINE,
            mie: 0,
            mtvec: 0,
            mscratch: 0,
            mepc: 0,
            mcause: 0,
            mtval: 0,
            mip: 0,
        }
    }

    pub(super) fn inspection(&self) -> Rv32CsrInspection {
        Rv32CsrInspection {
            mstatus: self.mstatus,
            mie: self.mie,
            mip: self.mip,
            mtvec: self.mtvec,
            mscratch: self.mscratch,
            mepc: self.mepc,
            mcause: self.mcause,
            mtval: self.mtval,
        }
    }

    pub(super) fn read(&self, csr: u16) -> Result<u32, Rv32CsrError> {
        match csr {
            CSR_MSTATUS => Ok(self.mstatus),
            CSR_MISA => Ok(MISA_RV32IMA),
            CSR_MIE => Ok(self.mie),
            CSR_MTVEC => Ok(self.mtvec),
            CSR_MSCRATCH => Ok(self.mscratch),
            CSR_MEPC => Ok(self.mepc),
            CSR_MCAUSE => Ok(self.mcause),
            CSR_MTVAL => Ok(self.mtval),
            CSR_MIP => Ok(self.mip),
            CSR_MHARTID => Ok(0),
            _ => Err(Rv32CsrError::Absent(csr)),
        }
    }

    pub(super) fn access(
        &mut self,
        csr: u16,
        operation: CsrOperation,
        source: u32,
        write_requested: bool,
    ) -> Result<u32, Rv32CsrError> {
        let old = self.read(csr)?;
        if !write_requested {
            return Ok(old);
        }
        if matches!(csr, CSR_MISA | CSR_MHARTID) {
            return Err(Rv32CsrError::ReadOnly(csr));
        }
        let value = match operation {
            CsrOperation::Write => source,
            CsrOperation::Set => old | source,
            CsrOperation::Clear => old & !source,
        };
        self.write_mutable(csr, value)?;
        Ok(old)
    }

    #[cfg(test)]
    pub(super) fn write_software(&mut self, csr: u16, value: u32) -> Result<(), Rv32CsrError> {
        self.access(csr, CsrOperation::Write, value, true)
            .map(|_| ())
    }

    pub(super) fn enter_trap(&mut self, pc: u32, cause: u32, value: u32) -> u32 {
        let vector = self.trap_base();
        self.enter_trap_state(pc, cause, value);
        vector
    }

    pub(super) fn enter_machine_interrupt(&mut self, pc: u32, cause: u32) -> u32 {
        let vector = if self.mtvec & 3 == 1 {
            self.trap_base().wrapping_add(4 * cause)
        } else {
            self.trap_base()
        };
        self.enter_trap_state(pc, 1 << 31 | cause, 0);
        vector
    }

    pub(super) fn set_machine_timer_pending(&mut self, pending: bool) {
        if pending {
            self.mip |= MIP_MTIP;
        } else {
            self.mip &= !MIP_MTIP;
        }
    }

    pub(super) fn set_machine_external_pending(&mut self, pending: bool) {
        if pending {
            self.mip |= MIP_MEIP;
        } else {
            self.mip &= !MIP_MEIP;
        }
    }

    pub(super) fn enabled_interrupt_pending(&self) -> bool {
        self.mie & self.mip & MIE_WRITABLE != 0
    }

    pub(super) fn highest_actionable_machine_interrupt(&self) -> Option<u32> {
        if self.mstatus & MSTATUS_MIE == 0 {
            return None;
        }
        if self.mie & self.mip & MIE_MEIE != 0 {
            return Some(11);
        }
        if self.mie & self.mip & MIE_MTIE != 0 {
            return Some(7);
        }
        None
    }

    fn enter_trap_state(&mut self, pc: u32, cause: u32, value: u32) {
        let previous_mie = self.mstatus & MSTATUS_MIE != 0;
        self.mstatus = MSTATUS_MPP_MACHINE;
        if previous_mie {
            self.mstatus |= MSTATUS_MPIE;
        }
        self.mepc = pc & !3;
        self.mcause = cause;
        self.mtval = value;
    }

    fn trap_base(&self) -> u32 {
        self.mtvec & !3
    }

    pub(super) fn return_from_trap(&mut self) -> u32 {
        let previous_mpie = self.mstatus & MSTATUS_MPIE != 0;
        self.mstatus = MSTATUS_MPP_MACHINE | MSTATUS_MPIE;
        if previous_mpie {
            self.mstatus |= MSTATUS_MIE;
        }
        self.mepc
    }

    fn write_mutable(&mut self, csr: u16, value: u32) -> Result<(), Rv32CsrError> {
        match csr {
            CSR_MSTATUS => {
                self.mstatus = value & MSTATUS_WRITABLE | MSTATUS_MPP_MACHINE;
            }
            CSR_MIE => self.mie = value & MIE_WRITABLE,
            CSR_MTVEC => {
                let mode = value & 3;
                self.mtvec = value & !3 | u32::from(mode == 1);
            }
            CSR_MSCRATCH => self.mscratch = value,
            CSR_MEPC => self.mepc = value & !3,
            CSR_MCAUSE => self.mcause = value,
            CSR_MTVAL => self.mtval = value,
            CSR_MIP => {}
            CSR_MISA | CSR_MHARTID => return Err(Rv32CsrError::ReadOnly(csr)),
            _ => return Err(Rv32CsrError::Absent(csr)),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csr_bank_reports_rv32ima_and_canonicalizes_warl_fields() {
        let mut csrs = Rv32MachineCsrs::new();
        assert_eq!(
            csrs.read(CSR_MISA).unwrap(),
            MISA_MXL_RV32 | MISA_A | MISA_I | MISA_M
        );
        assert_eq!(csrs.read(CSR_MHARTID).unwrap(), 0);
        assert_eq!(csrs.read(CSR_MSTATUS).unwrap(), MSTATUS_MPP_MACHINE);

        csrs.write_software(CSR_MTVEC, 0x1237).unwrap();
        assert_eq!(csrs.read(CSR_MTVEC).unwrap(), 0x1234);
        csrs.write_software(CSR_MEPC, 0x2003).unwrap();
        assert_eq!(csrs.read(CSR_MEPC).unwrap(), 0x2000);

        csrs.write_software(CSR_MSTATUS, u32::MAX).unwrap();
        assert_eq!(
            csrs.read(CSR_MSTATUS).unwrap(),
            MSTATUS_MIE | MSTATUS_MPIE | MSTATUS_MPP_MACHINE
        );
    }

    #[test]
    fn csr_bank_applies_write_set_and_clear_atomically() {
        let mut csrs = Rv32MachineCsrs::new();
        assert_eq!(
            csrs.access(CSR_MSCRATCH, CsrOperation::Write, 0b0101, true),
            Ok(0)
        );
        assert_eq!(
            csrs.access(CSR_MSCRATCH, CsrOperation::Set, 0b0010, true),
            Ok(0b0101)
        );
        assert_eq!(csrs.read(CSR_MSCRATCH).unwrap(), 0b0111);
        assert_eq!(
            csrs.access(CSR_MSCRATCH, CsrOperation::Clear, 0b0010, true),
            Ok(0b0111)
        );
        assert_eq!(csrs.read(CSR_MSCRATCH).unwrap(), 0b0101);
    }

    #[test]
    fn csr_bank_distinguishes_suppressed_and_requested_read_only_writes() {
        let mut csrs = Rv32MachineCsrs::new();
        assert_eq!(csrs.access(CSR_MHARTID, CsrOperation::Set, 0, false), Ok(0));
        assert_eq!(
            csrs.access(CSR_MHARTID, CsrOperation::Set, 0, true),
            Err(Rv32CsrError::ReadOnly(CSR_MHARTID))
        );
        assert_eq!(
            csrs.access(0x07ff, CsrOperation::Write, 1, true),
            Err(Rv32CsrError::Absent(0x07ff))
        );
        assert_eq!(csrs.read(CSR_MHARTID).unwrap(), 0);
    }

    #[test]
    fn machine_timer_interrupt_bits_are_masked_and_hardware_owned() {
        let mut csrs = Rv32MachineCsrs::new();
        assert_eq!(csrs.read(CSR_MIE).unwrap(), 0);
        assert_eq!(csrs.read(CSR_MIP).unwrap(), 0);

        csrs.write_software(CSR_MIE, u32::MAX).unwrap();
        assert_eq!(csrs.read(CSR_MIE).unwrap(), MIE_WRITABLE);
        csrs.write_software(CSR_MIP, u32::MAX).unwrap();
        assert_eq!(csrs.read(CSR_MIP).unwrap(), 0);

        csrs.set_machine_timer_pending(true);
        assert_eq!(csrs.read(CSR_MIP).unwrap(), MIP_MTIP);
        assert!(csrs.enabled_interrupt_pending());
        assert_eq!(csrs.highest_actionable_machine_interrupt(), None);

        csrs.write_software(CSR_MSTATUS, MSTATUS_MIE).unwrap();
        assert_eq!(csrs.highest_actionable_machine_interrupt(), Some(7));
        csrs.set_machine_timer_pending(false);
        assert_eq!(csrs.read(CSR_MIP).unwrap(), 0);
    }

    #[test]
    fn machine_timer_interrupt_uses_direct_and_vectored_mtvec_modes() {
        for (mtvec, expected_vector) in [(0x2000, 0x2000), (0x2001, 0x201c)] {
            let mut csrs = Rv32MachineCsrs::new();
            csrs.write_software(CSR_MTVEC, mtvec).unwrap();
            csrs.write_software(CSR_MSTATUS, MSTATUS_MIE).unwrap();
            csrs.write_software(CSR_MIE, MIE_MTIE).unwrap();
            csrs.set_machine_timer_pending(true);

            assert_eq!(csrs.enter_machine_interrupt(0x1234, 7), expected_vector);
            assert_eq!(csrs.read(CSR_MEPC).unwrap(), 0x1234);
            assert_eq!(csrs.read(CSR_MCAUSE).unwrap(), 0x8000_0007);
            assert_eq!(csrs.read(CSR_MTVAL).unwrap(), 0);
            assert_eq!(
                csrs.read(CSR_MSTATUS).unwrap(),
                MSTATUS_MPIE | MSTATUS_MPP_MACHINE
            );
        }
    }

    #[test]
    fn machine_external_interrupt_is_hardware_owned_and_precedes_timer() {
        let mut csrs = Rv32MachineCsrs::new();
        csrs.write_software(CSR_MIE, u32::MAX).unwrap();
        assert_eq!(csrs.read(CSR_MIE).unwrap(), MIE_MEIE | MIE_MTIE);

        csrs.write_software(CSR_MIP, u32::MAX).unwrap();
        assert_eq!(csrs.read(CSR_MIP).unwrap(), 0);
        csrs.set_machine_timer_pending(true);
        csrs.set_machine_external_pending(true);
        assert_eq!(csrs.read(CSR_MIP).unwrap(), MIP_MEIP | MIP_MTIP);
        assert!(csrs.enabled_interrupt_pending());
        assert_eq!(csrs.highest_actionable_machine_interrupt(), None);

        csrs.write_software(CSR_MSTATUS, MSTATUS_MIE).unwrap();
        assert_eq!(csrs.highest_actionable_machine_interrupt(), Some(11));
        csrs.set_machine_external_pending(false);
        assert_eq!(csrs.highest_actionable_machine_interrupt(), Some(7));
    }

    #[test]
    fn machine_external_interrupt_uses_direct_and_vectored_mtvec_modes() {
        for (mtvec, expected_vector) in [(0x2000, 0x2000), (0x2001, 0x202c)] {
            let mut csrs = Rv32MachineCsrs::new();
            csrs.write_software(CSR_MTVEC, mtvec).unwrap();

            assert_eq!(csrs.enter_machine_interrupt(0x1234, 11), expected_vector);
            assert_eq!(csrs.read(CSR_MEPC).unwrap(), 0x1234);
            assert_eq!(csrs.read(CSR_MCAUSE).unwrap(), 0x8000_000b);
            assert_eq!(csrs.read(CSR_MTVAL).unwrap(), 0);
        }
    }
}
