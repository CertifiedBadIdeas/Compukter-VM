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

use compukter_vm::bus::{MmioAccessWidth, MmioContext, MmioDevice};
use compukter_vm::memory::MemoryFault;

const FIFO_CAPACITY: usize = 16;
const UART_REGISTER_BYTES: u32 = 8;
const FCR_ENABLE: u8 = 1 << 0;
const FCR_RX_RESET: u8 = 1 << 1;
const FCR_TX_RESET: u8 = 1 << 2;
const FCR_STORED: u8 = FCR_ENABLE | 0xc0;
const IER_RX: u8 = 1 << 0;
const IER_TX: u8 = 1 << 1;
const IER_LINE_STATUS: u8 = 1 << 2;
const IER_SUPPORTED: u8 = IER_RX | IER_TX | IER_LINE_STATUS;
const IIR_NO_PENDING: u8 = 0x01;
const IIR_TX_READY: u8 = 0x02;
const IIR_RX_READY: u8 = 0x04;
const IIR_LINE_STATUS: u8 = 0x06;
const IIR_FIFO_ENABLED: u8 = 0xc0;
const LCR_DLAB: u8 = 1 << 7;
const LSR_DR: u8 = 1 << 0;
const LSR_OE: u8 = 1 << 1;
const LSR_THRE: u8 = 1 << 5;
const LSR_TEMT: u8 = 1 << 6;
const MSR_CONNECTED: u8 = 0xb0;

#[derive(Default)]
struct ByteFifo {
    bytes: [u8; FIFO_CAPACITY],
    head: u8,
    len: u8,
}

impl ByteFifo {
    fn len(&self) -> usize {
        usize::from(self.len)
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push(&mut self, byte: u8, effective_capacity: usize) -> bool {
        assert!((1..=FIFO_CAPACITY).contains(&effective_capacity));
        if self.len() >= effective_capacity {
            return false;
        }
        let tail = (usize::from(self.head) + self.len()) % FIFO_CAPACITY;
        self.bytes[tail] = byte;
        self.len += 1;
        true
    }

    fn pop(&mut self) -> Option<u8> {
        if self.is_empty() {
            return None;
        }
        let byte = self.bytes[usize::from(self.head)];
        self.head = ((usize::from(self.head) + 1) % FIFO_CAPACITY) as u8;
        self.len -= 1;
        if self.len == 0 {
            self.head = 0;
        }
        Some(byte)
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

/// Result of transferring a borrowed host byte slice at the UART boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartTransferResult {
    /// Bytes accepted by the bounded device queue.
    pub transferred: usize,
    /// Bytes rejected because the attachment was disconnected or full.
    pub dropped: usize,
}

impl UartTransferResult {
    pub const fn new(transferred: usize, dropped: usize) -> Self {
        Self {
            transferred,
            dropped,
        }
    }
}

/// Allocation-free snapshot of host-visible UART loss diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Uart16550Diagnostics {
    pub rx_dropped: u64,
    pub tx_dropped: u64,
    pub receive_overrun: bool,
}

/// Deterministic, bounded 16550-style UART device model.
pub struct Uart16550 {
    rx: ByteFifo,
    tx: ByteFifo,
    connected: bool,
    fifo_enabled: bool,
    receive_overrun: bool,
    rx_dropped: u64,
    tx_dropped: u64,
    ier: u8,
    lcr: u8,
    mcr: u8,
    scr: u8,
    divisor: u16,
    fcr: u8,
}

impl Default for Uart16550 {
    fn default() -> Self {
        Self::new()
    }
}

impl Uart16550 {
    /// Constructs a disconnected UART with empty fixed-capacity queues.
    pub fn new() -> Self {
        Self {
            rx: ByteFifo::default(),
            tx: ByteFifo::default(),
            connected: false,
            fifo_enabled: false,
            receive_overrun: false,
            rx_dropped: 0,
            tx_dropped: 0,
            ier: 0,
            lcr: 0,
            mcr: 0,
            scr: 0,
            divisor: 0,
            fcr: 0,
        }
    }

    /// Connects the external byte-stream attachment while the VM is stopped.
    pub fn connect(&mut self) {
        self.connected = true;
    }

