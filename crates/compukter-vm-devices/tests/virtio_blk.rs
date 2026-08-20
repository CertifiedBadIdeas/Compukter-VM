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

const VIRTIO_BASE: u32 = 0x1000_2000;

#[test]
fn block_device_exposes_standard_capacity_features_and_backing() {
    let mut bytes = vec![0_u8; 1024];
    bytes[7] = 0x5a;
    let mut block = VirtioBlockDevice::from_bytes(bytes, true).unwrap();

    assert_eq!(block.bytes()[7], 0x5a);
    assert!(block.read_only());
    block.bytes_mut()[8] = 0xa5;

    let mut bus = MachineBus::new(0x1_0000).unwrap();
    bus.map_mmio(
        VIRTIO_BASE,
        Box::new(VirtioMmioDevice::new(block).unwrap()),
    )
    .unwrap();

    assert_eq!(bus.load_i32(VIRTIO_BASE + 0x008).unwrap(), 2);
    assert_eq!(bus.load_u64(VIRTIO_BASE + 0x100).unwrap(), 2);
    assert_eq!(bus.load_i32(VIRTIO_BASE + 0x114).unwrap(), 512);
    let features = bus.load_i32(VIRTIO_BASE + 0x010).unwrap() as u32;
    assert_eq!(features & ((1 << 5) | (1 << 6) | (1 << 9)), (1 << 5) | (1 << 6) | (1 << 9));
    assert!(bus.store_i32(VIRTIO_BASE + 0x100, 0).is_err());
}

#[test]
fn block_device_rejects_invalid_capacity_and_checks_zeroed_size() {
    assert!(VirtioBlockDevice::from_bytes(Vec::new(), false).is_err());
    assert!(VirtioBlockDevice::from_bytes(vec![0; 511], false).is_err());
    assert!(VirtioBlockDevice::zeroed(0, false).is_err());

    let block = VirtioBlockDevice::zeroed(3, false).unwrap();
    assert_eq!(block.bytes(), &[0; 1536]);
    assert!(!block.read_only());
}
