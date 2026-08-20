use compukter_vm::bus::{MmioAccessWidth, MmioContext};
use std::error::Error;
use std::fmt::{Display, Formatter};

pub const MAX_QUEUE_SIZE: u16 = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioDescriptor {
    pub address: u32,
    pub length: u32,
    pub writable: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct VirtioDescriptorChain<'a> {
    head: u16,
    descriptors: &'a [VirtioDescriptor],
}

impl<'a> VirtioDescriptorChain<'a> {
    pub(crate) fn new(head: u16, descriptors: &'a [VirtioDescriptor]) -> Self {
        Self { head, descriptors }
    }

    pub fn head(self) -> u16 {
        self.head
    }

    pub fn descriptors(self) -> &'a [VirtioDescriptor] {
        self.descriptors
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioDeviceError {
    InvalidRequest,
}

impl Display for VirtioDeviceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest => formatter.write_str("invalid VirtIO device request"),
        }
    }
}

impl Error for VirtioDeviceError {}

pub trait VirtioDevice: Send + 'static {
    fn device_id(&self) -> u32;

    fn features(&self) -> u64 {
        0
    }

    fn reset(&mut self);

    fn read_config(&self, _offset: u32, _width: MmioAccessWidth) -> Result<u64, VirtioDeviceError> {
        Err(VirtioDeviceError::InvalidRequest)
    }

    fn write_config(
        &mut self,
        _offset: u32,
        _width: MmioAccessWidth,
        _value: u64,
    ) -> Result<(), VirtioDeviceError> {
        Err(VirtioDeviceError::InvalidRequest)
    }

    fn process_chain(
        &mut self,
        context: &mut MmioContext<'_>,
        chain: VirtioDescriptorChain<'_>,
    ) -> Result<u32, VirtioDeviceError>;
}
