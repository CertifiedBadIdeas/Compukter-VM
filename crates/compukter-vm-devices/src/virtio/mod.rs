mod device;
mod mmio;

pub use device::{
    VirtioDescriptor, VirtioDescriptorChain, VirtioDevice, VirtioDeviceError, MAX_QUEUE_SIZE,
};
pub use mmio::{VirtioMmioDevice, VirtioTransportError, VIRTIO_MMIO_REGION_SIZE};
