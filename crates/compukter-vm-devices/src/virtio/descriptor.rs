use compukter_vm::memory::{MachineMemory, MemoryFault};

use super::{
    device::{VirtioDescriptor, VirtioDescriptorChain, MAX_QUEUE_SIZE},
    queue::{GuestRange, SplitVirtqueue},
};

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;
const VIRTQ_DESC_F_INDIRECT: u16 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DescriptorError {
    InvalidChain,
    Memory(MemoryFault),
}

impl From<MemoryFault> for DescriptorError {
    fn from(error: MemoryFault) -> Self {
        Self::Memory(error)
    }
}

pub(crate) struct DescriptorScratch {
    descriptors: [VirtioDescriptor; MAX_QUEUE_SIZE as usize],
    visited: [u64; 2],
    count: usize,
}

impl Default for DescriptorScratch {
    fn default() -> Self {
        Self {
            descriptors: [VirtioDescriptor::default(); MAX_QUEUE_SIZE as usize],
            visited: [0; 2],
            count: 0,
        }
    }
}

impl DescriptorScratch {
    fn reset(&mut self) {
        self.visited = [0; 2];
        self.count = 0;
    }

    #[cfg(test)]
    fn count(&self) -> usize {
        self.count
    }
}

pub(crate) fn parse_chain<'a>(
    memory: &MachineMemory,
    queue: &SplitVirtqueue,
    head: u16,
    scratch: &'a mut DescriptorScratch,
) -> Result<VirtioDescriptorChain<'a>, DescriptorError> {
    scratch.reset();
    if let Err(error) = parse_into(memory, queue, head, scratch) {
        scratch.reset();
        return Err(error);
    }
    Ok(VirtioDescriptorChain::new(
        head,
        &scratch.descriptors[..scratch.count],
    ))
}

fn parse_into(
    memory: &MachineMemory,
    queue: &SplitVirtqueue,
    head: u16,
    scratch: &mut DescriptorScratch,
) -> Result<(), DescriptorError> {
    if !queue.ready() || head >= queue.size() {
        return Err(DescriptorError::InvalidChain);
    }

    let metadata = queue_metadata_ranges(queue, memory.len())?;
    let mut index = head;
    loop {
        if index >= queue.size() || scratch.count >= usize::from(queue.size()) {
            return Err(DescriptorError::InvalidChain);
        }
        let visited_word = usize::from(index / 64);
        let visited_bit = 1_u64 << (index % 64);
        if scratch.visited[visited_word] & visited_bit != 0 {
            return Err(DescriptorError::InvalidChain);
        }
        scratch.visited[visited_word] |= visited_bit;

        let table_offset = u64::from(index) * 16;
        let descriptor_address = queue
            .descriptor_address()
            .checked_add(table_offset)
            .and_then(|address| u32::try_from(address).ok())
            .ok_or(DescriptorError::InvalidChain)?;
        let address = memory.load_u64(descriptor_address)?;
        let length = memory.load_i32(descriptor_address + 8)? as u32;
        let flags = memory.load_u16(descriptor_address + 12)?;
        let next = memory.load_u16(descriptor_address + 14)?;

        if flags & VIRTQ_DESC_F_INDIRECT != 0 {
            return Err(DescriptorError::InvalidChain);
        }
        let buffer = GuestRange::new(address, u64::from(length), 1, memory.len())
            .ok_or(DescriptorError::InvalidChain)?;
        if metadata.iter().any(|range| buffer.overlaps(*range))
            || scratch.descriptors[..scratch.count].iter().any(|previous| {
                let previous = GuestRange {
                    start: u64::from(previous.address),
                    end: u64::from(previous.address) + u64::from(previous.length),
                };
                buffer.overlaps(previous)
            })
        {
            return Err(DescriptorError::InvalidChain);
        }

        scratch.descriptors[scratch.count] = VirtioDescriptor {
            address: u32::try_from(address).map_err(|_| DescriptorError::InvalidChain)?,
            length,
            writable: flags & VIRTQ_DESC_F_WRITE != 0,
        };
        scratch.count += 1;

        if flags & VIRTQ_DESC_F_NEXT == 0 {
            return Ok(());
        }
        index = next;
    }
}

