use super::{VirtioDescriptor, VirtioDescriptorChain, VirtioDevice, VirtioDeviceError};
use compukter_vm::bus::{MmioAccessWidth, MmioContext};
use compukter_vm::memory::MachineMemory;
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
        if !offset.is_multiple_of(width) || end > CONFIG_SIZE {
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
        let (readable, writable) = split_directional_streams(chain.descriptors())?;
        let readable_length = stream_length(readable)?;
        let writable_length = stream_length(writable)?;
        if readable_length < 16 || writable_length < 1 {
            return Err(VirtioDeviceError::InvalidRequest);
        }

        let mut request = [0_u8; 16];
        read_stream(context.memory(), readable, 0, &mut request)?;
        let request_type = u32::from_le_bytes(request[0..4].try_into().unwrap());
        let sector = u64::from_le_bytes(request[8..16].try_into().unwrap());

        match request_type {
            VIRTIO_BLK_T_IN if readable_length == 16 => {
                self.read_request(context, writable, writable_length, sector)
            }
            VIRTIO_BLK_T_IN => Err(VirtioDeviceError::InvalidRequest),
            VIRTIO_BLK_T_OUT => {
                self.write_request(context, readable, writable, readable_length, sector)
            }
            VIRTIO_BLK_T_FLUSH if readable_length == 16 => {
                write_stream_byte(context.memory_mut(), writable, 0, VIRTIO_BLK_S_OK)?;
                Ok(1)
            }
            VIRTIO_BLK_T_FLUSH => Err(VirtioDeviceError::InvalidRequest),
            _ => {
                write_stream_byte(
                    context.memory_mut(),
                    writable,
                    writable_length - 1,
                    VIRTIO_BLK_S_UNSUPP,
                )?;
                Ok(1)
            }
        }
    }
}

impl VirtioBlockDevice {
    fn read_request(
        &self,
        context: &mut MmioContext<'_>,
        writable: &[VirtioDescriptor],
        writable_length: usize,
        sector: u64,
    ) -> Result<u32, VirtioDeviceError> {
        let length = writable_length - 1;
        let used_length = length
            .checked_add(1)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(VirtioDeviceError::InvalidRequest)?;
        let Some(start) = self.data_start(length, sector) else {
            write_stream_byte(context.memory_mut(), writable, length, VIRTIO_BLK_S_IOERR)?;
            return Ok(1);
        };
        let end = start + length;
        write_stream(context.memory_mut(), writable, 0, &self.storage[start..end])?;
        write_stream_byte(context.memory_mut(), writable, length, VIRTIO_BLK_S_OK)?;
        Ok(used_length)
    }

    fn write_request(
        &mut self,
        context: &mut MmioContext<'_>,
        readable: &[VirtioDescriptor],
        writable: &[VirtioDescriptor],
        readable_length: usize,
        sector: u64,
    ) -> Result<u32, VirtioDeviceError> {
        let length = readable_length - 16;
        let Some(start) = self.data_start(length, sector) else {
            write_stream_byte(context.memory_mut(), writable, 0, VIRTIO_BLK_S_IOERR)?;
            return Ok(1);
        };
        if self.read_only {
            write_stream_byte(context.memory_mut(), writable, 0, VIRTIO_BLK_S_IOERR)?;
            return Ok(1);
        }

        let end = start + length;
        read_stream(
            context.memory(),
            readable,
            16,
            &mut self.storage[start..end],
        )?;
        write_stream_byte(context.memory_mut(), writable, 0, VIRTIO_BLK_S_OK)?;
        Ok(1)
    }

    fn data_start(&self, length: usize, sector: u64) -> Option<usize> {
        if length == 0 || !length.is_multiple_of(VIRTIO_BLOCK_SECTOR_SIZE) {
            return None;
        }
        let start = sector
            .checked_mul(VIRTIO_BLOCK_SECTOR_SIZE as u64)
            .and_then(|value| usize::try_from(value).ok())?;
        let end = start.checked_add(length)?;
        (end <= self.storage.len()).then_some(start)
    }
}

