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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioBlockError {
    InvalidCapacity,
    CapacityTooLarge,
    AllocationFailed,
}

impl Display for VirtioBlockError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCapacity => formatter.write_str(
                "VirtIO block capacity must be a positive multiple of 512 bytes",
            ),
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
        _context: &mut MmioContext<'_>,
        _chain: VirtioDescriptorChain<'_>,
    ) -> Result<u32, VirtioDeviceError> {
        Err(VirtioDeviceError::InvalidRequest)
    }
}