fn queue_metadata_ranges(
    queue: &SplitVirtqueue,
    memory_len: usize,
) -> Result<[GuestRange; 3], DescriptorError> {
    let size = u64::from(queue.size());
    Ok([
        GuestRange::new(queue.descriptor_address(), 16 * size, 16, memory_len)
            .ok_or(DescriptorError::InvalidChain)?,
        GuestRange::new(queue.available_address(), 6 + 2 * size, 2, memory_len)
            .ok_or(DescriptorError::InvalidChain)?,
        GuestRange::new(queue.used_address(), 6 + 8 * size, 4, memory_len)
            .ok_or(DescriptorError::InvalidChain)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_queue(memory: &MachineMemory) -> SplitVirtqueue {
        let mut queue = SplitVirtqueue::default();
        queue.set_size(8);
        queue.set_descriptor_low(0x1000);
        queue.set_available_low(0x2000);
        queue.set_used_low(0x3000);
        queue.try_enable(memory);
        assert!(queue.ready());
        queue
    }

    fn write_descriptor(
        memory: &mut MachineMemory,
        index: u16,
        address: u64,
        length: u32,
        flags: u16,
        next: u16,
    ) {
        let base = 0x1000 + u32::from(index) * 16;
        memory.store_u64(base, address).unwrap();
        memory.store_i32(base + 8, length as i32).unwrap();
        memory.store_u16(base + 12, flags).unwrap();
        memory.store_u16(base + 14, next).unwrap();
    }

    #[test]
    fn parser_preserves_a_valid_direct_chain() {
        let mut memory = MachineMemory::zeroed(0x1_0000).unwrap();
        let queue = ready_queue(&memory);
        write_descriptor(&mut memory, 3, 0x4000, 4, VIRTQ_DESC_F_NEXT, 5);
        write_descriptor(&mut memory, 5, 0x5000, 8, VIRTQ_DESC_F_WRITE, 0);
        let mut scratch = DescriptorScratch::default();

        let chain = parse_chain(&memory, &queue, 3, &mut scratch).unwrap();

        assert_eq!(chain.head(), 3);
        assert_eq!(
            chain.descriptors(),
            &[
                VirtioDescriptor {
                    address: 0x4000,
                    length: 4,
                    writable: false,
                },
                VirtioDescriptor {
                    address: 0x5000,
                    length: 8,
                    writable: true,
                },
            ]
        );
    }

    #[test]
    fn parser_rejects_every_unbounded_or_ambiguous_chain() {
        let cases = [
            (8, 0x4000, 4, 0, 0),
            (0, 0x4000, 4, VIRTQ_DESC_F_NEXT, 8),
            (0, 0x4000, 4, VIRTQ_DESC_F_NEXT, 0),
            (0, 0x4000, 4, VIRTQ_DESC_F_INDIRECT, 0),
            (0, 0x1_0000_0000, 4, 0, 0),
            (0, 0xffff_fff0, 32, 0, 0),
            (0, 0xfff0, 32, 0, 0),
            (0, 0x1000, 4, 0, 0),
        ];

        for (head, address, length, flags, next) in cases {
            let mut memory = MachineMemory::zeroed(0x1_0000).unwrap();
            let queue = ready_queue(&memory);
            write_descriptor(&mut memory, 0, address, length, flags, next);
            let mut scratch = DescriptorScratch::default();

            assert!(parse_chain(&memory, &queue, head, &mut scratch).is_err());
            assert_eq!(scratch.count(), 0);
        }
    }

    #[test]
    fn parser_rejects_overlapping_data_buffers_without_leaking_scratch() {
        let mut memory = MachineMemory::zeroed(0x1_0000).unwrap();
        let queue = ready_queue(&memory);
        write_descriptor(&mut memory, 0, 0x4000, 8, VIRTQ_DESC_F_NEXT, 1);
        write_descriptor(&mut memory, 1, 0x4004, 8, 0, 0);
        let mut scratch = DescriptorScratch::default();

        assert!(parse_chain(&memory, &queue, 0, &mut scratch).is_err());
        assert_eq!(scratch.count(), 0);
    }
}
