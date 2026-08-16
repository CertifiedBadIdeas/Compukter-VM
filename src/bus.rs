use crate::memory::{AtomicWordAccess, MachineMemory, MemoryBus, MemoryFault};
use std::any::Any;
use std::cell::Cell;

pub type MmioDeviceId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioAccessWidth {
    Byte,
    Halfword,
    Word,
    Doubleword,
}

impl MmioAccessWidth {
    pub const fn bytes(self) -> u32 {
        match self {
            Self::Byte => 1,
            Self::Halfword => 2,
            Self::Word => 4,
            Self::Doubleword => 8,
        }
    }
}

pub struct MmioContext<'a> {
    memory: &'a mut MachineMemory,
}

impl<'a> MmioContext<'a> {
    fn new(memory: &'a mut MachineMemory) -> Self {
        Self { memory }
    }

    pub fn memory(&self) -> &MachineMemory {
        self.memory
    }

    pub fn memory_mut(&mut self) -> &mut MachineMemory {
        self.memory
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MachineBusTrafficSnapshot {
    pub loads: u64,
    pub stores: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmioDeviceTrafficSnapshot {
    pub device_id: MmioDeviceId,
    pub base: u32,
    pub size: u32,
    pub traffic: MachineBusTrafficSnapshot,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineBusStatsSnapshot {
    pub ram: MachineBusTrafficSnapshot,
    pub mmio: MachineBusTrafficSnapshot,
    pub mmio_devices: Vec<MmioDeviceTrafficSnapshot>,
}

#[derive(Default)]
struct MachineBusTrafficCounters {
    loads: Cell<u64>,
    stores: Cell<u64>,
    bytes_read: Cell<u64>,
    bytes_written: Cell<u64>,
}

impl MachineBusTrafficCounters {
    fn record_load(&self, bytes: u64) {
        self.loads.set(self.loads.get().saturating_add(1));
        self.bytes_read
            .set(self.bytes_read.get().saturating_add(bytes));
    }

    fn record_store(&self, bytes: u64) {
        self.stores.set(self.stores.get().saturating_add(1));
        self.bytes_written
            .set(self.bytes_written.get().saturating_add(bytes));
    }

    fn snapshot(&self) -> MachineBusTrafficSnapshot {
        MachineBusTrafficSnapshot {
            loads: self.loads.get(),
            stores: self.stores.get(),
            bytes_read: self.bytes_read.get(),
            bytes_written: self.bytes_written.get(),
        }
    }
}

pub trait MmioDevice: Any {
    fn size(&self) -> u32;

    fn interrupt_level(&self) -> bool {
        false
    }

    fn take_yield_signal(&mut self) -> bool {
        false
    }

    fn read(
        &mut self,
        context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
    ) -> Result<u64, MemoryFault>;

    fn write(
        &mut self,
        context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
        value: u64,
    ) -> Result<(), MemoryFault>;
}

struct MmioRegion {
    base: u32,
    size: u32,
    device: Box<dyn MmioDevice>,
    traffic: MachineBusTrafficCounters,
}

impl MmioRegion {
    fn end(&self) -> Result<u32, MemoryFault> {
        self.base.checked_add(self.size).ok_or_else(|| {
            MemoryFault::new(format!(
                "mmio region {:#010x} with size {} overflows address space",
                self.base, self.size,
            ))
        })
    }

    fn offset_for(&self, address: u32, width: MmioAccessWidth) -> Option<u32> {
        let end = self.end().ok()?;
        let access_end = address.checked_add(width.bytes())?;
        if address >= self.base && access_end <= end {
            Some(address - self.base)
        } else {
            None
        }
    }

    fn overlaps_i32(&self, address: u32) -> bool {
        let Some(region_end) = self.end().ok() else {
            return false;
        };
        let Some(access_end) = address.checked_add(4) else {
            return false;
        };
        address < region_end && self.base < access_end
    }
}

pub struct MachineBus {
    memory: MachineMemory,
    regions: Vec<MmioRegion>,
    mmio_epoch: u64,
    ram_traffic: MachineBusTrafficCounters,
    mmio_traffic: MachineBusTrafficCounters,
}

impl MachineBus {
    pub fn new(memory_size: usize) -> Result<Self, MemoryFault> {
        Ok(Self {
            memory: MachineMemory::zeroed(memory_size)?,
            regions: Vec::new(),
            mmio_epoch: 0,
            ram_traffic: MachineBusTrafficCounters::default(),
            mmio_traffic: MachineBusTrafficCounters::default(),
        })
    }

    pub fn memory(&self) -> &MachineMemory {
        &self.memory
    }

    pub fn memory_mut(&mut self) -> &mut MachineMemory {
        &mut self.memory
    }

    pub fn map_mmio(
        &mut self,
        base: u32,
        device: Box<dyn MmioDevice>,
    ) -> Result<MmioDeviceId, MemoryFault> {
        let size = device.size();
        if size == 0 {
            return Err(MemoryFault::new(
                "mmio device size must be positive".to_string(),
            ));
        }
        let region = MmioRegion {
            base,
            size,
            device,
            traffic: MachineBusTrafficCounters::default(),
        };
        let region_end = region.end()?;
        if u64::from(base) < self.memory.len() as u64 {
            return Err(MemoryFault::new(format!(
                "mmio region {base:#010x}..{region_end:#010x} overlaps RAM 0x00000000..{:#010x}",
                self.memory.len(),
            )));
        }
        for existing in &self.regions {
            let existing_end = existing.end()?;
            if base < existing_end && existing.base < region_end {
                return Err(MemoryFault::new(format!(
                    "mmio region {base:#010x}..{region_end:#010x} overlaps existing region {:#010x}..{existing_end:#010x}",
                    existing.base,
                )));
            }
        }
        let device_id = self.regions.len();
        self.regions.push(region);
        Ok(device_id)
    }

    pub fn device<T: MmioDevice>(&self, id: MmioDeviceId) -> Option<&T> {
        self.regions
            .get(id)
            .and_then(|region| (&*region.device as &dyn Any).downcast_ref::<T>())
    }

    pub fn device_mut<T: MmioDevice>(&mut self, id: MmioDeviceId) -> Option<&mut T> {
        self.regions
            .get_mut(id)
            .and_then(|region| (&mut *region.device as &mut dyn Any).downcast_mut::<T>())
    }

    pub fn mmio_region_bounds(&self, id: MmioDeviceId) -> Option<(u32, u32)> {
        self.regions
            .get(id)
            .map(|region| (region.base, region.size))
    }

    pub fn interrupt_level(&self, id: MmioDeviceId) -> Option<bool> {
        self.regions
            .get(id)
            .map(|region| region.device.interrupt_level())
    }

    pub fn mmio_epoch(&self) -> u64 {
        self.mmio_epoch
    }

    pub fn load_i32(&mut self, address: u32) -> Result<i32, MemoryFault> {
        <Self as MemoryBus>::load_i32(self, address)
    }

    pub fn store_i32(&mut self, address: u32, value: i32) -> Result<(), MemoryFault> {
        <Self as MemoryBus>::store_i32(self, address, value)
    }

    pub fn load_u8(&mut self, address: u32) -> Result<u8, MemoryFault> {
        <Self as MemoryBus>::load_u8(self, address)
    }

    pub fn store_u8(&mut self, address: u32, value: u8) -> Result<(), MemoryFault> {
        <Self as MemoryBus>::store_u8(self, address, value)
    }

    pub fn load_u16(&mut self, address: u32) -> Result<u16, MemoryFault> {
        <Self as MemoryBus>::load_u16(self, address)
    }

    pub fn store_u16(&mut self, address: u32, value: u16) -> Result<(), MemoryFault> {
        <Self as MemoryBus>::store_u16(self, address, value)
    }

    pub fn load_u64(&mut self, address: u32) -> Result<u64, MemoryFault> {
        <Self as MemoryBus>::load_u64(self, address)
    }

    pub fn store_u64(&mut self, address: u32, value: u64) -> Result<(), MemoryFault> {
        <Self as MemoryBus>::store_u64(self, address, value)
    }

    pub fn stats_snapshot(&self) -> MachineBusStatsSnapshot {
        MachineBusStatsSnapshot {
            ram: self.ram_traffic.snapshot(),
            mmio: self.mmio_traffic.snapshot(),
            mmio_devices: self
                .regions
                .iter()
                .enumerate()
                .map(|(device_id, region)| MmioDeviceTrafficSnapshot {
                    device_id,
                    base: region.base,
                    size: region.size,
                    traffic: region.traffic.snapshot(),
                })
                .collect(),
        }
    }

    /// Returns aggregate RAM/MMIO counters without allocating per-device detail.
    pub fn aggregate_traffic_snapshot(
        &self,
    ) -> (MachineBusTrafficSnapshot, MachineBusTrafficSnapshot) {
        (self.ram_traffic.snapshot(), self.mmio_traffic.snapshot())
    }
}

impl MemoryBus for MachineBus {
    fn len(&self) -> usize {
        self.memory.len()
    }

    fn take_yield_signal(&mut self) -> bool {
        self.regions
            .iter_mut()
            .any(|region| region.device.take_yield_signal())
    }

    fn load_i32(&mut self, address: u32) -> Result<i32, MemoryFault> {
        for region in &mut self.regions {
            if let Some(offset) = region.offset_for(address, MmioAccessWidth::Word) {
                let mut context = MmioContext::new(&mut self.memory);
                let value = region
                    .device
                    .read(&mut context, offset, MmioAccessWidth::Word)?;
                region.traffic.record_load(4);
                self.mmio_traffic.record_load(4);
                self.mmio_epoch = self.mmio_epoch.wrapping_add(1);
                return Ok(value as u32 as i32);
            }
        }
        let value = self.memory.load_i32(address)?;
        self.ram_traffic.record_load(4);
        Ok(value)
    }

    fn store_i32(&mut self, address: u32, value: i32) -> Result<(), MemoryFault> {
        for region in &mut self.regions {
            if let Some(offset) = region.offset_for(address, MmioAccessWidth::Word) {
                let mut context = MmioContext::new(&mut self.memory);
                region.device.write(
                    &mut context,
                    offset,
                    MmioAccessWidth::Word,
                    u64::from(value as u32),
                )?;
                region.traffic.record_store(4);
                self.mmio_traffic.record_store(4);
                self.mmio_epoch = self.mmio_epoch.wrapping_add(1);
                return Ok(());
            }
        }
        self.memory.store_i32(address, value)?;
        self.ram_traffic.record_store(4);
        Ok(())
    }

    fn validate_atomic_i32(
        &self,
        address: u32,
        _access: AtomicWordAccess,
    ) -> Result<(), MemoryFault> {
        if self
            .regions
            .iter()
            .any(|region| region.overlaps_i32(address))
        {
            return Err(MemoryFault::at(
                address,
                format!("atomic word access to MMIO at {address:#010x} is unsupported"),
            ));
        }
        self.memory.validate_atomic_i32(address)
    }

    fn atomic_load_i32(&mut self, address: u32) -> Result<i32, MemoryFault> {
        self.validate_atomic_i32(address, AtomicWordAccess::Load)?;
        let value = self.memory.atomic_load_i32(address)?;
        self.ram_traffic.record_load(4);
        Ok(value)
    }

    fn atomic_store_i32(&mut self, address: u32, value: i32) -> Result<(), MemoryFault> {
        self.validate_atomic_i32(address, AtomicWordAccess::Store)?;
        self.memory.atomic_store_i32(address, value)?;
        self.ram_traffic.record_store(4);
        Ok(())
    }

    fn atomic_update_i32(
        &mut self,
        address: u32,
        update: &mut dyn FnMut(i32) -> i32,
    ) -> Result<i32, MemoryFault> {
        self.validate_atomic_i32(address, AtomicWordAccess::ReadModifyWrite)?;
        let old = self.memory.atomic_update_i32(address, update)?;
        self.ram_traffic.record_load(4);
        self.ram_traffic.record_store(4);
        Ok(old)
    }

    fn load_u8(&mut self, address: u32) -> Result<u8, MemoryFault> {
        for region in &mut self.regions {
            if let Some(offset) = region.offset_for(address, MmioAccessWidth::Byte) {
                let mut context = MmioContext::new(&mut self.memory);
                let value = region
                    .device
                    .read(&mut context, offset, MmioAccessWidth::Byte)?;
                region.traffic.record_load(1);
                self.mmio_traffic.record_load(1);
                self.mmio_epoch = self.mmio_epoch.wrapping_add(1);
                return Ok(value as u8);
            }
        }
        let value = self.memory.load_u8(address)?;
        self.ram_traffic.record_load(1);
        Ok(value)
    }

    fn store_u8(&mut self, address: u32, value: u8) -> Result<(), MemoryFault> {
        for region in &mut self.regions {
            if let Some(offset) = region.offset_for(address, MmioAccessWidth::Byte) {
                let mut context = MmioContext::new(&mut self.memory);
                region.device.write(
                    &mut context,
                    offset,
                    MmioAccessWidth::Byte,
                    u64::from(value),
                )?;
                region.traffic.record_store(1);
                self.mmio_traffic.record_store(1);
                self.mmio_epoch = self.mmio_epoch.wrapping_add(1);
                return Ok(());
            }
        }
        self.memory.store_u8(address, value)?;
        self.ram_traffic.record_store(1);
        Ok(())
    }

    fn load_u16(&mut self, address: u32) -> Result<u16, MemoryFault> {
        for region in &mut self.regions {
            if let Some(offset) = region.offset_for(address, MmioAccessWidth::Halfword) {
                let mut context = MmioContext::new(&mut self.memory);
                let value = region
                    .device
                    .read(&mut context, offset, MmioAccessWidth::Halfword)?;
                region.traffic.record_load(2);
                self.mmio_traffic.record_load(2);
                self.mmio_epoch = self.mmio_epoch.wrapping_add(1);
                return Ok(value as u16);
            }
        }
        let value = self.memory.load_u16(address)?;
        self.ram_traffic.record_load(2);
        Ok(value)
    }

    fn store_u16(&mut self, address: u32, value: u16) -> Result<(), MemoryFault> {
        for region in &mut self.regions {
            if let Some(offset) = region.offset_for(address, MmioAccessWidth::Halfword) {
                let mut context = MmioContext::new(&mut self.memory);
                region.device.write(
                    &mut context,
                    offset,
                    MmioAccessWidth::Halfword,
                    u64::from(value),
                )?;
                region.traffic.record_store(2);
                self.mmio_traffic.record_store(2);
                self.mmio_epoch = self.mmio_epoch.wrapping_add(1);
                return Ok(());
            }
        }
        self.memory.store_u16(address, value)?;
        self.ram_traffic.record_store(2);
        Ok(())
    }

    fn load_u64(&mut self, address: u32) -> Result<u64, MemoryFault> {
        for region in &mut self.regions {
            if let Some(offset) = region.offset_for(address, MmioAccessWidth::Doubleword) {
                let mut context = MmioContext::new(&mut self.memory);
                let value =
                    region
                        .device
                        .read(&mut context, offset, MmioAccessWidth::Doubleword)?;
                region.traffic.record_load(8);
                self.mmio_traffic.record_load(8);
                self.mmio_epoch = self.mmio_epoch.wrapping_add(1);
                return Ok(value);
            }
        }
        let value = self.memory.load_u64(address)?;
        self.ram_traffic.record_load(8);
        Ok(value)
    }

    fn store_u64(&mut self, address: u32, value: u64) -> Result<(), MemoryFault> {
        for region in &mut self.regions {
            if let Some(offset) = region.offset_for(address, MmioAccessWidth::Doubleword) {
                let mut context = MmioContext::new(&mut self.memory);
                region
                    .device
                    .write(&mut context, offset, MmioAccessWidth::Doubleword, value)?;
                region.traffic.record_store(8);
                self.mmio_traffic.record_store(8);
                self.mmio_epoch = self.mmio_epoch.wrapping_add(1);
                return Ok(());
            }
        }
        self.memory.store_u64(address, value)?;
        self.ram_traffic.record_store(8);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::{MachineBus, MmioAccessWidth, MmioContext, MmioDevice};
    use crate::memory::{MemoryBus, MemoryFault};

    struct RegisterDevice {
        value: i32,
        read_only: bool,
    }

    struct InterruptRegister {
        level: bool,
    }

    impl MmioDevice for InterruptRegister {
        fn size(&self) -> u32 {
            4
        }

        fn interrupt_level(&self) -> bool {
            self.level
        }

        fn read(
            &mut self,
            _context: &mut MmioContext<'_>,
            _offset: u32,
            _width: MmioAccessWidth,
        ) -> Result<u64, MemoryFault> {
            Ok(u64::from(self.level))
        }

        fn write(
            &mut self,
            _context: &mut MmioContext<'_>,
            _offset: u32,
            _width: MmioAccessWidth,
            value: u64,
        ) -> Result<(), MemoryFault> {
            self.level = value != 0;
            Ok(())
        }
    }

    impl MmioDevice for RegisterDevice {
        fn size(&self) -> u32 {
            4
        }

        fn read(
            &mut self,
            _context: &mut MmioContext<'_>,
            offset: u32,
            width: MmioAccessWidth,
        ) -> Result<u64, MemoryFault> {
            assert_eq!(offset, 0);
            assert_eq!(width, MmioAccessWidth::Word);
            Ok(u64::from(self.value as u32))
        }

        fn write(
            &mut self,
            _context: &mut MmioContext<'_>,
            offset: u32,
            width: MmioAccessWidth,
            value: u64,
        ) -> Result<(), MemoryFault> {
            assert_eq!(offset, 0);
            assert_eq!(width, MmioAccessWidth::Word);
            if self.read_only {
                return Err(MemoryFault::new("register is read-only".to_string()));
            }
            self.value = value as u32 as i32;
            Ok(())
        }
    }

    struct ByteWindowDevice {
        bytes: Vec<u8>,
    }

    struct DmaDevice {
        destination: u32,
    }

    impl MmioDevice for DmaDevice {
        fn size(&self) -> u32 {
            4
        }

        fn read(
            &mut self,
            context: &mut MmioContext<'_>,
            offset: u32,
            width: MmioAccessWidth,
        ) -> Result<u64, MemoryFault> {
            assert_eq!((offset, width), (0, MmioAccessWidth::Word));
            Ok(u64::from(
                context.memory().load_i32(self.destination)? as u32
            ))
        }

        fn write(
            &mut self,
            context: &mut MmioContext<'_>,
            offset: u32,
            width: MmioAccessWidth,
            value: u64,
        ) -> Result<(), MemoryFault> {
            assert_eq!((offset, width), (0, MmioAccessWidth::Word));
            context
                .memory_mut()
                .store_i32(self.destination, value as u32 as i32)
        }
    }

    struct CountingDevice {
        accesses: Vec<MmioAccessWidth>,
    }

    impl MmioDevice for CountingDevice {
        fn size(&self) -> u32 {
            8
        }

        fn read(
            &mut self,
            _context: &mut MmioContext<'_>,
            offset: u32,
            width: MmioAccessWidth,
        ) -> Result<u64, MemoryFault> {
            self.accesses.push(width);
            Ok(match width {
                MmioAccessWidth::Byte => u64::from(offset + 1),
                MmioAccessWidth::Halfword => 0x0201,
                MmioAccessWidth::Word => 0x0403_0201,
                MmioAccessWidth::Doubleword => 0x0807_0605_0403_0201,
            })
        }

        fn write(
            &mut self,
            _context: &mut MmioContext<'_>,
            _offset: u32,
            width: MmioAccessWidth,
            _value: u64,
        ) -> Result<(), MemoryFault> {
            self.accesses.push(width);
            Ok(())
        }
    }

    impl MmioDevice for ByteWindowDevice {
        fn size(&self) -> u32 {
            self.bytes.len() as u32
        }

        fn read(
            &mut self,
            _context: &mut MmioContext<'_>,
            offset: u32,
            width: MmioAccessWidth,
        ) -> Result<u64, MemoryFault> {
            let offset = offset as usize;
            let width = width.bytes() as usize;
            let bytes = self
                .bytes
                .get(offset..offset + width)
                .ok_or_else(|| MemoryFault::new(format!("invalid read at offset {offset}")))?;
            let mut value = [0_u8; 8];
            value[..width].copy_from_slice(bytes);
            Ok(u64::from_le_bytes(value))
        }

        fn write(
            &mut self,
            _context: &mut MmioContext<'_>,
            offset: u32,
            width: MmioAccessWidth,
            value: u64,
        ) -> Result<(), MemoryFault> {
            let offset = offset as usize;
            let width = width.bytes() as usize;
            let bytes = self
                .bytes
                .get_mut(offset..offset + width)
                .ok_or_else(|| MemoryFault::new(format!("invalid write at offset {offset}")))?;
            bytes.copy_from_slice(&value.to_le_bytes()[..width]);
            Ok(())
        }
    }

    #[test]
    fn machine_bus_delivers_each_width_as_one_mutable_device_access() {
        let mut bus = MachineBus::new(16).unwrap();
        let device_id = bus
            .map_mmio(
                0x1000,
                Box::new(CountingDevice {
                    accesses: Vec::new(),
                }),
            )
            .unwrap();

        assert_eq!(bus.load_u8(0x1000).unwrap(), 0x01);
        assert_eq!(bus.load_u16(0x1000).unwrap(), 0x0201);
        assert_eq!(bus.load_i32(0x1000).unwrap(), 0x0403_0201);
        assert_eq!(bus.load_u64(0x1000).unwrap(), 0x0807_0605_0403_0201);
        bus.store_u8(0x1000, 1).unwrap();
        bus.store_u16(0x1000, 2).unwrap();
        bus.store_i32(0x1000, 3).unwrap();
        bus.store_u64(0x1000, 4).unwrap();
        assert_eq!(bus.mmio_epoch(), 8);
        assert_eq!(
            bus.device::<CountingDevice>(device_id)
                .unwrap()
                .accesses
                .as_slice(),
            &[
                MmioAccessWidth::Byte,
                MmioAccessWidth::Halfword,
                MmioAccessWidth::Word,
                MmioAccessWidth::Doubleword,
                MmioAccessWidth::Byte,
                MmioAccessWidth::Halfword,
                MmioAccessWidth::Word,
                MmioAccessWidth::Doubleword,
            ]
        );
    }

    #[test]
    fn mmio_epoch_and_interrupt_level_track_device_boundary_activity() {
        let mut bus = MachineBus::new(16).unwrap();
        let device = bus
            .map_mmio(0x1000, Box::new(InterruptRegister { level: false }))
            .unwrap();

        assert_eq!(bus.mmio_epoch(), 0);
        bus.store_i32(0, 7).unwrap();
        assert_eq!(bus.mmio_epoch(), 0);
        assert_eq!(bus.interrupt_level(device), Some(false));

        assert_eq!(bus.load_i32(0x1000).unwrap(), 0);
        assert_eq!(bus.mmio_epoch(), 1);
        bus.store_i32(0x1000, 1).unwrap();
        assert_eq!(bus.mmio_epoch(), 2);
        assert_eq!(bus.interrupt_level(device), Some(true));
        assert_eq!(bus.interrupt_level(device + 1), None);
    }

    #[test]
    fn machine_bus_gives_devices_bounded_ram_access() {
        let mut bus = MachineBus::new(16).unwrap();
        bus.map_mmio(0x1000, Box::new(DmaDevice { destination: 4 }))
            .unwrap();

        bus.store_i32(0x1000, 0x1122_3344).unwrap();

        assert_eq!(bus.memory().load_i32(4).unwrap(), 0x1122_3344);
        assert_eq!(bus.load_i32(0x1000).unwrap(), 0x1122_3344);
    }

    #[test]
    fn machine_bus_rejects_mmio_that_overlaps_ram() {
        let mut bus = MachineBus::new(16).unwrap();

        let error = bus
            .map_mmio(
                8,
                Box::new(RegisterDevice {
                    value: 7,
                    read_only: false,
                }),
            )
            .unwrap_err();

        assert!(error.to_string().contains("overlaps RAM"));
    }

    #[test]
    fn machine_bus_routes_regular_addresses_to_ram() {
        let mut bus = MachineBus::new(16).unwrap();

        bus.store_i32(4, 0x11223344).unwrap();

        assert_eq!(bus.load_i32(4).unwrap(), 0x11223344);
        assert_eq!(&bus.memory().bytes()[4..8], &[0x44, 0x33, 0x22, 0x11]);
    }

    #[test]
    fn machine_bus_routes_low_ram_addresses_even_when_high_mmio_is_mapped() {
        let mut bus = MachineBus::new(16).unwrap();
        bus.map_mmio(
            0x1000_0000,
            Box::new(RegisterDevice {
                value: 7,
                read_only: false,
            }),
        )
        .unwrap();

        bus.store_i32(4, 0x55667788).unwrap();

        assert_eq!(bus.load_i32(4).unwrap(), 0x55667788);
    }

    #[test]
    fn machine_bus_routes_mmio_addresses_to_registered_devices() {
        let mut bus = MachineBus::new(16).unwrap();
        let device_id = bus
            .map_mmio(
                0x1000_0000,
                Box::new(RegisterDevice {
                    value: 7,
                    read_only: false,
                }),
            )
            .unwrap();

        assert_eq!(bus.load_i32(0x1000_0000).unwrap(), 7);
        bus.store_i32(0x1000_0000, 9).unwrap();

        let device = bus.device::<RegisterDevice>(device_id).unwrap();
        assert_eq!(device.value, 9);
    }

    #[test]
    fn machine_bus_rejects_atomic_mmio_before_device_access() {
        let mut bus = MachineBus::new(16).unwrap();
        let device_id = bus
            .map_mmio(
                0x1000,
                Box::new(RegisterDevice {
                    value: 7,
                    read_only: false,
                }),
            )
            .unwrap();
        let mut increment = |old: i32| old.wrapping_add(1);

        let error = bus.atomic_update_i32(0x1000, &mut increment).unwrap_err();

        assert_eq!(error.address(), Some(0x1000));
        assert_eq!(bus.device::<RegisterDevice>(device_id).unwrap().value, 7);
        assert_eq!(bus.stats_snapshot().mmio.loads, 0);
        assert_eq!(bus.stats_snapshot().mmio.stores, 0);
    }

    #[test]
    fn machine_bus_atomic_update_counts_one_ram_load_and_store() {
        let mut bus = MachineBus::new(16).unwrap();
        bus.memory_mut().store_i32(4, 7).unwrap();
        let mut increment = |old: i32| old.wrapping_add(1);

        assert_eq!(bus.atomic_update_i32(4, &mut increment).unwrap(), 7);

        let stats = bus.stats_snapshot();
        assert_eq!(stats.ram.loads, 1);
        assert_eq!(stats.ram.stores, 1);
        assert_eq!(stats.ram.bytes_read, 4);
        assert_eq!(stats.ram.bytes_written, 4);
        assert_eq!(bus.memory().load_i32(4).unwrap(), 8);
    }

    #[test]
    fn machine_bus_stats_snapshot_counts_ram_and_mmio_traffic() {
        let mut bus = MachineBus::new(16).unwrap();
        let device_id = bus
            .map_mmio(
                0x1000,
                Box::new(RegisterDevice {
                    value: 7,
                    read_only: false,
                }),
            )
            .unwrap();

        bus.store_u8(0, 1).unwrap();
        assert_eq!(bus.load_u16(0).unwrap(), 1);
        bus.store_i32(0x1000, 11).unwrap();
        assert_eq!(bus.load_i32(0x1000).unwrap(), 11);

        let stats = bus.stats_snapshot();

        assert_eq!(stats.ram.loads, 1);
        assert_eq!(stats.ram.stores, 1);
        assert_eq!(stats.ram.bytes_read, 2);
        assert_eq!(stats.ram.bytes_written, 1);
        assert_eq!(stats.mmio.loads, 1);
        assert_eq!(stats.mmio.stores, 1);
        assert_eq!(stats.mmio.bytes_read, 4);
        assert_eq!(stats.mmio.bytes_written, 4);
        assert_eq!(stats.mmio_devices.len(), 1);
        assert_eq!(stats.mmio_devices[0].device_id, device_id);
        assert_eq!(stats.mmio_devices[0].base, 0x1000);
        assert_eq!(stats.mmio_devices[0].size, 4);
        assert_eq!(stats.mmio_devices[0].traffic, stats.mmio);
    }

    #[test]
    fn machine_bus_stats_snapshot_counts_u64_as_single_bus_access() {
        let mut bus = MachineBus::new(32).unwrap();
        bus.map_mmio(
            0x1000,
            Box::new(ByteWindowDevice {
                bytes: vec![0_u8; 8],
            }),
        )
        .unwrap();

        bus.store_u64(0, 0x0102_0304_0506_0708).unwrap();
        assert_eq!(bus.load_u64(0).unwrap(), 0x0102_0304_0506_0708);
        bus.store_u64(0x1000, 0x1112_1314_1516_1718).unwrap();
        assert_eq!(bus.load_u64(0x1000).unwrap(), 0x1112_1314_1516_1718);

        let stats = bus.stats_snapshot();

        assert_eq!(stats.ram.loads, 1);
        assert_eq!(stats.ram.stores, 1);
        assert_eq!(stats.ram.bytes_read, 8);
        assert_eq!(stats.ram.bytes_written, 8);
        assert_eq!(stats.mmio.loads, 1);
        assert_eq!(stats.mmio.stores, 1);
        assert_eq!(stats.mmio.bytes_read, 8);
        assert_eq!(stats.mmio.bytes_written, 8);
        assert_eq!(stats.mmio_devices[0].traffic, stats.mmio);
    }

    #[test]
    fn machine_bus_preserves_device_faults() {
        let mut bus = MachineBus::new(16).unwrap();
        bus.map_mmio(
            0x1000_0100,
            Box::new(RegisterDevice {
                value: 11,
                read_only: true,
            }),
        )
        .unwrap();

        let error = bus.store_i32(0x1000_0100, 3).unwrap_err();

        assert_eq!(error.to_string(), "register is read-only");
    }
}
