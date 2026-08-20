use super::{VirtioDescriptorChain, VirtioDevice, VirtioDeviceError};
use compukter_vm::bus::{MmioAccessWidth, MmioContext};
use std::error::Error;
use std::fmt::{Display, Formatter};

pub const VIRTIO_BLOCK_DEVICE_ID: u32 = 2;
pub const VIRTIO_BLOCK_SECTOR_SIZE: usize = 512;

const VIRTIO_BLK_F_RO: u64 = 1 << 5;
const VIRTIO_BLK_F_BLK_SIZE: u64 = 1 << 6;
const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
const CONFIG_SIZE: usize = 24;
const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;
const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioBlockError {
    InvalidCapacity,
    CapacityTooLarge,
    AllocationFailed,
}

impl Display for VirtioBlockError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCapacity => formatter
                .write_str("VirtIO block capacity must be a positive multiple of 512 bytes"),
            Self::CapacityTooLarge => {
                formatter.write_str("VirtIO block capacity does not fit host address space")
            }
            Self::AllocationFailed => {
                formatter.write_str("could not allocate VirtIO block backing storage")
            }
        }
    }
}

impl Error for VirtioBlockError {}

pub struct VirtioBlockDevice {
    storage: Box<[u8]>,
    read_only: bool,
}

impl VirtioBlockDevice {
    pub fn from_bytes(bytes: Vec<u8>, read_only: bool) -> Result<Self, VirtioBlockError> {
        if bytes.is_empty() || !bytes.len().is_multiple_of(VIRTIO_BLOCK_SECTOR_SIZE) {
            return Err(VirtioBlockError::InvalidCapacity);
        }
        Ok(Self {
            storage: bytes.into_boxed_slice(),
            read_only,
        })
    }

    pub fn zeroed(sectors: u64, read_only: bool) -> Result<Self, VirtioBlockError> {
        if sectors == 0 {
            return Err(VirtioBlockError::InvalidCapacity);
        }
        let byte_count = sectors
            .checked_mul(VIRTIO_BLOCK_SECTOR_SIZE as u64)
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or(VirtioBlockError::CapacityTooLarge)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_count)
            .map_err(|_| VirtioBlockError::AllocationFailed)?;
        bytes.resize(byte_count, 0);
        Self::from_bytes(bytes, read_only)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.storage
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.storage
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    fn config(&self) -> [u8; CONFIG_SIZE] {
        let mut config = [0_u8; CONFIG_SIZE];
        let sectors = (self.storage.len() / VIRTIO_BLOCK_SECTOR_SIZE) as u64;
        config[0..8].copy_from_slice(&sectors.to_le_bytes());
        config[20..24].copy_from_slice(&(VIRTIO_BLOCK_SECTOR_SIZE as u32).to_le_bytes());
        config
    }
}

impl VirtioDevice for VirtioBlockDevice {
    fn device_id(&self) -> u32 {
        VIRTIO_BLOCK_DEVICE_ID
    }

    fn features(&self) -> u64 {
        VIRTIO_BLK_F_BLK_SIZE
            | VIRTIO_BLK_F_FLUSH
            | if self.read_only { VIRTIO_BLK_F_RO } else { 0 }
    }

    fn reset(&mut self) {}

    fn read_config(&self, offset: u32, width: MmioAccessWidth) -> Result<u64, VirtioDeviceError> {
        let width = width.bytes() as usize;
        let offset = offset as usize;
        let end = offset
            .checked_add(width)
            .ok_or(VirtioDeviceError::InvalidRequest)?;
        if offset % width != 0 || end > CONFIG_SIZE {
            return Err(VirtioDeviceError::InvalidRequest);
        }
        let config = self.config();
        let bytes = &config[offset..end];
        Ok(match width {
            1 => u64::from(bytes[0]),
            2 => u64::from(u16::from_le_bytes(bytes.try_into().unwrap())),
            4 => u64::from(u32::from_le_bytes(bytes.try_into().unwrap())),
            8 => u64::from_le_bytes(bytes.try_into().unwrap()),
            _ => return Err(VirtioDeviceError::InvalidRequest),
        })
    }

