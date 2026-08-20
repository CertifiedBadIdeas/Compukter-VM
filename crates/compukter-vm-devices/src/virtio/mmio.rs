use super::{
    descriptor::{parse_chain, DescriptorScratch},
    queue::SplitVirtqueue,
    VirtioDevice, MAX_QUEUE_SIZE,
};
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
    queue_selector: u32,
    queue: SplitVirtqueue,
    descriptor_scratch: DescriptorScratch,
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
            queue_selector: 0,
            queue: SplitVirtqueue::default(),
            descriptor_scratch: DescriptorScratch::default(),
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

    pub fn interrupt_pending(&self) -> bool {
        self.interrupt_status != 0
    }

    pub fn driver_ready(&self) -> bool {
        const READY: u32 =
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK;
        self.status & READY == READY
            && self.status & (STATUS_DEVICE_NEEDS_RESET | STATUS_FAILED) == 0
    }

    pub fn queue_ready(&self) -> bool {
        self.queue.ready()
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
        self.queue_selector = 0;
        self.queue.reset();
        self.descriptor_scratch = DescriptorScratch::default();
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

    fn process_queue(&mut self, context: &mut MmioContext<'_>) {
        let available_index = match self.queue.available_index(context.memory()) {
            Ok(index) => index,
            Err(_) => {
                self.needs_reset();
                return;
            }
        };
        let pending = available_index.wrapping_sub(self.queue.last_available_index);
        if pending > self.queue.size() {
            self.needs_reset();
            return;
        }

        for _ in 0..pending {
            let current = self.queue.last_available_index;
            let head = match self.queue.available_head(context.memory(), current) {
                Ok(head) => head,
                Err(_) => {
                    self.needs_reset();
                    return;
                }
            };
            let chain = match parse_chain(
                context.memory(),
                &self.queue,
                head,
                &mut self.descriptor_scratch,
            ) {
                Ok(chain) => chain,
                Err(_) => {
                    self.needs_reset();
                    return;
                }
            };
            let written = match self.device.process_chain(context, chain) {
                Ok(written) => written,
                Err(_) => {
                    self.needs_reset();
                    return;
                }
            };
            if self
                .queue
                .publish_used(context.memory_mut(), head, written)
                .is_err()
            {
                self.needs_reset();
                return;
            }
            self.queue.last_available_index = current.wrapping_add(1);
            self.interrupt_status |= 1;
        }
    }

    fn needs_reset(&mut self) {
        self.status |= STATUS_DEVICE_NEEDS_RESET;
        self.interrupt_status |= 2;
    }
}

impl<D: VirtioDevice> MmioDevice for VirtioMmioDevice<D> {
    fn size(&self) -> u32 {
        VIRTIO_MMIO_REGION_SIZE
    }