    /// Disconnects the attachment and drops bytes not yet drained from TX.
    pub fn disconnect(&mut self) {
        if self.connected {
            self.tx_dropped = self
                .tx_dropped
                .saturating_add(u64::try_from(self.tx.len()).unwrap_or(u64::MAX));
            self.tx.clear();
        }
        self.connected = false;
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Injects received bytes from the connected host attachment.
    pub fn inject_rx(&mut self, bytes: &[u8]) -> UartTransferResult {
        if !self.connected {
            let dropped = bytes.len();
            self.rx_dropped = self
                .rx_dropped
                .saturating_add(u64::try_from(dropped).unwrap_or(u64::MAX));
            return UartTransferResult::new(0, dropped);
        }

        let capacity = self.effective_capacity();
        let mut transferred = 0;
        for &byte in bytes {
            if self.rx.push(byte, capacity) {
                transferred += 1;
            } else {
                self.receive_overrun = true;
            }
        }
        let dropped = bytes.len() - transferred;
        self.rx_dropped = self
            .rx_dropped
            .saturating_add(u64::try_from(dropped).unwrap_or(u64::MAX));
        UartTransferResult::new(transferred, dropped)
    }

    /// Drains pending transmitted bytes into caller-owned storage.
    pub fn drain_tx(&mut self, output: &mut [u8]) -> usize {
        let mut drained = 0;
        for slot in output {
            let Some(byte) = self.tx.pop() else {
                break;
            };
            *slot = byte;
            drained += 1;
        }
        drained
    }

    pub fn diagnostics(&self) -> Uart16550Diagnostics {
        Uart16550Diagnostics {
            rx_dropped: self.rx_dropped,
            tx_dropped: self.tx_dropped,
            receive_overrun: self.receive_overrun,
        }
    }

    /// Clears cumulative host counters without acknowledging guest state.
    pub fn clear_diagnostics(&mut self) {
        self.rx_dropped = 0;
        self.tx_dropped = 0;
    }

    fn effective_capacity(&self) -> usize {
        if self.fifo_enabled {
            FIFO_CAPACITY
        } else {
            1
        }
    }

    fn read_receive_byte(&mut self) -> Option<u8> {
        self.rx.pop()
    }

    fn write_transmit_byte(&mut self, byte: u8) {
        if !self.connected || !self.tx.push(byte, self.effective_capacity()) {
            self.tx_dropped = self.tx_dropped.saturating_add(1);
        }
    }

    fn transmitter_ready(&self) -> bool {
        !self.connected || self.tx.len() < self.effective_capacity()
    }

    fn line_status(&self) -> u8 {
        let mut status = 0;
        if !self.rx.is_empty() {
            status |= LSR_DR;
        }
        if self.receive_overrun {
            status |= LSR_OE;
        }
        if self.transmitter_ready() {
            status |= LSR_THRE;
        }
        if self.tx.is_empty() {
            status |= LSR_TEMT;
        }
        status
    }

    fn interrupt_identification(&self) -> u8 {
        let fifo = if self.fifo_enabled {
            IIR_FIFO_ENABLED
        } else {
            0
        };
        let reason = if self.ier & IER_LINE_STATUS != 0 && self.receive_overrun {
            IIR_LINE_STATUS
        } else if self.ier & IER_RX != 0 && !self.rx.is_empty() {
            IIR_RX_READY
        } else if self.ier & IER_TX != 0 && self.transmitter_ready() {
            IIR_TX_READY
        } else {
            IIR_NO_PENDING
        };
        fifo | reason
    }

    fn write_fifo_control(&mut self, value: u8) {
        let enabled = value & FCR_ENABLE != 0;
        if enabled != self.fifo_enabled {
            self.rx.clear();
            self.tx.clear();
        }
        if value & FCR_RX_RESET != 0 {
            self.rx.clear();
        }
        if value & FCR_TX_RESET != 0 {
            self.tx.clear();
        }
        self.fifo_enabled = enabled;
        self.fcr = value & FCR_STORED;
    }

    fn read_register(&mut self, offset: u32) -> Result<u8, MemoryFault> {
        let value = match offset {
            0 if self.lcr & LCR_DLAB != 0 => self.divisor as u8,
            0 => self.read_receive_byte().unwrap_or(0),
            1 if self.lcr & LCR_DLAB != 0 => (self.divisor >> 8) as u8,
            1 => self.ier,
            2 => self.interrupt_identification(),
            3 => self.lcr,
            4 => self.mcr,
            5 => {
                let status = self.line_status();
                self.receive_overrun = false;
                status
            }
            6 => {
                if self.connected {
                    MSR_CONNECTED
                } else {
                    0
                }
            }
            7 => self.scr,
            _ => return Err(invalid_uart_access(offset, MmioAccessWidth::Byte)),
        };
        Ok(value)
    }

    fn write_register(&mut self, offset: u32, value: u8) -> Result<(), MemoryFault> {
        match offset {
            0 if self.lcr & LCR_DLAB != 0 => {
                self.divisor = (self.divisor & 0xff00) | u16::from(value);
            }
            0 => self.write_transmit_byte(value),
            1 if self.lcr & LCR_DLAB != 0 => {
                self.divisor = (self.divisor & 0x00ff) | (u16::from(value) << 8);
            }
            1 => self.ier = value & IER_SUPPORTED,
            2 => self.write_fifo_control(value),
            3 => self.lcr = value,
            4 => self.mcr = value & 0x1f,
            5 | 6 => {}
            7 => self.scr = value,
            _ => return Err(invalid_uart_access(offset, MmioAccessWidth::Byte)),
        }
        Ok(())
    }

    #[cfg(test)]
    fn set_fifo_enabled_for_test(&mut self, enabled: bool) {
        self.write_fifo_control(if enabled { FCR_ENABLE } else { 0 });
    }
}

impl MmioDevice for Uart16550 {
    fn size(&self) -> u32 {
        UART_REGISTER_BYTES
    }

