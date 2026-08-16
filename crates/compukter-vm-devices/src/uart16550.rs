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

const FIFO_CAPACITY: usize = 16;

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

    #[cfg(test)]
    fn set_fifo_enabled_for_test(&mut self, enabled: bool) {
        self.fifo_enabled = enabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
