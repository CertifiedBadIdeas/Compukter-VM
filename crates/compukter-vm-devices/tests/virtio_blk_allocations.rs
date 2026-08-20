/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

use compukter_vm::bus::MachineBus;
use compukter_vm_devices::virtio::{VirtioBlockDevice, VirtioMmioDevice};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

const VIRTIO_BASE: u32 = 0x1000_2000;
const DESCRIPTOR_ADDRESS: u32 = 0x4000;
const AVAILABLE_ADDRESS: u32 = 0x4100;
const USED_ADDRESS: u32 = 0x4200;
const HEADER_ADDRESS: u32 = 0x4300;
const DATA_ADDRESS: u32 = 0x4400;
const STATUS_ADDRESS: u32 = 0x4800;
const QUEUE_SIZE: u16 = 8;

struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

#[test]
fn steady_state_block_requests_allocate_nothing() {
    let mut bus = MachineBus::new(0x1_0000).unwrap();
    let device = VirtioBlockDevice::from_bytes(vec![0x5a; 512], false).unwrap();
    bus.map_mmio(
        VIRTIO_BASE,
        Box::new(VirtioMmioDevice::new(device).unwrap()),
    )
    .unwrap();
    prepare_request(&mut bus);
    configure_transport(&mut bus);

    submit(&mut bus, 1);
    bus.store_i32(VIRTIO_BASE + 0x064, 1).unwrap();
    assert_eq!(bus.memory().load_u8(DATA_ADDRESS).unwrap(), 0x5a);

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    for available_index in 2_u16..=33 {
        bus.memory_mut().store_u8(DATA_ADDRESS, 0).unwrap();
        bus.memory_mut().store_u8(STATUS_ADDRESS, 0xff).unwrap();
        submit(&mut bus, available_index);
        assert_eq!(bus.memory().load_u8(DATA_ADDRESS).unwrap(), 0x5a);
        assert_eq!(bus.memory().load_u8(STATUS_ADDRESS).unwrap(), 0);
        bus.store_i32(VIRTIO_BASE + 0x064, 1).unwrap();
    }

    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(ALLOCATED_BYTES.load(Ordering::Relaxed), 0);
}

fn prepare_request(bus: &mut MachineBus) {
    bus.memory_mut().store_i32(HEADER_ADDRESS, 0).unwrap();
    bus.memory_mut().store_i32(HEADER_ADDRESS + 4, 0).unwrap();
    bus.memory_mut().store_u64(HEADER_ADDRESS + 8, 0).unwrap();
    write_descriptor(bus, 0, HEADER_ADDRESS, 16, 1, 1);
    write_descriptor(bus, 1, DATA_ADDRESS, 512, 1 | 2, 2);
    write_descriptor(bus, 2, STATUS_ADDRESS, 1, 2, 0);
}

fn write_descriptor(
    bus: &mut MachineBus,
    index: u16,
    address: u32,
    length: u32,
    flags: u16,
    next: u16,
) {
    let descriptor = DESCRIPTOR_ADDRESS + u32::from(index) * 16;
    bus.memory_mut()
        .store_u64(descriptor, u64::from(address))
        .unwrap();
    bus.memory_mut()
        .store_i32(descriptor + 8, length as i32)
        .unwrap();
    bus.memory_mut().store_u16(descriptor + 12, flags).unwrap();
    bus.memory_mut().store_u16(descriptor + 14, next).unwrap();
}

fn configure_transport(bus: &mut MachineBus) {
    bus.store_i32(VIRTIO_BASE + 0x024, 1).unwrap();
    bus.store_i32(VIRTIO_BASE + 0x020, 1).unwrap();
    bus.store_i32(VIRTIO_BASE + 0x070, 1 | 2 | 8).unwrap();
    bus.store_i32(VIRTIO_BASE + 0x038, i32::from(QUEUE_SIZE))
        .unwrap();
    bus.store_i32(VIRTIO_BASE + 0x080, DESCRIPTOR_ADDRESS as i32)
        .unwrap();
    bus.store_i32(VIRTIO_BASE + 0x090, AVAILABLE_ADDRESS as i32)
        .unwrap();
    bus.store_i32(VIRTIO_BASE + 0x0a0, USED_ADDRESS as i32)
        .unwrap();
    bus.store_i32(VIRTIO_BASE + 0x044, 1).unwrap();
    bus.store_i32(VIRTIO_BASE + 0x070, 1 | 2 | 4 | 8).unwrap();
}

fn submit(bus: &mut MachineBus, available_index: u16) {
    let slot = available_index.wrapping_sub(1) & (QUEUE_SIZE - 1);
    bus.memory_mut()
        .store_u16(AVAILABLE_ADDRESS + 4 + u32::from(slot) * 2, 0)
        .unwrap();
    bus.memory_mut()
        .store_u16(AVAILABLE_ADDRESS + 2, available_index)
        .unwrap();
    bus.store_i32(VIRTIO_BASE + 0x050, 0).unwrap();
}
