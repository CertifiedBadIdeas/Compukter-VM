/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 * You may obtain a copy at https://www.apache.org/licenses/LICENSE-2.0
 */

//! Best-effort identity reuse after complete verification, never a trust cache.
use super::VerifiedArtifactInner;
use std::sync::{Arc, Mutex, Weak};

const CAPACITY: usize = 128;
static INTERNER: Mutex<Interner<CAPACITY>> = Mutex::new(Interner::new());

struct Interner<const N: usize> {
    entries: [Weak<VerifiedArtifactInner>; N],
    next: usize,
}

impl<const N: usize> Interner<N> {
    const fn new() -> Self {
        assert!(N > 0);
        Self {
            entries: [const { Weak::new() }; N],
            next: 0,
        }
    }

    fn intern(&mut self, candidate: Arc<VerifiedArtifactInner>) -> Arc<VerifiedArtifactInner> {
        let mut vacant = None;
        for (index, entry) in self.entries.iter().enumerate() {
            if let Some(existing) = entry.upgrade() {
                // Container headers are not represented by the content digest.
                // Equality must include every byte, even when hashes match.
                if existing.decoded.content_hash == candidate.decoded.content_hash
                    && existing.decoded.bytes == candidate.decoded.bytes
                {
                    return existing;
                }
            } else if vacant.is_none() {
                vacant = Some(index);
            }
        }
        let index = vacant.unwrap_or(self.next);
        self.entries[index] = Arc::downgrade(&candidate);
        self.next = (index + 1) % N;
        candidate
    }
}

pub(super) fn intern(candidate: Arc<VerifiedArtifactInner>) -> Arc<VerifiedArtifactInner> {
    // The lock protects only the fixed-size weak identity index. Verification,
    // program preparation, admission and Guest execution run outside it.
    INTERNER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .intern(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{limits::ArtifactLimits, verify_artifact};
    use std::sync::OnceLock;

    fn candidate(marker: u8) -> Arc<VerifiedArtifactInner> {
        let mut decoded = crate::decode::records::decode_artifact(
            crate::test_support::minimal_vector().into(),
            &ArtifactLimits::default(),
        )
        .unwrap();
        // Deliberately collide the digest, as headers can vary independently.
        let mut bytes = decoded.bytes.to_vec();
        bytes.push(marker);
        decoded.bytes = bytes.into();
        Arc::new(VerifiedArtifactInner {
            decoded,
            prepared_program: OnceLock::new(),
        })
    }

    #[test]
    fn weak_identity_checks_complete_bytes_and_does_not_own_artifacts() {
        let mut cache = Interner::<2>::new();
        let first = cache.intern(candidate(1));
        let same = cache.intern(candidate(1));
        let changed = cache.intern(candidate(2));
        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &changed));
        let weak = Arc::downgrade(&first);
        drop(first);
        drop(same);
        assert!(weak.upgrade().is_none());
        let replacement = cache.intern(candidate(1));
        assert!(!Weak::ptr_eq(&weak, &Arc::downgrade(&replacement)));
    }

    #[test]
    fn bounded_index_eviction_preserves_live_owners() {
        let mut cache = Interner::<2>::new();
        let first = cache.intern(candidate(1));
        let second = cache.intern(candidate(2));
        let third = cache.intern(candidate(3));
        let replaced = cache.intern(candidate(1));
        assert!(!Arc::ptr_eq(&first, &replaced));
        assert_eq!(1, *first.decoded.bytes.last().unwrap());
        assert_eq!(2, *second.decoded.bytes.last().unwrap());
        assert_eq!(3, *third.decoded.bytes.last().unwrap());
        assert_eq!(2, cache.entries.len());
    }

    #[test]
    fn separate_verifications_share_identity_but_still_enforce_input_limits() {
        let bytes: Arc<[u8]> = crate::test_support::minimal_vector().into();
        let first = verify_artifact(bytes.clone(), ArtifactLimits::default()).unwrap();
        let second = verify_artifact(Arc::from(bytes.to_vec()), ArtifactLimits::default()).unwrap();
        assert!(Arc::ptr_eq(&first.inner, &second.inner));
        let restricted = ArtifactLimits {
            artifact_bytes: 1,
            ..ArtifactLimits::default()
        };
        assert!(verify_artifact(bytes, restricted).is_err());
    }

    #[test]
    fn concurrent_verified_publication_has_one_live_identity() {
        let cache = Mutex::new(Interner::<2>::new());
        let barrier = std::sync::Barrier::new(8);
        let values = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let cache = &cache;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        let candidate = candidate(9);
                        barrier.wait();
                        cache.lock().unwrap().intern(candidate)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(values.iter().all(|value| Arc::ptr_eq(&values[0], value)));
        let weak = Arc::downgrade(&values[0]);
        drop(values);
        assert!(weak.upgrade().is_none());
    }
}
