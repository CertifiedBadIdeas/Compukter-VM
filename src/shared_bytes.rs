/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 * You may obtain a copy at https://www.apache.org/licenses/LICENSE-2.0
 */

//! Immutable file contents shared across independent computer namespaces.
use crate::live_registry::LiveRegistry;
use sha2::{Digest, Sha256};
use std::ops::Deref;
use std::sync::Arc;

static BLOBS: LiveRegistry<Blob> = LiveRegistry::new();
#[derive(Debug)]
struct Blob {
    key: [u8; 32],
    bytes: Arc<[u8]>,
}
impl Drop for Blob {
    fn drop(&mut self) {
        BLOBS.remove(self.key, self);
    }
}
#[derive(Clone, Debug)]
pub(crate) struct SharedBytes(Arc<Blob>);
impl SharedBytes {
    #[cfg(any(test, feature = "program-sharing-benchmark"))]
    pub fn new_unshared(bytes: Arc<[u8]>) -> Self {
        Self(Arc::new(Blob {
            key: Sha256::digest(&bytes).into(),
            bytes,
        }))
    }

    pub fn new(bytes: Arc<[u8]>) -> Self {
        let key = Sha256::digest(&bytes).into();
        Self(BLOBS.intern(key, Arc::new(Blob { key, bytes }), |a, b| {
            a.bytes == b.bytes
        }))
    }
}
impl Deref for SharedBytes {
    type Target = Arc<[u8]>;
    fn deref(&self) -> &Self::Target {
        &self.0.bytes
    }
}
impl PartialEq for SharedBytes {
    fn eq(&self, other: &Self) -> bool {
        self.0.bytes == other.0.bytes
    }
}
impl Eq for SharedBytes {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identical_contents_share_and_final_release_removes_the_index() {
        let first = SharedBytes::new(Arc::from(&b"unique shared filesystem blob test"[..]));
        let second = SharedBytes::new(Arc::from(first.to_vec()));
        assert!(Arc::ptr_eq(&first, &second));
        let key = first.0.key;
        let weak = Arc::downgrade(&first.0);
        drop(first);
        drop(second);
        assert!(weak.upgrade().is_none());
        assert!(!BLOBS.contains(key));
    }
}
