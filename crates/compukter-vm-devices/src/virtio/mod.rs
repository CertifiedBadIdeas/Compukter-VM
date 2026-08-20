#[allow(
    dead_code,
    reason = "descriptor parsing is connected to MMIO notifications in the next slice"
)]
mod descriptor;
mod device;
mod mmio;
mod queue;

pub use device::{
    VirtioDescriptor, VirtioDescriptorChain, VirtioDevice, VirtioDeviceError, MAX_QUEUE_SIZE,
};
pub use mmio::{VirtioMmioDevice, VirtioTransportError, VIRTIO_MMIO_REGION_SIZE};
