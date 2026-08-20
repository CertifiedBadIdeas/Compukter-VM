use compukter_vm::memory::{MachineMemory, MemoryFault};

use super::MAX_QUEUE_SIZE;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SplitVirtqueue {
    size: u16,
    ready: bool,
    descriptor_address: u64,
    available_address: u64,
    used_address: u64,
    pub(crate) last_available_index: u16,
}

impl SplitVirtqueue {
    pub(crate) fn size(&self) -> u16 {
        self.size
    }

    pub(crate) fn ready(&self) -> bool {
        self.ready
    }

    pub(crate) fn descriptor_address(&self) -> u64 {
        self.descriptor_address
    }

    pub(crate) fn available_address(&self) -> u64 {
        self.available_address
    }

    pub(crate) fn used_address(&self) -> u64 {
        self.used_address
    }

    pub(crate) fn set_size(&mut self, size: u32) {
        if !self.ready {
            self.size = u16::try_from(size).unwrap_or(0);
        }
    }

    pub(crate) fn set_descriptor_low(&mut self, value: u32) {
        if !self.ready {
            set_low(&mut self.descriptor_address, value);
        }
    }

    pub(crate) fn set_descriptor_high(&mut self, value: u32) {
        if !self.ready {
            set_high(&mut self.descriptor_address, value);
        }
    }

    pub(crate) fn set_available_low(&mut self, value: u32) {
        if !self.ready {
            set_low(&mut self.available_address, value);
        }
    }

    pub(crate) fn set_available_high(&mut self, value: u32) {
        if !self.ready {
            set_high(&mut self.available_address, value);
        }
    }

    pub(crate) fn set_used_low(&mut self, value: u32) {
        if !self.ready {
            set_low(&mut self.used_address, value);
        }
    }

    pub(crate) fn set_used_high(&mut self, value: u32) {
        if !self.ready {
            set_high(&mut self.used_address, value);
        }
    }

    pub(crate) fn try_enable(&mut self, memory: &MachineMemory) {
        if !self.ready && self.validate(memory) {
            self.ready = true;
            self.last_available_index = 0;
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn available_index(&self, memory: &MachineMemory) -> Result<u16, MemoryFault> {
        memory.load_u16(guest_address(self.available_address, 2)?)
    }

    pub(crate) fn available_head(
        &self,
        memory: &MachineMemory,
        available_index: u16,
    ) -> Result<u16, MemoryFault> {
        let slot = available_index & (self.size - 1);
        memory.load_u16(guest_address(
            self.available_address,
            4 + u64::from(slot) * 2,
        )?)
    }

    pub(crate) fn publish_used(
        &self,
        memory: &mut MachineMemory,
        head: u16,
        written: u32,
    ) -> Result<(), MemoryFault> {
        let used_index_address = guest_address(self.used_address, 2)?;
        let used_index = memory.load_u16(used_index_address)?;
        let slot = used_index & (self.size - 1);
        let element = guest_address(self.used_address, 4 + u64::from(slot) * 8)?;
        memory.store_i32(element, i32::from(head))?;
        memory.store_i32(element + 4, written as i32)?;
        memory.store_u16(used_index_address, used_index.wrapping_add(1))
    }

    fn validate(&self, memory: &MachineMemory) -> bool {
        if self.size == 0 || self.size > MAX_QUEUE_SIZE || !self.size.is_power_of_two() {
            return false;
        }

        let size = u64::from(self.size);
        let Some(descriptor) =
            GuestRange::new(self.descriptor_address, 16 * size, 16, memory.len())
        else {
            return false;
        };
        let Some(available) =
            GuestRange::new(self.available_address, 4 + 2 * size, 2, memory.len())
        else {
            return false;
        };
        let Some(used) = GuestRange::new(self.used_address, 4 + 8 * size, 4, memory.len()) else {
            return false;
        };

        !descriptor.overlaps(available) && !descriptor.overlaps(used) && !available.overlaps(used)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GuestRange {
    pub(crate) start: u64,
    pub(crate) end: u64,
}

impl GuestRange {
    pub(crate) fn new(address: u64, size: u64, alignment: u64, memory_len: usize) -> Option<Self> {
        if !address.is_multiple_of(alignment) || address > u64::from(u32::MAX) {
            return None;
        }
        let end = address.checked_add(size)?;
        if end > memory_len as u64 || end > u64::from(u32::MAX) + 1 {
            return None;
        }
        Some(Self {
            start: address,
            end,
        })
    }

    pub(crate) fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

fn set_low(address: &mut u64, value: u32) {
    *address = (*address & (u64::from(u32::MAX) << 32)) | u64::from(value);
}

fn set_high(address: &mut u64, value: u32) {
    *address = (*address & u64::from(u32::MAX)) | (u64::from(value) << 32);
}

fn guest_address(base: u64, offset: u64) -> Result<u32, MemoryFault> {
    base.checked_add(offset)
        .and_then(|address| u32::try_from(address).ok())
        .ok_or_else(|| MemoryFault::new("VirtIO queue address is outside RV32".to_string()))
}
