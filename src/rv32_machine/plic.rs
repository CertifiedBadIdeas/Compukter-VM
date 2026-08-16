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

pub const PLIC_BASE: u32 = 0x0c00_0000;
pub(super) const PLIC_SIZE: u32 = 0x0020_1000;
pub(super) const MAX_SOURCES: u32 = 1023;
const MAX_PRIORITY: u32 = 7;
const PRIORITY_BASE: u32 = 0x000000;
const PRIORITY_END: u32 = 0x001000;
const PENDING_BASE: u32 = 0x001000;
const PENDING_END: u32 = 0x001080;
const ENABLE_BASE: u32 = 0x002000;
const ENABLE_END: u32 = 0x002080;
const CONTEXT_BASE: u32 = 0x200000;
const CLAIM_COMPLETE: u32 = CONTEXT_BASE + 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rv32PlicSource(u32);

impl Rv32PlicSource {
    pub fn get(self) -> u32 {
        self.0
    }

    pub(crate) fn new(id: u32) -> Option<Self> {
        (1..=MAX_SOURCES).contains(&id).then_some(Self(id))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Gateway {
    level: bool,
    pending: bool,
    in_flight: bool,
}

pub(super) struct PlicDevice {
    priorities: Vec<u8>,
    gateways: Vec<Gateway>,
    enables: Vec<u32>,
    threshold: u8,
}

impl PlicDevice {
    pub(super) fn new(source_count: usize) -> Self {
        debug_assert!(source_count <= MAX_SOURCES as usize);
        let entries = source_count + 1;
        Self {
            priorities: vec![0; entries],
            gateways: vec![Gateway::default(); entries],
            enables: vec![0; entries.div_ceil(32)],
            threshold: 0,
        }
    }

    pub(super) fn set_source_level(&mut self, source: Rv32PlicSource, level: bool) {
        let Some(gateway) = self.gateways.get_mut(source.get() as usize) else {
            return;
        };
        gateway.level = level;
        if level && !gateway.in_flight {
            gateway.pending = true;
        }
    }

    pub(super) fn machine_notification(&self) -> bool {
        self.best_eligible_source().is_some()
    }

    fn best_eligible_source(&self) -> Option<usize> {
        let mut best = None;
        for source in 1..self.gateways.len() {
            let priority = self.priorities[source];
            if !self.gateways[source].pending
                || !self.source_enabled(source)
                || priority <= self.threshold
            {
                continue;
            }
            match best {
                None => best = Some(source),
                Some(current) if priority > self.priorities[current] => best = Some(source),
                _ => {}
            }
        }
        best
    }

    fn source_enabled(&self, source: usize) -> bool {
        self.enables
            .get(source / 32)
            .is_some_and(|word| word & (1 << (source % 32)) != 0)
    }

    fn claim(&mut self) -> u32 {
        let Some(source) = self.best_eligible_source() else {
            return 0;
        };
        let gateway = &mut self.gateways[source];
        gateway.pending = false;
        gateway.in_flight = true;
        source as u32
    }

    fn complete(&mut self, source: u32) {
        let Some(gateway) = self.gateways.get_mut(source as usize) else {
            return;
        };
        if source == 0 || !gateway.in_flight {
            return;
        }
        gateway.in_flight = false;
        if gateway.level {
            gateway.pending = true;
        }
    }

    fn implemented_word_mask(&self, word_index: usize) -> u32 {
        let first = word_index * 32;
        if first >= self.gateways.len() {
            return 0;
        }
        let remaining = self.gateways.len() - first;
        let mut mask = if remaining >= 32 {
            u32::MAX
        } else {
            (1_u32 << remaining) - 1
        };
        if word_index == 0 {
            mask &= !1;
        }
        mask
    }

    fn pending_word(&self, word_index: usize) -> u32 {
        let first = word_index * 32;
        let mut value = 0;
        for bit in 0..32 {
            let source = first + bit;
            if self
                .gateways
                .get(source)
                .is_some_and(|gateway| gateway.pending)
            {
                value |= 1 << bit;
            }
        }
        value & self.implemented_word_mask(word_index)
    }

    fn read_register(&mut self, offset: u32) -> u32 {
        match offset {
            PRIORITY_BASE..PRIORITY_END => {
                let source = (offset / 4) as usize;
                self.priorities.get(source).copied().unwrap_or(0).into()
            }
            PENDING_BASE..PENDING_END => self.pending_word(((offset - PENDING_BASE) / 4) as usize),
            ENABLE_BASE..ENABLE_END => {
                let word = ((offset - ENABLE_BASE) / 4) as usize;
                self.enables.get(word).copied().unwrap_or(0) & self.implemented_word_mask(word)
            }
            CONTEXT_BASE => u32::from(self.threshold),
            CLAIM_COMPLETE => self.claim(),
            _ => 0,
        }
    }

    fn write_register(&mut self, offset: u32, value: u32) {
        match offset {
            PRIORITY_BASE..PRIORITY_END => {
                let source = (offset / 4) as usize;
                if source != 0 {
                    if let Some(priority) = self.priorities.get_mut(source) {
                        *priority = (value & MAX_PRIORITY) as u8;
                    }
                }
            }
            ENABLE_BASE..ENABLE_END => {
                let word = ((offset - ENABLE_BASE) / 4) as usize;
                let mask = self.implemented_word_mask(word);
                if let Some(enable) = self.enables.get_mut(word) {
                    *enable = value & mask;
                }
            }
            CONTEXT_BASE => self.threshold = (value & MAX_PRIORITY) as u8,
            CLAIM_COMPLETE => self.complete(value),
            _ => {}
        }
    }
}

impl MmioDevice for PlicDevice {
    fn size(&self) -> u32 {
        PLIC_SIZE
    }

    fn read(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
    ) -> Result<u64, MemoryFault> {
        if width != MmioAccessWidth::Word || offset % 4 != 0 {
            return Err(MemoryFault::new(format!(
                "RV32 PLIC requires aligned word reads, got {width:?} at offset {offset:#x}"
            )));
        }
        Ok(u64::from(self.read_register(offset)))
    }

    fn write(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
        value: u64,
    ) -> Result<(), MemoryFault> {
        if width != MmioAccessWidth::Word || offset % 4 != 0 {
            return Err(MemoryFault::new(format!(
                "RV32 PLIC requires aligned word writes, got {width:?} at offset {offset:#x}"
            )));
        }
        self.write_register(offset, value as u32);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::MachineBus;

    #[test]
    fn priority_threshold_and_enable_control_notification() {
        let mut plic = PlicDevice::new(2);
        plic.set_source_level(Rv32PlicSource::new(1).unwrap(), true);
        assert!(!plic.machine_notification());

        plic.write_register(PRIORITY_BASE + 4, 3);
        plic.write_register(ENABLE_BASE, 1 << 1);
        assert!(plic.machine_notification());

        plic.write_register(CONTEXT_BASE, 3);
        assert!(!plic.machine_notification());
    }

    #[test]
    fn claim_prefers_priority_then_low_source_id_and_complete_reasserts_level() {
        let mut plic = PlicDevice::new(3);
        for id in 1..=3 {
            let source = Rv32PlicSource::new(id).unwrap();
            plic.write_register(PRIORITY_BASE + 4 * id, if id == 3 { 2 } else { 3 });
            plic.set_source_level(source, true);
        }
        plic.write_register(ENABLE_BASE, 0b1110);

        assert_eq!(plic.read_register(CLAIM_COMPLETE), 1);
        plic.write_register(CLAIM_COMPLETE, 1);
        assert_eq!(plic.read_register(CLAIM_COMPLETE), 1);

        plic.set_source_level(Rv32PlicSource::new(1).unwrap(), false);
        plic.write_register(CLAIM_COMPLETE, 1);
        assert_eq!(plic.read_register(CLAIM_COMPLETE), 2);
    }

    #[test]
    fn deassertion_preserves_an_already_pending_request() {
        let mut plic = PlicDevice::new(1);
        let source = Rv32PlicSource::new(1).unwrap();
        plic.write_register(PRIORITY_BASE + 4, 1);
        plic.write_register(ENABLE_BASE, 1 << 1);
        plic.set_source_level(source, true);
        plic.set_source_level(source, false);

        assert_eq!(plic.read_register(PENDING_BASE), 1 << 1);
        assert_eq!(plic.read_register(CLAIM_COMPLETE), 1);
    }

    #[test]
    fn reserved_and_unassigned_registers_are_zero_and_ignore_writes() {
        let mut plic = PlicDevice::new(1);
        for offset in [PRIORITY_BASE, PRIORITY_BASE + 8, CONTEXT_BASE + 8] {
            plic.write_register(offset, u32::MAX);
            assert_eq!(plic.read_register(offset), 0);
        }
    }

    #[test]
    fn mmio_accepts_aligned_words_and_rejects_other_accesses() {
        let mut bus = MachineBus::new(16).unwrap();
        bus.map_mmio(PLIC_BASE, Box::new(PlicDevice::new(1)))
            .unwrap();

        bus.store_i32(PLIC_BASE + 4, 7).unwrap();
        assert_eq!(bus.load_i32(PLIC_BASE + 4).unwrap(), 7);
        assert!(bus.load_u8(PLIC_BASE + 4).is_err());
        assert!(bus.load_i32(PLIC_BASE + 2).is_err());
    }
}
