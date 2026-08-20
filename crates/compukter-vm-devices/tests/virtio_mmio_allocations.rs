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

#[path = "support/virtio.rs"]
mod virtio_support;

use compukter_vm::bus::MachineBus;
use compukter_vm_devices::virtio::VirtioMmioDevice;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use virtio_support::{
    queue_image, EchoDevice, AVAILABLE_ADDRESS, DESCRIPTOR_ADDRESS, INPUT_ADDRESS, OUTPUT_ADDRESS,
    QUEUE_SIZE, USED_ADDRESS, VIRTIO_BASE,
};

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
fn steady_state_virtio_notifications_allocate_nothing() {
    let mut bus = MachineBus::new(0x1_0000).unwrap();
    bus.memory_mut()
        .write_bytes(DESCRIPTOR_ADDRESS, &queue_image(&[0]))
        .unwrap();
    bus.map_mmio(
        VIRTIO_BASE,
        Box::new(VirtioMmioDevice::new(EchoDevice).unwrap()),
    )
    .unwrap();
    configure_transport(&mut bus);

    bus.store_i32(VIRTIO_BASE + 0x050, 0).unwrap();
    bus.store_i32(VIRTIO_BASE + 0x064, 1).unwrap();
    assert_eq!(bus.memory().load_u16(USED_ADDRESS + 2).unwrap(), 1);

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    for index in 1_u16..=32 {
        let input = index as u8;
        bus.memory_mut().store_u8(INPUT_ADDRESS, input).unwrap();
        bus.memory_mut().store_u8(OUTPUT_ADDRESS, 0).unwrap();
        let available_slot = index & (QUEUE_SIZE - 1);
        bus.memory_mut()
            .store_u16(AVAILABLE_ADDRESS + 4 + u32::from(available_slot) * 2, 0)
            .unwrap();
        bus.memory_mut()
            .store_u16(AVAILABLE_ADDRESS + 2, index.wrapping_add(1))
            .unwrap();

        bus.store_i32(VIRTIO_BASE + 0x050, 0).unwrap();

        assert_eq!(bus.memory().load_u8(OUTPUT_ADDRESS).unwrap(), input);
        assert_eq!(
            bus.memory().load_u16(USED_ADDRESS + 2).unwrap(),
            index.wrapping_add(1)
        );
        bus.store_i32(VIRTIO_BASE + 0x064, 1).unwrap();
    }
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);

    assert_eq!(allocations, 0, "VirtIO notification path allocated");
    assert_eq!(
        allocated_bytes, 0,
        "VirtIO notification path allocated bytes"
    );
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
