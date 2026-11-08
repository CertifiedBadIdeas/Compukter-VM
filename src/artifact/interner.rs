/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 * You may obtain a copy at https://www.apache.org/licenses/LICENSE-2.0
 */

//! Identity reuse after complete verification, never a trust cache.
use super::VerifiedArtifactInner;
use crate::live_registry::LiveRegistry;
use std::sync::Arc;

static INTERNER: LiveRegistry<VerifiedArtifactInner> = LiveRegistry::new();

pub(super) fn intern(candidate: Arc<VerifiedArtifactInner>) -> Arc<VerifiedArtifactInner> {
    INTERNER.intern(
        candidate.decoded.content_hash,
        candidate,
        |existing, candidate| {
            // Headers/debug sections can differ independently of the semantic hash.
            existing.decoded.bytes == candidate.decoded.bytes
        },
    )
}

impl Drop for VerifiedArtifactInner {
    fn drop(&mut self) {
        INTERNER.remove(self.decoded.content_hash, self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{limits::ArtifactLimits, verify_artifact};
    use std::sync::{OnceLock, Weak};

    fn candidate(marker: u16) -> Arc<VerifiedArtifactInner> {
        let mut decoded = crate::decode::records::decode_artifact(
            crate::test_support::minimal_vector().into(),
            &ArtifactLimits::default(),
        )
        .unwrap();
        // Deliberately collide the digest, as headers can vary independently.
        let mut bytes = decoded.bytes.to_vec();
        bytes.extend_from_slice(&marker.to_le_bytes());
        decoded.bytes = bytes.into();
        Arc::new(VerifiedArtifactInner {
            decoded,
            prepared_program: OnceLock::new(),
        })
    }

    #[test]
    fn weak_identity_checks_complete_bytes_and_does_not_own_artifacts() {
        let first = intern(candidate(1));
        let same = intern(candidate(1));
        let changed = intern(candidate(2));
        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &changed));
        let weak = Arc::downgrade(&first);
        drop(first);
        drop(same);
        assert!(weak.upgrade().is_none());
        let replacement = intern(candidate(1));
        assert!(!Weak::ptr_eq(&weak, &Arc::downgrade(&replacement)));
    }

    #[test]
    fn all_live_images_remain_canonical_beyond_the_old_capacity() {
        let owners: Vec<_> = (1000..=2023)
            .map(|marker| intern(candidate(marker)))
            .collect();
        for marker in 1000..=2023 {
            assert!(Arc::ptr_eq(
                &owners[usize::from(marker - 1000)],
                &intern(candidate(marker))
            ));
        }
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
        let barrier = std::sync::Barrier::new(8);
        let values = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        let candidate = candidate(9);
                        barrier.wait();
                        intern(candidate)
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