    fn interrupt_level(&self) -> bool {
        self.interrupt_identification() & IIR_NO_PENDING == 0
    }

    fn read(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
    ) -> Result<u64, MemoryFault> {
        if width != MmioAccessWidth::Byte {
            return Err(invalid_uart_access(offset, width));
        }
        self.read_register(offset).map(u64::from)
    }

    fn write(
        &mut self,
        _context: &mut MmioContext<'_>,
        offset: u32,
        width: MmioAccessWidth,
        value: u64,
    ) -> Result<(), MemoryFault> {
        if width != MmioAccessWidth::Byte {
            return Err(invalid_uart_access(offset, width));
        }
        self.write_register(offset, value as u8)
    }
}

fn invalid_uart_access(offset: u32, width: MmioAccessWidth) -> MemoryFault {
    MemoryFault::new(format!(
        "invalid UART access at offset {offset:#x} with width {width:?}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use compukter_vm::bus::{MachineBus, MmioDeviceId};

    const UART_BASE: u32 = 0x1000;
    fn mapped_uart() -> (MachineBus, MmioDeviceId) {
        let mut bus = MachineBus::new(UART_BASE as usize).unwrap();
        let id = bus.map_mmio(UART_BASE, Box::new(Uart16550::new())).unwrap();
        (bus, id)
    }

    #[test]
    fn fixed_fifo_preserves_order_and_rejects_the_seventeenth_byte() {
        let mut fifo = ByteFifo::default();
        for byte in 0_u8..16 {
            assert!(fifo.push(byte, 16));
        }
        assert!(!fifo.push(16, 16));
        for byte in 0_u8..16 {
            assert_eq!(fifo.pop(), Some(byte));
        }
        assert_eq!(fifo.pop(), None);
    }

    #[test]
    fn one_byte_mode_and_clear_reuse_the_same_storage() {
        let mut fifo = ByteFifo::default();
        assert!(fifo.push(7, 1));
        assert!(!fifo.push(8, 1));
        fifo.clear();
        assert!(fifo.push(9, 1));
        assert_eq!(fifo.pop(), Some(9));
    }

    #[test]
    fn disconnected_traffic_is_dropped_without_backpressuring_tx() {
        let mut uart = Uart16550::new();
        assert!(!uart.is_connected());
        assert_eq!(uart.inject_rx(&[1, 2]), UartTransferResult::new(0, 2));
        uart.write_transmit_byte(3);
        assert_eq!(uart.drain_tx(&mut [0; 1]), 0);
        assert_eq!(uart.diagnostics().rx_dropped, 2);
        assert_eq!(uart.diagnostics().tx_dropped, 1);
        assert!(uart.transmitter_ready());
    }

    #[test]
    fn connected_transfer_is_bounded_and_ordered() {
        let mut uart = Uart16550::new();
        uart.connect();
        uart.set_fifo_enabled_for_test(true);
        let input = std::array::from_fn::<u8, 20, _>(|index| index as u8);
        assert_eq!(uart.inject_rx(&input), UartTransferResult::new(16, 4));
        assert!(uart.diagnostics().receive_overrun);
        for expected in 0..16 {
            assert_eq!(uart.read_receive_byte(), Some(expected));
        }
        assert_eq!(uart.read_receive_byte(), None);

        for byte in 32..48 {
            uart.write_transmit_byte(byte);
        }
        uart.write_transmit_byte(48);
        let mut output = [0_u8; 16];
        assert_eq!(uart.drain_tx(&mut output), 16);
        assert_eq!(output, std::array::from_fn(|index| 32 + index as u8));
        assert_eq!(uart.diagnostics().tx_dropped, 1);
    }

    #[test]
    fn clearing_diagnostics_preserves_guest_state_and_queued_bytes() {
        let mut uart = Uart16550::new();
        uart.connect();
        assert_eq!(uart.inject_rx(&[1, 2]), UartTransferResult::new(1, 1));

        uart.clear_diagnostics();

        assert_eq!(uart.diagnostics().rx_dropped, 0);
        assert!(uart.diagnostics().receive_overrun);
        assert_eq!(uart.read_receive_byte(), Some(1));
    }

    #[test]
    fn disconnect_drops_queued_tx_but_preserves_received_bytes() {
        let mut uart = Uart16550::new();
        uart.connect();
        uart.set_fifo_enabled_for_test(true);
        assert_eq!(uart.inject_rx(&[7]), UartTransferResult::new(1, 0));
        uart.write_transmit_byte(8);
        uart.write_transmit_byte(9);

        uart.disconnect();

        assert_eq!(uart.drain_tx(&mut [0; 2]), 0);
        assert_eq!(uart.diagnostics().tx_dropped, 2);
        assert_eq!(uart.read_receive_byte(), Some(7));
    }

    #[test]
    fn register_reset_state_and_access_width_are_strict() {
        let (mut bus, _) = mapped_uart();
        assert_eq!(bus.load_u8(UART_BASE + 5).unwrap(), LSR_THRE | LSR_TEMT);
        assert_eq!(bus.load_u8(UART_BASE + 6).unwrap(), 0);
        assert!(bus.load_i32(UART_BASE).is_err());
        assert!(bus.load_u8(UART_BASE + 8).is_err());
        assert_eq!(bus.load_u8(UART_BASE + 5).unwrap(), LSR_THRE | LSR_TEMT);
    }

    #[test]
    fn dlab_multiplexes_divisor_without_touching_data_or_ier() {
        let (mut bus, _) = mapped_uart();
        bus.store_u8(UART_BASE + 3, LCR_DLAB | 3).unwrap();
        bus.store_u8(UART_BASE, 0x34).unwrap();
        bus.store_u8(UART_BASE + 1, 0x12).unwrap();
        assert_eq!(bus.load_u8(UART_BASE).unwrap(), 0x34);
        assert_eq!(bus.load_u8(UART_BASE + 1).unwrap(), 0x12);

        bus.store_u8(UART_BASE + 3, 3).unwrap();
        assert_eq!(bus.load_u8(UART_BASE + 1).unwrap(), 0);
        assert_eq!(bus.load_u8(UART_BASE).unwrap(), 0);
    }

    #[test]
    fn fcr_switches_one_and_sixteen_byte_modes_and_resets_queues() {
        let (mut bus, id) = mapped_uart();
        bus.device_mut::<Uart16550>(id).unwrap().connect();
        assert_eq!(
            bus.device_mut::<Uart16550>(id)
                .unwrap()
                .inject_rx(&[1, 2])
                .transferred,
            1
        );

        bus.store_u8(UART_BASE + 2, FCR_ENABLE | FCR_RX_RESET | FCR_TX_RESET)
            .unwrap();
        assert_eq!(bus.load_u8(UART_BASE + 5).unwrap() & LSR_DR, 0);
        assert_eq!(
            bus.device_mut::<Uart16550>(id)
                .unwrap()
                .inject_rx(&[1; 16])
                .transferred,
            16
        );

        bus.store_u8(UART_BASE + 2, 0).unwrap();
        assert_eq!(bus.load_u8(UART_BASE + 5).unwrap() & LSR_DR, 0);
        assert_eq!(
            bus.device_mut::<Uart16550>(id)
                .unwrap()
                .inject_rx(&[3, 4])
                .transferred,
            1
        );
    }

    #[test]
    fn line_and_modem_registers_have_stable_readback_and_acknowledgement() {
        let (mut bus, id) = mapped_uart();
        bus.store_u8(UART_BASE + 3, 0x1b).unwrap();
        bus.store_u8(UART_BASE + 4, 0xff).unwrap();
        bus.store_u8(UART_BASE + 7, 0xa5).unwrap();
        assert_eq!(bus.load_u8(UART_BASE + 3).unwrap(), 0x1b);
        assert_eq!(bus.load_u8(UART_BASE + 4).unwrap(), 0x1f);
        assert_eq!(bus.load_u8(UART_BASE + 7).unwrap(), 0xa5);

        bus.device_mut::<Uart16550>(id).unwrap().connect();
        assert_eq!(bus.load_u8(UART_BASE + 6).unwrap(), 0xb0);
        let result = bus.device_mut::<Uart16550>(id).unwrap().inject_rx(&[1, 2]);
        assert_eq!(result, UartTransferResult::new(1, 1));
        assert_ne!(bus.load_u8(UART_BASE + 5).unwrap() & LSR_OE, 0);
        assert_eq!(bus.load_u8(UART_BASE + 5).unwrap() & LSR_OE, 0);

        bus.store_u8(UART_BASE + 5, 0xff).unwrap();
        bus.store_u8(UART_BASE + 6, 0xff).unwrap();
        assert_eq!(bus.load_u8(UART_BASE + 6).unwrap(), 0xb0);
    }

    #[test]
    fn interrupt_reason_prioritizes_line_status_then_rx_then_tx() {
        const IER_RX: u8 = 1 << 0;
        const IER_TX: u8 = 1 << 1;
        const IER_LINE_STATUS: u8 = 1 << 2;
        const IIR_REASON_MASK: u8 = 0x0f;
        const IIR_TX_READY: u8 = 0x02;
        const IIR_RX_READY: u8 = 0x04;
        const IIR_LINE_STATUS: u8 = 0x06;
        const IIR_NO_PENDING: u8 = 0x01;

        let (mut bus, id) = mapped_uart();
        bus.device_mut::<Uart16550>(id).unwrap().connect();
        bus.store_u8(UART_BASE + 1, IER_RX | IER_TX | IER_LINE_STATUS)
            .unwrap();
        assert_eq!(
            bus.load_u8(UART_BASE + 2).unwrap() & IIR_REASON_MASK,
            IIR_TX_READY
        );

        bus.device_mut::<Uart16550>(id).unwrap().inject_rx(&[7]);
        assert_eq!(
            bus.load_u8(UART_BASE + 2).unwrap() & IIR_REASON_MASK,
            IIR_RX_READY
        );

        bus.device_mut::<Uart16550>(id).unwrap().inject_rx(&[8]);
        assert_eq!(
            bus.load_u8(UART_BASE + 2).unwrap() & IIR_REASON_MASK,
            IIR_LINE_STATUS
        );
        assert!(bus.device::<Uart16550>(id).unwrap().interrupt_level());

        assert_ne!(bus.load_u8(UART_BASE + 5).unwrap() & LSR_OE, 0);
        assert_eq!(
            bus.load_u8(UART_BASE + 2).unwrap() & IIR_REASON_MASK,
            IIR_RX_READY
        );
        assert_eq!(bus.load_u8(UART_BASE).unwrap(), 7);
        assert_eq!(
            bus.load_u8(UART_BASE + 2).unwrap() & IIR_REASON_MASK,
            IIR_TX_READY
        );

        bus.store_u8(UART_BASE, 9).unwrap();
        assert_eq!(
            bus.load_u8(UART_BASE + 2).unwrap() & IIR_REASON_MASK,
            IIR_NO_PENDING
        );
        assert!(!bus.device::<Uart16550>(id).unwrap().interrupt_level());

        let mut output = [0_u8; 1];
        assert_eq!(
            bus.device_mut::<Uart16550>(id)
                .unwrap()
                .drain_tx(&mut output),
            1
        );
        assert_eq!(output, [9]);
        assert_eq!(
            bus.load_u8(UART_BASE + 2).unwrap() & IIR_REASON_MASK,
            IIR_TX_READY
        );
    }

    #[test]
    fn iir_is_level_derived_and_reports_fifo_mode() {
        let (mut bus, id) = mapped_uart();
        bus.device_mut::<Uart16550>(id).unwrap().connect();
        bus.store_u8(UART_BASE + 1, 1).unwrap();
        bus.store_u8(UART_BASE + 2, FCR_ENABLE | 0xc0).unwrap();
        bus.device_mut::<Uart16550>(id).unwrap().inject_rx(&[1]);

        assert_eq!(bus.load_u8(UART_BASE + 2).unwrap(), 0xc4);
        assert_eq!(bus.load_u8(UART_BASE + 2).unwrap(), 0xc4);
        assert!(bus.device::<Uart16550>(id).unwrap().interrupt_level());

        bus.store_u8(UART_BASE + 1, 1 << 3).unwrap();
        assert_eq!(bus.load_u8(UART_BASE + 1).unwrap(), 0);
        assert_eq!(bus.load_u8(UART_BASE + 2).unwrap(), 0xc1);
        assert!(!bus.device::<Uart16550>(id).unwrap().interrupt_level());
    }
}
