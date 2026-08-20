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
const VIRTIO_F_VERSION_1: u64 = 1 << 32;

pub const STATUS_ACKNOWLEDGE: u32 = 1;
pub const STATUS_DRIVER: u32 = 2;
pub const STATUS_DRIVER_OK: u32 = 4;
pub const STATUS_FEATURES_OK: u32 = 8;
pub const STATUS_DEVICE_NEEDS_RESET: u32 = 64;
pub const STATUS_FAILED: u32 = 128;

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
    device_features_selector: u32,
    driver_features_selector: u32,
    driver_features: u64,
    status: u32,
    interrupt_status: u32,
    config_generation: u32,
}

impl<D: VirtioDevice> VirtioMmioDevice<D> {
    pub fn new(device: D) -> Result<Self, VirtioTransportError> {
        if device.device_id() == 0 {
            return Err(VirtioTransportError::ReservedDeviceId);
        }
        Ok(Self {
            device,
            device_features_selector: 0,
            driver_features_selector: 0,
            driver_features: 0,
            status: 0,
            interrupt_status: 0,
            config_generation: 0,
        })
    }

    pub fn device(&self) -> &D {
        &self.device
    }

    pub fn device_mut(&mut self) -> &mut D {
        &mut self.device
    }

    pub fn driver_features(&self) -> u64 {
        self.driver_features
    }

    pub fn interrupt_status(&self) -> u32 {
        self.interrupt_status
    }

    pub fn driver_ready(&self) -> bool {
        const READY: u32 =
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK;
        self.status & READY == READY
            && self.status & (STATUS_DEVICE_NEEDS_RESET | STATUS_FAILED) == 0
    }

    fn offered_features(&self) -> u64 {
        self.device.features() | VIRTIO_F_VERSION_1
    }

    fn reset(&mut self) {
        self.device_features_selector = 0;
        self.driver_features_selector = 0;
        self.driver_features = 0;
        self.status = 0;
        self.interrupt_status = 0;
        self.config_generation = 0;
        self.device.reset();
    }

    fn write_status(&mut self, value: u32) {
        if value == 0 {
            self.reset();
            return;
        }
        if value & self.status != self.status {
            return;
        }

        self.status = value;
        if value & STATUS_FEATURES_OK != 0
            && (self.driver_features & !self.offered_features() != 0
                || self.driver_features & VIRTIO_F_VERSION_1 == 0)
        {
            self.status &= !STATUS_FEATURES_OK;
        }
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
            0x010 => feature_bank(self.offered_features(), self.device_features_selector),
            0x060 => self.interrupt_status,
            0x070 => self.status,
            0x0fc => self.config_generation,
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
        require_header_word(offset, width)?;
        let value = value as u32;
        match offset {
            0x014 => self.device_features_selector = value,
            0x020 if self.status & STATUS_FEATURES_OK == 0 => {
                write_feature_bank(
                    &mut self.driver_features,
                    self.driver_features_selector,
                    value,
                );
            }
            0x024 if self.status & STATUS_FEATURES_OK == 0 => {
                self.driver_features_selector = value;
            }
            0x070 => self.write_status(value),
            _ => {}
        }
        Ok(())
    }
}

fn feature_bank(features: u64, selector: u32) -> u32 {
    match selector {
        0 => features as u32,
        1 => (features >> 32) as u32,
        _ => 0,
    }
}

