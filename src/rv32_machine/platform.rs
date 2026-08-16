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

use super::inspection::Rv32TimerInspection;
use crate::bus::{MmioAccessWidth, MmioContext, MmioDevice};
use crate::memory::MemoryFault;

pub const CONTROL_BASE: u32 = 0x1000_0000;
pub const DEBUG_BASE: u32 = 0x1000_0100;
pub const TIMER_BASE: u32 = 0x1000_0200;
pub const MMIO_PAGE_SIZE: u32 = 256;
pub const STATUS_RESET: i32 = 0;
pub const STATUS_BOOTING: i32 = 1;
pub const STATUS_HALTED: i32 = 3;
pub const STATUS_PANIC: i32 = 4;

pub(super) struct TimerDevice {
    mtime: u64,
    mtimecmp: u64,
}

impl TimerDevice {
    pub(super) fn new() -> Self {
        Self {
            mtime: 0,
            mtimecmp: u64::MAX,
        }
    }

    pub(super) fn time(&self) -> u64 {
        self.mtime
    }

    pub(super) fn inspection(&self) -> Rv32TimerInspection {
        Rv32TimerInspection {
            time: self.mtime,
            compare: self.mtimecmp,
            pending: self.pending(),
        }
    }

    #[cfg(test)]
    fn compare(&self) -> u64 {
        self.mtimecmp
    }

    #[cfg(test)]
    fn set_time(&mut self, value: u64) {
        self.mtime = value;
    }

    #[cfg(test)]
    fn set_compare(&mut self, value: u64) {
        self.mtimecmp = value;
    }

    pub(super) fn advance(&mut self, delta: u64) {
        self.mtime = self.mtime.wrapping_add(delta);
    }

    pub(super) fn pending(&self) -> bool {
        self.mtime >= self.mtimecmp
    }

    fn read_half(&self, offset: u32) -> Option<u32> {
        match offset {
            0 => Some(self.mtimecmp as u32),
            4 => Some((self.mtimecmp >> 32) as u32),
            8 => Some(self.mtime as u32),
            12 => Some((self.mtime >> 32) as u32),
            _ => None,
        }
    }

    fn write_half(&mut self, offset: u32, value: u32) -> bool {
        match offset {
            0 => self.mtimecmp = self.mtimecmp & 0xffff_ffff_0000_0000 | u64::from(value),
            4 => self.mtimecmp = self.mtimecmp & 0x0000_0000_ffff_ffff | (u64::from(value) << 32),
            8 => self.mtime = self.mtime & 0xffff_ffff_0000_0000 | u64::from(value),
            12 => self.mtime = self.mtime & 0x0000_0000_ffff_ffff | (u64::from(value) << 32),
            _ => return false,
        }
        true
    }
}

impl MmioDevice for TimerDevice {
    fn size(&self) -> u32 {
        MMIO_PAGE_SIZE
    }

    fn read(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
    ) -> Result<u64, MemoryFault> {
        if width != MmioAccessWidth::Word {
            return Err(MemoryFault::new(format!(
                "RV32 timer does not support {width:?} reads"
            )));
        }
        self.read_half(offset)
            .map(u64::from)
            .ok_or_else(|| MemoryFault::new(format!("RV32 timer offset {offset} is not mapped")))
    }

    fn write(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
        value: u64,
    ) -> Result<(), MemoryFault> {
        if width != MmioAccessWidth::Word {
            return Err(MemoryFault::new(format!(
                "RV32 timer does not support {width:?} writes"
            )));
        }
        if self.write_half(offset, value as u32) {
            Ok(())
        } else {
            Err(MemoryFault::new(format!(
                "RV32 timer offset {offset} is not mapped"
            )))
        }
    }
}

pub(super) struct ControlDevice {
    pub status: i32,
    pub panic_code: i32,
    pub exit_code: i32,
}

impl ControlDevice {
    pub(super) fn new() -> Self {
        Self {
            status: STATUS_RESET,
            panic_code: 0,
            exit_code: 0,
        }
    }
}

impl MmioDevice for ControlDevice {
    fn size(&self) -> u32 {
        MMIO_PAGE_SIZE
    }

    fn read(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
    ) -> Result<u64, MemoryFault> {
        if width != MmioAccessWidth::Word {
            return Err(MemoryFault::new(format!(
                "RV32 control does not support {width:?} reads"
            )));
        }
        match offset {
            0 => Ok(u64::from(self.status as u32)),
            4 => Ok(u64::from(self.panic_code as u32)),
            8 => Ok(u64::from(self.exit_code as u32)),
            _ => Err(MemoryFault::new(format!(
                "RV32 control offset {offset} is not mapped"
            ))),
        }
    }