    fn process_chain(
        &mut self,
        context: &mut MmioContext<'_>,
        chain: VirtioDescriptorChain<'_>,
    ) -> Result<u32, VirtioDeviceError> {
        let descriptors = chain.descriptors();
        if descriptors.len() < 2 {
            return Err(VirtioDeviceError::InvalidRequest);
        }
        let header = descriptors[0];
        let status = descriptors[descriptors.len() - 1];
        if header.writable || header.length < 16 || !status.writable || status.length < 1 {
            return Err(VirtioDeviceError::InvalidRequest);
        }

        let mut request = [0_u8; 16];
        context
            .memory()
            .read_bytes(header.address, &mut request)
            .map_err(|_| VirtioDeviceError::InvalidRequest)?;
        let request_type = u32::from_le_bytes(request[0..4].try_into().unwrap());
        let sector = u64::from_le_bytes(request[8..16].try_into().unwrap());
        let data = &descriptors[1..descriptors.len() - 1];

        match request_type {
            VIRTIO_BLK_T_IN => self.read_request(context, data, status.address, sector),
            VIRTIO_BLK_T_OUT => self.write_request(context, data, status.address, sector),
            VIRTIO_BLK_T_FLUSH if data.is_empty() => {
                write_status(context, status.address, VIRTIO_BLK_S_OK)?;
                Ok(1)
            }
            VIRTIO_BLK_T_FLUSH => Err(VirtioDeviceError::InvalidRequest),
            _ => {
                write_status(context, status.address, VIRTIO_BLK_S_UNSUPP)?;
                Ok(1)
            }
        }
    }
}

impl VirtioBlockDevice {
    fn read_request(
        &self,
        context: &mut MmioContext<'_>,
        data: &[super::VirtioDescriptor],
        status_address: u32,
        sector: u64,
    ) -> Result<u32, VirtioDeviceError> {
        if data.is_empty() || data.iter().any(|descriptor| !descriptor.writable) {
            return Err(VirtioDeviceError::InvalidRequest);
        }
        let Some((start, length)) = self.data_range(data, sector) else {
            write_status(context, status_address, VIRTIO_BLK_S_IOERR)?;
            return Ok(1);
        };

        let mut storage_offset = start;
        for descriptor in data {
            let end = storage_offset + descriptor.length as usize;
            context
                .memory_mut()
                .write_bytes(descriptor.address, &self.storage[storage_offset..end])
                .map_err(|_| VirtioDeviceError::InvalidRequest)?;
            storage_offset = end;
        }
        write_status(context, status_address, VIRTIO_BLK_S_OK)?;
        u32::try_from(length + 1).map_err(|_| VirtioDeviceError::InvalidRequest)
    }

    fn write_request(
        &mut self,
        context: &mut MmioContext<'_>,
        data: &[super::VirtioDescriptor],
        status_address: u32,
        sector: u64,
    ) -> Result<u32, VirtioDeviceError> {
        if data.is_empty() || data.iter().any(|descriptor| descriptor.writable) {
            return Err(VirtioDeviceError::InvalidRequest);
        }
        let Some((start, _length)) = self.data_range(data, sector) else {
            write_status(context, status_address, VIRTIO_BLK_S_IOERR)?;
            return Ok(1);
        };
        if self.read_only {
            write_status(context, status_address, VIRTIO_BLK_S_IOERR)?;
            return Ok(1);
        }

        let mut storage_offset = start;
        for descriptor in data {
            let end = storage_offset + descriptor.length as usize;
            context
                .memory()
                .read_bytes(descriptor.address, &mut self.storage[storage_offset..end])
                .map_err(|_| VirtioDeviceError::InvalidRequest)?;
            storage_offset = end;
        }
        write_status(context, status_address, VIRTIO_BLK_S_OK)?;
        Ok(1)
    }

    fn data_range(&self, data: &[super::VirtioDescriptor], sector: u64) -> Option<(usize, usize)> {
        let length = data.iter().try_fold(0_usize, |total, descriptor| {
            total.checked_add(descriptor.length as usize)
        })?;
        if length == 0 || !length.is_multiple_of(VIRTIO_BLOCK_SECTOR_SIZE) {
            return None;
        }
        let start = sector
            .checked_mul(VIRTIO_BLOCK_SECTOR_SIZE as u64)
            .and_then(|value| usize::try_from(value).ok())?;
        let end = start.checked_add(length)?;
        (end <= self.storage.len()).then_some((start, length))
    }
}

fn write_status(
    context: &mut MmioContext<'_>,
    address: u32,
    status: u8,
) -> Result<(), VirtioDeviceError> {
    context
        .memory_mut()
        .store_u8(address, status)
        .map_err(|_| VirtioDeviceError::InvalidRequest)
}