fn write_feature_bank(features: &mut u64, selector: u32, value: u32) {
    match selector {
        0 => *features = (*features & !u64::from(u32::MAX)) | u64::from(value),
        1 => *features = (*features & u64::from(u32::MAX)) | (u64::from(value) << 32),
        _ => {}
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
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    const MMIO_BASE: u32 = 0x2000;

    struct IdentityDevice {
        features: u64,
        reset_count: Arc<AtomicUsize>,
    }

    impl IdentityDevice {
        fn new(features: u64) -> (Self, Arc<AtomicUsize>) {
            let reset_count = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    features,
                    reset_count: Arc::clone(&reset_count),
                },
                reset_count,
            )
        }
    }

    impl VirtioDevice for IdentityDevice {
        fn device_id(&self) -> u32 {
            0xffff_ff00
        }

        fn features(&self) -> u64 {
            self.features
        }

        fn reset(&mut self) {
            self.reset_count.fetch_add(1, Ordering::Relaxed);
        }

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
        let (device, _) = IdentityDevice::new(0);
        bus.map_mmio(MMIO_BASE, Box::new(VirtioMmioDevice::new(device).unwrap()))
            .unwrap();

        assert_eq!(bus.load_i32(MMIO_BASE).unwrap() as u32, 0x7472_6976);
        assert_eq!(bus.load_i32(MMIO_BASE + 0x004).unwrap() as u32, 2);
        assert_eq!(bus.load_i32(MMIO_BASE + 0x008).unwrap() as u32, 0xffff_ff00);
        assert_eq!(
            bus.load_i32(MMIO_BASE + 0x00c).unwrap() as u32,
            u32::from_le_bytes(*b"COMP")
        );
    }

    #[test]
    fn transport_header_requires_aligned_word_accesses() {
        let mut bus = MachineBus::new(0x1000).unwrap();
        let (device, _) = IdentityDevice::new(0);
        bus.map_mmio(MMIO_BASE, Box::new(VirtioMmioDevice::new(device).unwrap()))
            .unwrap();

        assert!(bus.load_u8(MMIO_BASE).is_err());
        assert!(bus.store_u16(MMIO_BASE + 0x070, 0).is_err());
    }

    #[test]
    fn feature_banks_offer_device_features_and_required_modern_bit() {
        let mut bus = MachineBus::new(0x1000).unwrap();
        let (device, _) = IdentityDevice::new((1 << 5) | (1_u64 << 47));
        bus.map_mmio(MMIO_BASE, Box::new(VirtioMmioDevice::new(device).unwrap()))
            .unwrap();

        assert_eq!(bus.load_i32(MMIO_BASE + 0x010).unwrap() as u32, 1 << 5);
        bus.store_i32(MMIO_BASE + 0x014, 1).unwrap();
        assert_eq!(
            bus.load_i32(MMIO_BASE + 0x010).unwrap() as u32,
            (1 << 15) | 1
        );
        bus.store_i32(MMIO_BASE + 0x014, 2).unwrap();
        assert_eq!(bus.load_i32(MMIO_BASE + 0x010).unwrap(), 0);
    }

    #[test]
    fn supported_modern_features_make_the_driver_ready() {
        let mut bus = MachineBus::new(0x1000).unwrap();
        let (device, _) = IdentityDevice::new(1 << 5);
        bus.map_mmio(MMIO_BASE, Box::new(VirtioMmioDevice::new(device).unwrap()))
            .unwrap();

        bus.store_i32(MMIO_BASE + 0x020, 1 << 5).unwrap();
        bus.store_i32(MMIO_BASE + 0x024, 1).unwrap();
        bus.store_i32(MMIO_BASE + 0x020, 1).unwrap();
        bus.store_i32(MMIO_BASE + 0x070, 1 | 2 | 8).unwrap();
        assert_eq!(bus.load_i32(MMIO_BASE + 0x070).unwrap(), 1 | 2 | 8);

        bus.store_i32(MMIO_BASE + 0x070, 1 | 2 | 8 | 4).unwrap();
        assert_eq!(bus.load_i32(MMIO_BASE + 0x070).unwrap(), 1 | 2 | 8 | 4);
    }

    #[test]
    fn features_ok_is_cleared_for_unsupported_or_legacy_negotiation() {
        for (low, high) in [(1 << 6, 1), (1 << 5, 0)] {
            let mut bus = MachineBus::new(0x1000).unwrap();
            let (device, _) = IdentityDevice::new(1 << 5);
            bus.map_mmio(MMIO_BASE, Box::new(VirtioMmioDevice::new(device).unwrap()))
                .unwrap();

            bus.store_i32(MMIO_BASE + 0x020, low).unwrap();
            bus.store_i32(MMIO_BASE + 0x024, 1).unwrap();
            bus.store_i32(MMIO_BASE + 0x020, high).unwrap();
            bus.store_i32(MMIO_BASE + 0x070, 1 | 2 | 8).unwrap();

            assert_eq!(bus.load_i32(MMIO_BASE + 0x070).unwrap(), 1 | 2);
        }
    }

    #[test]
    fn zero_status_resets_transport_and_device_state() {
        let mut bus = MachineBus::new(0x1000).unwrap();
        let (device, reset_count) = IdentityDevice::new(1 << 5);
        bus.map_mmio(MMIO_BASE, Box::new(VirtioMmioDevice::new(device).unwrap()))
            .unwrap();

        bus.store_i32(MMIO_BASE + 0x020, 1 << 5).unwrap();
        bus.store_i32(MMIO_BASE + 0x014, 1).unwrap();
        bus.store_i32(MMIO_BASE + 0x070, 1 | 2).unwrap();
        bus.store_i32(MMIO_BASE + 0x070, 0).unwrap();

        assert_eq!(bus.load_i32(MMIO_BASE + 0x070).unwrap(), 0);
        assert_eq!(bus.load_i32(MMIO_BASE + 0x010).unwrap(), 1 << 5);
        assert_eq!(reset_count.load(Ordering::Relaxed), 1);
    }
}
