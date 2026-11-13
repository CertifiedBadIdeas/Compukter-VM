/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

use std::sync::Mutex;

/// Exclusive temporary buffers shared by JNI advance calls, not by VM sessions.
pub(crate) struct AdvanceScratchPool {
    idle: Mutex<Vec<Vec<u8>>>,
    maximum_retained: usize,
}

impl AdvanceScratchPool {
    pub(crate) const fn new(maximum_retained: usize) -> Self {
        Self {
            idle: Mutex::new(Vec::new()),
            maximum_retained,
        }
    }

    pub(crate) fn lease(&self, size: usize) -> AdvanceScratch<'_> {
        let mut buffer = self
            .idle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop()
            .unwrap_or_default();
        buffer.resize(size, 0);
        AdvanceScratch {
            pool: self,
            buffer: Some(buffer),
        }
    }
}

pub(crate) struct AdvanceScratch<'a> {
    pool: &'a AdvanceScratchPool,
    buffer: Option<Vec<u8>>,
}

impl AdvanceScratch<'_> {
    pub(crate) fn bytes(&mut self) -> &mut [u8] {
        self.buffer.as_mut().expect("scratch lease owns its buffer")
    }
}

impl Drop for AdvanceScratch<'_> {
    fn drop(&mut self) {
        let mut idle = self.pool.idle.lock().unwrap_or_else(|e| e.into_inner());
        if idle.len() < self.pool.maximum_retained {
            idle.push(self.buffer.take().expect("scratch lease owns its buffer"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AdvanceScratchPool;

    #[test]
    fn leases_are_exclusive_and_retention_is_bounded_even_on_unwind() {
        let pool = AdvanceScratchPool::new(1);
        let _ = std::panic::catch_unwind(|| {
            let mut first = pool.lease(64);
            first.bytes()[0] = 7;
            let mut second = pool.lease(64);
            assert_ne!(first.bytes().as_ptr(), second.bytes().as_ptr());
            drop(second);
            panic!("test unwind");
        });
        assert_eq!(pool.idle.lock().unwrap().len(), 1);
        let mut reused = pool.lease(64);
        assert_eq!(reused.bytes().len(), 64);
    }
}