    fn write(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
        value: u64,
    ) -> Result<(), MemoryFault> {
        if width != MmioAccessWidth::Word {
            return Err(MemoryFault::new(format!(
                "RV32 control does not support {width:?} writes"
            )));
        }
        let value = value as u32 as i32;
        match offset {
            0 => self.status = value,
            4 => self.panic_code = value,
            8 => self.exit_code = value,
            _ => {
                return Err(MemoryFault::new(format!(
                    "RV32 control offset {offset} is not mapped"
                )))
            }
        }
        Ok(())
    }
}

pub(super) struct DebugDevice {
    bytes: Vec<u8>,
    limit: usize,
}

impl DebugDevice {
    pub(super) fn with_limit(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit),
            limit,
        }
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn push(&mut self, value: u8) -> Result<(), MemoryFault> {
        if self.bytes.len() == self.limit {
            return Err(MemoryFault::new(format!(
                "RV32 debug output exceeds limit {}",
                self.limit
            )));
        }
        self.bytes.push(value);
        Ok(())
    }
}

impl MmioDevice for DebugDevice {
    fn size(&self) -> u32 {
        MMIO_PAGE_SIZE
    }

    fn read(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
    ) -> Result<u64, MemoryFault> {
        if offset == 0 && matches!(width, MmioAccessWidth::Byte | MmioAccessWidth::Word) {
            Ok(0)
        } else {
            Err(MemoryFault::new(format!(
                "RV32 debug offset {offset} is not mapped"
            )))
        }
    }

    fn write(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
        value: u64,
    ) -> Result<(), MemoryFault> {
        if offset == 0 && matches!(width, MmioAccessWidth::Byte | MmioAccessWidth::Word) {
            self.push(value as u8)
        } else {
            Err(MemoryFault::new(format!(
                "RV32 debug offset {offset} is not mapped"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TimerDevice;
    use crate::bus::MachineBus;

    const BASE: u32 = 0x1000;

    #[test]
    fn timer_exposes_rv32_halves_and_rejects_other_accesses() {
        let mut bus = MachineBus::new(16).unwrap();
        let timer_id = bus.map_mmio(BASE, Box::new(TimerDevice::new())).unwrap();

        assert_eq!(bus.load_i32(BASE).unwrap() as u32, u32::MAX);
        assert_eq!(bus.load_i32(BASE + 4).unwrap() as u32, u32::MAX);
        assert_eq!(bus.load_i32(BASE + 8).unwrap(), 0);
        assert_eq!(bus.load_i32(BASE + 12).unwrap(), 0);

        bus.store_i32(BASE, 0x89ab_cdef_u32 as i32).unwrap();
        bus.store_i32(BASE + 4, 0x0123_4567).unwrap();
        bus.store_i32(BASE + 8, 0x7654_3210).unwrap();
        bus.store_i32(BASE + 12, 0xfedc_ba98_u32 as i32).unwrap();

        let timer = bus.device::<TimerDevice>(timer_id).unwrap();
        assert_eq!(timer.compare(), 0x0123_4567_89ab_cdef);
        assert_eq!(timer.time(), 0xfedc_ba98_7654_3210);
        assert!(bus.load_u8(BASE).is_err());
        assert!(bus.load_i32(BASE + 2).is_err());
        assert!(bus.load_i32(BASE + 16).is_err());
    }

    #[test]
    fn timer_pending_is_level_triggered_and_time_wraps() {
        let mut timer = TimerDevice::new();
        assert!(!timer.pending());
        timer.set_compare(5);
        timer.advance(4);
        assert!(!timer.pending());
        timer.advance(1);
        assert!(timer.pending());
        timer.set_compare(6);
        assert!(!timer.pending());
        timer.set_time(u64::MAX);
        timer.advance(2);
        assert_eq!(timer.time(), 1);
    }

    #[test]
    fn timer_inspection_is_repeatable_and_side_effect_free() {
        let mut timer = TimerDevice::new();
        timer.set_compare(5);
        timer.advance(5);

        let first = timer.inspection();
        let second = timer.inspection();

        assert_eq!(first, second);
        assert_eq!(first.time, 5);
        assert_eq!(first.compare, 5);
        assert!(first.pending);
        assert!(timer.pending());
    }
}
