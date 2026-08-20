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

use compukter_vm::bus::{MachineBus, MmioDeviceId};
use compukter_vm_devices::virtio::{VirtioBlockDevice, VirtioMmioDevice};

const VIRTIO_BASE: u32 = 0x1000_2000;
const DESCRIPTOR_ADDRESS: u32 = 0x4000;
const AVAILABLE_ADDRESS: u32 = 0x4100;
const USED_ADDRESS: u32 = 0x4200;
const HEADER_ADDRESS: u32 = 0x4300;
const DATA_ADDRESS: u32 = 0x4400;
const SECOND_DATA_ADDRESS: u32 = 0x4600;
const STATUS_ADDRESS: u32 = 0x4800;
const COALESCED_ADDRESS: u32 = 0x5000;
const QUEUE_SIZE: u16 = 8;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

#[test]
fn block_device_exposes_standard_capacity_features_and_backing() {
    let mut bytes = vec![0_u8; 1024];
    bytes[7] = 0x5a;
    let mut block = VirtioBlockDevice::from_bytes(bytes, true).unwrap();

    assert_eq!(block.bytes()[7], 0x5a);
    assert!(block.read_only());
    block.bytes_mut()[8] = 0xa5;

    let mut bus = MachineBus::new(0x1_0000).unwrap();
    bus.map_mmio(VIRTIO_BASE, Box::new(VirtioMmioDevice::new(block).unwrap()))
        .unwrap();

    assert_eq!(bus.load_i32(VIRTIO_BASE + 0x008).unwrap(), 2);
    assert_eq!(bus.load_u64(VIRTIO_BASE + 0x100).unwrap(), 2);
    assert_eq!(bus.load_i32(VIRTIO_BASE + 0x114).unwrap(), 512);
    let features = bus.load_i32(VIRTIO_BASE + 0x010).unwrap() as u32;
    assert_eq!(
        features & ((1 << 5) | (1 << 6) | (1 << 9)),
        (1 << 5) | (1 << 6) | (1 << 9)
    );
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

#[test]
fn block_read_copies_multiple_data_descriptors_and_completes() {
    let bytes: Vec<u8> = (0..1024).map(|index| index as u8).collect();
    let expected = bytes[512..].to_vec();
    let (mut bus, _) = block_bus(bytes, false);
    write_request_header(&mut bus, 0, 1);
    write_descriptor(&mut bus, 0, HEADER_ADDRESS, 16, VIRTQ_DESC_F_NEXT, 1);
    write_descriptor(
        &mut bus,
        1,
        DATA_ADDRESS,
        256,
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
        2,
    );
    write_descriptor(
        &mut bus,
        2,
        SECOND_DATA_ADDRESS,
        256,
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
        3,
    );
    write_descriptor(&mut bus, 3, STATUS_ADDRESS, 1, VIRTQ_DESC_F_WRITE, 0);

    submit(&mut bus, 0, 1);

    assert_eq!(
        &bus.memory().bytes()[DATA_ADDRESS as usize..DATA_ADDRESS as usize + 256],
        &expected[..256]
    );
    assert_eq!(
        &bus.memory().bytes()[SECOND_DATA_ADDRESS as usize..SECOND_DATA_ADDRESS as usize + 256],
        &expected[256..]
    );
    assert_eq!(bus.memory().load_u8(STATUS_ADDRESS).unwrap(), 0);
    assert_eq!(bus.memory().load_i32(USED_ADDRESS + 8).unwrap(), 513);
}

#[test]
fn block_write_copies_multiple_data_descriptors_and_reset_preserves_media() {
    let (mut bus, device_id) = block_bus(vec![0; 1024], false);
    let first = vec![0x5a; 256];
    let second = vec![0xa5; 256];
    bus.memory_mut().write_bytes(DATA_ADDRESS, &first).unwrap();
    bus.memory_mut()
        .write_bytes(SECOND_DATA_ADDRESS, &second)
        .unwrap();
    write_request_header(&mut bus, 1, 0);
    write_descriptor(&mut bus, 0, HEADER_ADDRESS, 16, VIRTQ_DESC_F_NEXT, 1);
    write_descriptor(&mut bus, 1, DATA_ADDRESS, 256, VIRTQ_DESC_F_NEXT, 2);
    write_descriptor(&mut bus, 2, SECOND_DATA_ADDRESS, 256, VIRTQ_DESC_F_NEXT, 3);
    write_descriptor(&mut bus, 3, STATUS_ADDRESS, 1, VIRTQ_DESC_F_WRITE, 0);

    submit(&mut bus, 0, 1);

    let block = bus
        .device::<VirtioMmioDevice<VirtioBlockDevice>>(device_id)
        .unwrap()
        .device();
    assert_eq!(&block.bytes()[..256], &first);
    assert_eq!(&block.bytes()[256..512], &second);
    assert_eq!(bus.memory().load_u8(STATUS_ADDRESS).unwrap(), 0);
    assert_eq!(bus.memory().load_i32(USED_ADDRESS + 8).unwrap(), 1);

    bus.store_i32(VIRTIO_BASE + 0x070, 0).unwrap();
    assert_eq!(
        bus.device::<VirtioMmioDevice<VirtioBlockDevice>>(device_id)
            .unwrap()
            .device()
            .bytes()[0],
        0x5a
    );
}

#[test]
fn block_flush_completes_without_changing_media() {
    let bytes = vec![0x3c; 512];
    let (mut bus, device_id) = block_bus(bytes.clone(), false);
    write_request_header(&mut bus, 4, 0);
    write_descriptor(&mut bus, 0, HEADER_ADDRESS, 16, VIRTQ_DESC_F_NEXT, 1);
    write_descriptor(&mut bus, 1, STATUS_ADDRESS, 1, VIRTQ_DESC_F_WRITE, 0);

    submit(&mut bus, 0, 1);

    assert_eq!(bus.memory().load_u8(STATUS_ADDRESS).unwrap(), 0);
    assert_eq!(bus.memory().load_i32(USED_ADDRESS + 8).unwrap(), 1);
    assert_eq!(
        bus.device::<VirtioMmioDevice<VirtioBlockDevice>>(device_id)
            .unwrap()
            .device()
            .bytes(),
        bytes
    );
}

#[test]
fn block_reports_standard_statuses_without_partial_rejected_writes() {
    let original = vec![0x11; 1024];
    let (mut bus, device_id) = block_bus(original.clone(), true);
    bus.memory_mut()
        .write_bytes(DATA_ADDRESS, &[0x77; 512])
        .unwrap();
    write_request_header(&mut bus, 1, 0);
    write_descriptor(&mut bus, 0, HEADER_ADDRESS, 16, VIRTQ_DESC_F_NEXT, 1);
    write_descriptor(&mut bus, 1, DATA_ADDRESS, 512, VIRTQ_DESC_F_NEXT, 2);
    write_descriptor(&mut bus, 2, STATUS_ADDRESS, 1, VIRTQ_DESC_F_WRITE, 0);

    submit(&mut bus, 0, 1);

    assert_eq!(bus.memory().load_u8(STATUS_ADDRESS).unwrap(), 1);
    assert_eq!(bus.memory().load_i32(USED_ADDRESS + 8).unwrap(), 1);
    assert_eq!(
        bus.device::<VirtioMmioDevice<VirtioBlockDevice>>(device_id)
            .unwrap()
            .device()
            .bytes(),
        original
    );

    write_request_header(&mut bus, 0xffff, 0);
    submit(&mut bus, 0, 2);
    assert_eq!(bus.memory().load_u8(STATUS_ADDRESS).unwrap(), 2);
    assert_eq!(bus.memory().load_i32(USED_ADDRESS + 16).unwrap(), 1);
}

#[test]
fn block_out_of_range_write_is_rejected_before_media_mutation() {
    let original = vec![0x22; 1024];
    let (mut bus, device_id) = block_bus(original.clone(), false);
    bus.memory_mut()
        .write_bytes(DATA_ADDRESS, &[0x88; 512])
        .unwrap();
    write_request_header(&mut bus, 1, 2);
    write_descriptor(&mut bus, 0, HEADER_ADDRESS, 16, VIRTQ_DESC_F_NEXT, 1);
    write_descriptor(&mut bus, 1, DATA_ADDRESS, 512, VIRTQ_DESC_F_NEXT, 2);
    write_descriptor(&mut bus, 2, STATUS_ADDRESS, 1, VIRTQ_DESC_F_WRITE, 0);

    submit(&mut bus, 0, 1);

    assert_eq!(bus.memory().load_u8(STATUS_ADDRESS).unwrap(), 1);
    assert_eq!(
        bus.device::<VirtioMmioDevice<VirtioBlockDevice>>(device_id)
            .unwrap()
            .device()
            .bytes(),
        original
    );
}

#[test]
fn malformed_block_layout_requires_transport_reset_without_completion() {
    let (mut bus, _) = block_bus(vec![0; 512], false);
    bus.memory_mut().store_u8(STATUS_ADDRESS, 0xff).unwrap();
    write_request_header(&mut bus, 0, 0);
    write_descriptor(&mut bus, 0, HEADER_ADDRESS, 16, VIRTQ_DESC_F_NEXT, 1);
    write_descriptor(&mut bus, 1, DATA_ADDRESS, 512, VIRTQ_DESC_F_NEXT, 2);
    write_descriptor(&mut bus, 2, STATUS_ADDRESS, 1, VIRTQ_DESC_F_WRITE, 0);

    submit(&mut bus, 0, 1);

    assert_eq!(bus.memory().load_u8(STATUS_ADDRESS).unwrap(), 0xff);
    assert_eq!(bus.memory().load_u16(USED_ADDRESS + 2).unwrap(), 0);
    assert_ne!(bus.load_i32(VIRTIO_BASE + 0x070).unwrap() & 64, 0);
}

#[test]
fn block_write_accepts_a_split_header_coalesced_with_data() {
    let (mut bus, device_id) = block_bus(vec![0; 512], false);
    let mut tail = vec![0_u8; 8 + 512];
    tail[..8].copy_from_slice(&0_u64.to_le_bytes());
    tail[8..].fill(0x6d);
    bus.memory_mut().store_i32(HEADER_ADDRESS, 1).unwrap();
    bus.memory_mut().store_i32(HEADER_ADDRESS + 4, 0).unwrap();
    bus.memory_mut()
        .write_bytes(COALESCED_ADDRESS, &tail)
        .unwrap();
    write_descriptor(&mut bus, 0, HEADER_ADDRESS, 8, VIRTQ_DESC_F_NEXT, 1);
    write_descriptor(
        &mut bus,
        1,
        COALESCED_ADDRESS,
        tail.len() as u32,
        VIRTQ_DESC_F_NEXT,
        2,
    );
    write_descriptor(&mut bus, 2, STATUS_ADDRESS, 1, VIRTQ_DESC_F_WRITE, 0);

    submit(&mut bus, 0, 1);

    assert_eq!(bus.memory().load_u8(STATUS_ADDRESS).unwrap(), 0);
    assert_eq!(
        bus.device::<VirtioMmioDevice<VirtioBlockDevice>>(device_id)
            .unwrap()
            .device()
            .bytes(),
        &[0x6d; 512]
    );
}

#[test]
fn block_read_accepts_data_and_status_in_one_writable_descriptor() {
    let (mut bus, _) = block_bus(vec![0x7e; 512], false);
    write_request_header(&mut bus, 0, 0);
    write_descriptor(&mut bus, 0, HEADER_ADDRESS, 16, VIRTQ_DESC_F_NEXT, 1);
    write_descriptor(&mut bus, 1, DATA_ADDRESS, 513, VIRTQ_DESC_F_WRITE, 0);

    submit(&mut bus, 0, 1);

    assert_eq!(
        &bus.memory().bytes()[DATA_ADDRESS as usize..DATA_ADDRESS as usize + 512],
        &[0x7e; 512]
    );
    assert_eq!(bus.memory().load_u8(DATA_ADDRESS + 512).unwrap(), 0);
    assert_eq!(bus.memory().load_i32(USED_ADDRESS + 8).unwrap(), 513);
}

fn block_bus(bytes: Vec<u8>, read_only: bool) -> (MachineBus, MmioDeviceId) {
    let mut bus = MachineBus::new(0x1_0000).unwrap();
    let device = VirtioBlockDevice::from_bytes(bytes, read_only).unwrap();
    let id = bus
        .map_mmio(
            VIRTIO_BASE,
            Box::new(VirtioMmioDevice::new(device).unwrap()),
        )
        .unwrap();
    configure_transport(&mut bus);
    (bus, id)
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

fn write_request_header(bus: &mut MachineBus, request_type: u32, sector: u64) {
    bus.memory_mut()
        .store_i32(HEADER_ADDRESS, request_type as i32)
        .unwrap();
    bus.memory_mut().store_i32(HEADER_ADDRESS + 4, 0).unwrap();
    bus.memory_mut()
        .store_u64(HEADER_ADDRESS + 8, sector)
        .unwrap();
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

fn submit(bus: &mut MachineBus, head: u16, available_index: u16) {
    let slot = available_index.wrapping_sub(1) & (QUEUE_SIZE - 1);
    bus.memory_mut()
        .store_u16(AVAILABLE_ADDRESS + 4 + u32::from(slot) * 2, head)
        .unwrap();
    bus.memory_mut()
        .store_u16(AVAILABLE_ADDRESS + 2, available_index)
        .unwrap();
    bus.store_i32(VIRTIO_BASE + 0x050, 0).unwrap();
}
