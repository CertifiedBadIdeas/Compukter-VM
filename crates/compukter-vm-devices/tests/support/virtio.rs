use compukter_vm::bus::MmioContext;
use compukter_vm_devices::virtio::{VirtioDescriptorChain, VirtioDevice, VirtioDeviceError};

pub const VIRTIO_BASE: u32 = 0x1000_2000;
pub const TEST_DEVICE_ID: u32 = 0xffff_ff00;
pub const DESCRIPTOR_ADDRESS: u32 = 0x4000;
pub const AVAILABLE_ADDRESS: u32 = 0x4100;
pub const USED_ADDRESS: u32 = 0x4200;
pub const INPUT_ADDRESS: u32 = 0x4300;
pub const OUTPUT_ADDRESS: u32 = 0x4400;
pub const QUEUE_SIZE: u16 = 8;

pub struct EchoDevice;

impl VirtioDevice for EchoDevice {
    fn device_id(&self) -> u32 {
        TEST_DEVICE_ID
    }

    fn reset(&mut self) {}

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
        let written = input.length.min(output.length);
        if written > 64 {
            return Err(VirtioDeviceError::InvalidRequest);
        }
        let mut bytes = [0_u8; 64];
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

pub fn queue_image(input: &[u8]) -> Vec<u8> {
    let mut image = vec![0_u8; 0x500];
    put_u64(&mut image, 0x000, u64::from(INPUT_ADDRESS));
    put_u32(&mut image, 0x008, input.len() as u32);
    put_u16(&mut image, 0x00c, 1);
    put_u16(&mut image, 0x00e, 1);
    put_u64(&mut image, 0x010, u64::from(OUTPUT_ADDRESS));
    put_u32(&mut image, 0x018, input.len() as u32);
    put_u16(&mut image, 0x01c, 2);
    put_u16(&mut image, 0x104, 0);
    put_u16(&mut image, 0x102, 1);
    image[0x300..0x300 + input.len()].copy_from_slice(input);
    image
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
