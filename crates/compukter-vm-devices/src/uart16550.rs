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
}