    fn interrupt_level(&self) -> bool {
        self.interrupt_pending()
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
            0x034 if self.queue_selector == 0 => u32::from(MAX_QUEUE_SIZE),
            0x044 if self.queue_selector == 0 => u32::from(self.queue.ready()),
            0x060 => self.interrupt_status,
            0x070 => self.status,
            0x0fc => self.config_generation,
            _ => 0,
        }))
    }

    fn write(
        &mut self,
        context: &mut MmioContext<'_>,
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
            0x030 => self.queue_selector = value,
            0x038 if self.queue_selector == 0 => self.queue.set_size(value),
            0x044
                if self.queue_selector == 0
                    && value == 1
                    && self.status & STATUS_FEATURES_OK != 0 =>
            {
                self.queue.try_enable(context.memory());
            }
            0x050 if value == 0 && self.driver_ready() && self.queue.ready() => {
                self.process_queue(context);
            }
            0x064 => self.interrupt_status &= !value,
            0x080 if self.queue_selector == 0 => self.queue.set_descriptor_low(value),
            0x084 if self.queue_selector == 0 => self.queue.set_descriptor_high(value),
            0x090 if self.queue_selector == 0 => self.queue.set_available_low(value),
            0x094 if self.queue_selector == 0 => self.queue.set_available_high(value),
            0x0a0 if self.queue_selector == 0 => self.queue.set_used_low(value),
            0x0a4 if self.queue_selector == 0 => self.queue.set_used_high(value),
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
    const QUEUE_MMIO_BASE: u32 = 0x1_0000;

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
            context: &mut MmioContext<'_>,
            chain: VirtioDescriptorChain<'_>,
        ) -> Result<u32, VirtioDeviceError> {
            let [input, output] = chain.descriptors() else {
                return Err(VirtioDeviceError::InvalidRequest);
            };
            if input.writable || !output.writable {
                return Err(VirtioDeviceError::InvalidRequest);
            }
            let written = input.length.min(output.length).min(128);
            let mut bytes = [0_u8; 128];
            context
                .memory()
                .read_bytes(input.address, &mut bytes[..written as usize])
                .map_err(|_| VirtioDeviceError::InvalidRequest)?;
            context
                .memory_mut()
                .write_bytes(output.address, &bytes[..written as usize])
                .map_err(|_| VirtioDeviceError::InvalidRequest)?;
            Ok(written)
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

    fn mapped_queue_transport() -> (MachineBus, usize) {
        let mut bus = MachineBus::new(0x1_0000).unwrap();
        let (device, _) = IdentityDevice::new(0);
        let id = bus
            .map_mmio(
                QUEUE_MMIO_BASE,
                Box::new(VirtioMmioDevice::new(device).unwrap()),
            )
            .unwrap();
        bus.store_i32(QUEUE_MMIO_BASE + 0x024, 1).unwrap();
        bus.store_i32(QUEUE_MMIO_BASE + 0x020, 1).unwrap();
        bus.store_i32(QUEUE_MMIO_BASE + 0x070, 1 | 2 | 8).unwrap();
        (bus, id)
    }

    fn write_address(bus: &mut MachineBus, offset: u32, address: u64) {
        bus.store_i32(QUEUE_MMIO_BASE + offset, address as i32)
            .unwrap();
        bus.store_i32(QUEUE_MMIO_BASE + offset + 4, (address >> 32) as i32)
            .unwrap();
    }

    fn configure_queue(
        bus: &mut MachineBus,
        size: u32,
        descriptor: u64,
        available: u64,
        used: u64,
    ) {
        bus.store_i32(QUEUE_MMIO_BASE + 0x038, size as i32).unwrap();
        write_address(bus, 0x080, descriptor);
        write_address(bus, 0x090, available);
        write_address(bus, 0x0a0, used);
        bus.store_i32(QUEUE_MMIO_BASE + 0x044, 1).unwrap();
    }

    #[test]
    fn queue_zero_accepts_a_bounded_power_of_two_configuration() {
        let (mut bus, id) = mapped_queue_transport();

        assert_eq!(
            bus.load_i32(QUEUE_MMIO_BASE + 0x034).unwrap(),
            i32::from(crate::virtio::MAX_QUEUE_SIZE)
        );
        configure_queue(&mut bus, 64, 0x1000, 0x2000, 0x3000);

        assert!(bus
            .device::<VirtioMmioDevice<IdentityDevice>>(id)
            .unwrap()
            .queue_ready());
    }

    #[test]
    fn invalid_queue_configuration_never_becomes_ready() {
        let invalid = [
            (0, 0x1000, 0x2000, 0x3000),
            (3, 0x1000, 0x2000, 0x3000),
            (256, 0x1000, 0x2000, 0x3000),
            (64, 0x1008, 0x2000, 0x3000),
            (64, 0x1000, 0x2001, 0x3000),
            (64, 0x1000, 0x2000, 0x3002),
            (64, 0x1000, 0x1100, 0x3000),
            (64, 0x1000, 0x2000, 0xff00),
            (64, 0x1_0000_1000, 0x2000, 0x3000),
        ];

        for (size, descriptor, available, used) in invalid {
            let (mut bus, id) = mapped_queue_transport();
            configure_queue(&mut bus, size, descriptor, available, used);
            assert!(
                !bus.device::<VirtioMmioDevice<IdentityDevice>>(id)
                    .unwrap()
                    .queue_ready(),
                "invalid queue became ready: size={size}, desc={descriptor:#x}, avail={available:#x}, used={used:#x}"
            );
        }
    }

    fn configured_echo_queue(driver_ok: bool) -> (MachineBus, usize) {
        let (mut bus, id) = mapped_queue_transport();
        configure_queue(&mut bus, 8, 0x1000, 0x2000, 0x3000);
        if driver_ok {
            bus.store_i32(QUEUE_MMIO_BASE + 0x070, 1 | 2 | 8 | 4)
                .unwrap();
        }
        (bus, id)
    }

    fn write_queue_descriptor(
        bus: &mut MachineBus,
        index: u16,
        address: u64,
        length: u32,
        flags: u16,
        next: u16,
    ) {
        let base = 0x1000 + u32::from(index) * 16;
        bus.memory_mut().store_u64(base, address).unwrap();
        bus.memory_mut().store_i32(base + 8, length as i32).unwrap();
        bus.memory_mut().store_u16(base + 12, flags).unwrap();
        bus.memory_mut().store_u16(base + 14, next).unwrap();
    }

    fn submit_echo_chain(bus: &mut MachineBus, head: u16, input: &[u8]) {
        let input_address = 0x4000 + u32::from(head) * 0x100;
        let output_address = 0x5000 + u32::from(head) * 0x100;
        write_queue_descriptor(
            bus,
            head,
            input_address.into(),
            input.len() as u32,
            1,
            head + 1,
        );
        write_queue_descriptor(
            bus,
            head + 1,
            output_address.into(),
            input.len() as u32,
            2,
            0,
        );
        bus.memory_mut().write_bytes(input_address, input).unwrap();
    }

    #[test]
    fn notify_processes_available_entries_and_raises_used_interrupt() {
        let (mut bus, id) = configured_echo_queue(true);
        submit_echo_chain(&mut bus, 0, b"virtio");
        bus.memory_mut().store_u16(0x2004, 0).unwrap();
        bus.memory_mut().store_u16(0x2002, 1).unwrap();

        bus.store_i32(QUEUE_MMIO_BASE + 0x050, 0).unwrap();

        assert_eq!(bus.memory().load_u16(0x3002).unwrap(), 1);
        assert_eq!(bus.memory().load_i32(0x3004).unwrap(), 0);
        assert_eq!(bus.memory().load_i32(0x3008).unwrap(), 6);
        let mut output = [0_u8; 6];
        bus.memory().read_bytes(0x5000, &mut output).unwrap();
        assert_eq!(&output, b"virtio");
        assert_eq!(bus.load_i32(QUEUE_MMIO_BASE + 0x060).unwrap(), 1);
        assert_eq!(bus.interrupt_level(id), Some(true));

        bus.store_i32(QUEUE_MMIO_BASE + 0x064, 1).unwrap();
        assert_eq!(bus.load_i32(QUEUE_MMIO_BASE + 0x060).unwrap(), 0);
        assert_eq!(bus.interrupt_level(id), Some(false));
    }

    #[test]
    fn notify_before_driver_ok_does_nothing() {
        let (mut bus, id) = configured_echo_queue(false);
        submit_echo_chain(&mut bus, 0, b"early");
        bus.memory_mut().store_u16(0x2004, 0).unwrap();
        bus.memory_mut().store_u16(0x2002, 1).unwrap();

        bus.store_i32(QUEUE_MMIO_BASE + 0x050, 0).unwrap();

        assert_eq!(bus.memory().load_u16(0x3002).unwrap(), 0);
        assert_eq!(bus.interrupt_level(id), Some(false));
    }

    #[test]
    fn excessive_available_delta_requires_reset_and_raises_config_interrupt() {
        let (mut bus, id) = configured_echo_queue(true);
        bus.memory_mut().store_u16(0x2002, 9).unwrap();

        bus.store_i32(QUEUE_MMIO_BASE + 0x050, 0).unwrap();

        assert_ne!(
            bus.load_i32(QUEUE_MMIO_BASE + 0x070).unwrap() as u32 & STATUS_DEVICE_NEEDS_RESET,
            0
        );
        assert_eq!(bus.load_i32(QUEUE_MMIO_BASE + 0x060).unwrap(), 2);
        assert_eq!(bus.interrupt_level(id), Some(true));
    }

    #[test]
    fn malformed_later_entry_preserves_an_earlier_completion() {
        let (mut bus, _) = configured_echo_queue(true);
        submit_echo_chain(&mut bus, 0, b"first");
        write_queue_descriptor(&mut bus, 2, 0x6000, 4, 4, 0);
        bus.memory_mut().store_u16(0x2004, 0).unwrap();
        bus.memory_mut().store_u16(0x2006, 2).unwrap();
        bus.memory_mut().store_u16(0x2002, 2).unwrap();

        bus.store_i32(QUEUE_MMIO_BASE + 0x050, 0).unwrap();

        assert_eq!(bus.memory().load_u16(0x3002).unwrap(), 1);
        assert_eq!(bus.memory().load_i32(0x3004).unwrap(), 0);
        assert_eq!(bus.memory().load_i32(0x3008).unwrap(), 5);
        assert_eq!(bus.load_i32(QUEUE_MMIO_BASE + 0x060).unwrap(), 3);
        assert_ne!(
            bus.load_i32(QUEUE_MMIO_BASE + 0x070).unwrap() as u32 & STATUS_DEVICE_NEEDS_RESET,
            0
        );
    }
}
