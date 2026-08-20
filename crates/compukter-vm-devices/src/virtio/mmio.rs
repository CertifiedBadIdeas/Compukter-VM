use super::VirtioDevice;
use compukter_vm::bus::{MmioAccessWidth, MmioContext, MmioDevice};
use compukter_vm::memory::MemoryFault;
use std::error::Error;
use std::fmt::{Display, Formatter};

pub const VIRTIO_MMIO_REGION_SIZE: u32 = 0x200;

const MAGIC_VALUE: u32 = 0x7472_6976;
const VERSION: u32 = 2;
const VENDOR_ID: u32 = u32::from_le_bytes(*b"COMP");
const CONFIG_SPACE_OFFSET: u32 = 0x100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioTransportError {
    ReservedDeviceId,
}

impl Display for VirtioTransportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReservedDeviceId => formatter.write_str("VirtIO device id zero is reserved"),
        }
    }
}

impl Error for VirtioTransportError {}

pub struct VirtioMmioDevice<D: VirtioDevice> {
    device: D,
}

impl<D: VirtioDevice> VirtioMmioDevice<D> {
    pub fn new(device: D) -> Result<Self, VirtioTransportError> {
        if device.device_id() == 0 {
            return Err(VirtioTransportError::ReservedDeviceId);
        }
        Ok(Self { device })
    }

    pub fn device(&self) -> &D {
        &self.device
    }

    pub fn device_mut(&mut self) -> &mut D {
        &mut self.device
    }
}

impl<D: VirtioDevice> MmioDevice for VirtioMmioDevice<D> {
    fn size(&self) -> u32 {
        VIRTIO_MMIO_REGION_SIZE
    }

    fn read(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
    ) -> Result<u64, MemoryFault> {
        if offset >= CONFIG_SPACE_OFFSET {
            return self
                .device
                .read_config(offset - CONFIG_SPACE_OFFSET, width)
                .map_err(device_fault);
        }
        require_header_word(offset, width)?;
        Ok(u64::from(match offset {
            0x000 => MAGIC_VALUE,
            0x004 => VERSION,
            0x008 => self.device.device_id(),
            0x00c => VENDOR_ID,
            _ => 0,
        }))
    }

    fn write(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
        value: u64,
    ) -> Result<(), MemoryFault> {
        if offset >= CONFIG_SPACE_OFFSET {
            return self
                .device
                .write_config(offset - CONFIG_SPACE_OFFSET, width, value)
                .map_err(device_fault);
        }
        require_header_word(offset, width)
    }
}

fn require_header_word(offset: u32, width: MmioAccessWidth) -> Result<(), MemoryFault> {
    if width == MmioAccessWidth::Word && offset.is_multiple_of(4) {
        Ok(())
    } else {
        Err(MemoryFault::at(
            offset,
            format!("VirtIO-MMIO header requires aligned word access at offset {offset:#x}"),
        ))
    }
}

fn device_fault(error: super::VirtioDeviceError) -> MemoryFault {
    MemoryFault::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtio::{VirtioDescriptorChain, VirtioDevice, VirtioDeviceError};
    use compukter_vm::bus::{MachineBus, MmioContext};

    struct IdentityDevice;

    impl VirtioDevice for IdentityDevice {
        fn device_id(&self) -> u32 {
            0xffff_ff00
        }

        fn reset(&mut self) {}

        fn process_chain(
            &mut self,
            _context: &mut MmioContext<'_>,
            _chain: VirtioDescriptorChain<'_>,
        ) -> Result<u32, VirtioDeviceError> {
            Ok(0)
        }
    }

    #[test]
    fn modern_transport_exposes_standard_identity_registers() {
        let mut bus = MachineBus::new(0x1000).unwrap();
        bus.map_mmio(
            0x2000,
            Box::new(VirtioMmioDevice::new(IdentityDevice).unwrap()),
        )
        .unwrap();

        assert_eq!(bus.load_i32(0x2000).unwrap() as u32, 0x7472_6976);
        assert_eq!(bus.load_i32(0x2004).unwrap() as u32, 2);
        assert_eq!(bus.load_i32(0x2008).unwrap() as u32, 0xffff_ff00);
        assert_eq!(
            bus.load_i32(0x200c).unwrap() as u32,
            u32::from_le_bytes(*b"COMP")
        );
    }

    #[test]
    fn transport_header_requires_aligned_word_accesses() {
        let mut bus = MachineBus::new(0x1000).unwrap();
        bus.map_mmio(
            0x2000,
            Box::new(VirtioMmioDevice::new(IdentityDevice).unwrap()),
        )
        .unwrap();

        assert!(bus.load_u8(0x2000).is_err());
        assert!(bus.store_u16(0x2070, 0).is_err());
    }
}