fn split_directional_streams(
    descriptors: &[VirtioDescriptor],
) -> Result<(&[VirtioDescriptor], &[VirtioDescriptor]), VirtioDeviceError> {
    let first_writable = descriptors
        .iter()
        .position(|descriptor| descriptor.writable)
        .ok_or(VirtioDeviceError::InvalidRequest)?;
    let (readable, writable) = descriptors.split_at(first_writable);
    if readable.is_empty() || writable.iter().any(|descriptor| !descriptor.writable) {
        return Err(VirtioDeviceError::InvalidRequest);
    }
    Ok((readable, writable))
}

fn stream_length(descriptors: &[VirtioDescriptor]) -> Result<usize, VirtioDeviceError> {
    descriptors.iter().try_fold(0_usize, |length, descriptor| {
        length
            .checked_add(descriptor.length as usize)
            .ok_or(VirtioDeviceError::InvalidRequest)
    })
}

fn read_stream(
    memory: &MachineMemory,
    descriptors: &[VirtioDescriptor],
    skip: usize,
    output: &mut [u8],
) -> Result<(), VirtioDeviceError> {
    let mut skip = skip;
    let mut written = 0;
    for descriptor in descriptors {
        let descriptor_length = descriptor.length as usize;
        if skip >= descriptor_length {
            skip -= descriptor_length;
            continue;
        }
        let offset = skip;
        skip = 0;
        let count = (descriptor_length - offset).min(output.len() - written);
        let address = descriptor_address(*descriptor, offset)?;
        memory
            .read_bytes(address, &mut output[written..written + count])
            .map_err(|_| VirtioDeviceError::InvalidRequest)?;
        written += count;
        if written == output.len() {
            return Ok(());
        }
    }
    Err(VirtioDeviceError::InvalidRequest)
}

fn write_stream(
    memory: &mut MachineMemory,
    descriptors: &[VirtioDescriptor],
    skip: usize,
    input: &[u8],
) -> Result<(), VirtioDeviceError> {
    let mut skip = skip;
    let mut read = 0;
    for descriptor in descriptors {
        let descriptor_length = descriptor.length as usize;
        if skip >= descriptor_length {
            skip -= descriptor_length;
            continue;
        }
        let offset = skip;
        skip = 0;
        let count = (descriptor_length - offset).min(input.len() - read);
        let address = descriptor_address(*descriptor, offset)?;
        memory
            .write_bytes(address, &input[read..read + count])
            .map_err(|_| VirtioDeviceError::InvalidRequest)?;
        read += count;
        if read == input.len() {
            return Ok(());
        }
    }
    Err(VirtioDeviceError::InvalidRequest)
}

fn write_stream_byte(
    memory: &mut MachineMemory,
    descriptors: &[VirtioDescriptor],
    offset: usize,
    value: u8,
) -> Result<(), VirtioDeviceError> {
    let address = stream_address(descriptors, offset)?;
    memory
        .store_u8(address, value)
        .map_err(|_| VirtioDeviceError::InvalidRequest)
}

fn stream_address(
    descriptors: &[VirtioDescriptor],
    mut offset: usize,
) -> Result<u32, VirtioDeviceError> {
    for descriptor in descriptors {
        let length = descriptor.length as usize;
        if offset < length {
            return descriptor_address(*descriptor, offset);
        }
        offset -= length;
    }
    Err(VirtioDeviceError::InvalidRequest)
}

fn descriptor_address(
    descriptor: VirtioDescriptor,
    offset: usize,
) -> Result<u32, VirtioDeviceError> {
    let offset = u32::try_from(offset).map_err(|_| VirtioDeviceError::InvalidRequest)?;
    descriptor
        .address
        .checked_add(offset)
        .ok_or(VirtioDeviceError::InvalidRequest)
}
