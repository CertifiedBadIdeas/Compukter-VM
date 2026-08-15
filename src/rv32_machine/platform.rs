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

use crate::bus::{MmioAccessWidth, MmioContext, MmioDevice};
use crate::memory::MemoryFault;

pub const CONTROL_BASE: u32 = 0x1000_0000;
pub const DEBUG_BASE: u32 = 0x1000_0100;
pub const MMIO_PAGE_SIZE: u32 = 256;
pub const STATUS_RESET: i32 = 0;
pub const STATUS_BOOTING: i32 = 1;
pub const STATUS_HALTED: i32 = 3;
pub const STATUS_PANIC: i32 = 4;

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
