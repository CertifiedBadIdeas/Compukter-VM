/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 * You may obtain a copy at https://www.apache.org/licenses/LICENSE-2.0
 */

//! Weak identity publication with prompt last-owner cleanup.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

type Key = [u8; 32];
struct Bucket<T> {
    revision: u64,
    entries: Vec<Weak<T>>,
}
struct State<T> {
    revision: u64,
    buckets: BTreeMap<Key, Bucket<T>>,
}
pub(crate) struct LiveRegistry<T>(Mutex<State<T>>);

impl<T> LiveRegistry<T> {
    pub const fn new() -> Self {
        Self(Mutex::new(State {
            revision: 0,
            buckets: BTreeMap::new(),
        }))
    }

    pub fn intern(&self, key: Key, candidate: Arc<T>, equal: impl Fn(&T, &T) -> bool) -> Arc<T> {
        loop {
            // Upgraded owners must be destroyed outside the lock: their Drop
            // implementations unregister themselves from this same registry.
            let (revision, owners) = {
                let state = self
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match state.buckets.get(&key) {
                    Some(bucket) => (
                        Some(bucket.revision),
                        bucket
                            .entries
                            .iter()
                            .filter_map(Weak::upgrade)
                            .collect::<Vec<_>>(),
                    ),
                    None => (None, Vec::new()),
                }
            };
            if let Some(owner) = owners.iter().find(|owner| equal(owner, &candidate)) {
                return Arc::clone(owner);
            }
            drop(owners);
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.buckets.get(&key).map(|bucket| bucket.revision) != revision {
                continue;
            }
            state.revision = state
                .revision
                .checked_add(1)
                .expect("registry revision exhausted");
            let next = state.revision;
            let bucket = state.buckets.entry(key).or_insert_with(|| Bucket {
                revision: next,
                entries: Vec::new(),
            });
            bucket.revision = next;
            bucket.entries.push(Arc::downgrade(&candidate));
            drop(state);
            return candidate;
        }
    }

    pub fn remove(&self, key: Key, owner: *const T) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(bucket) = state.buckets.get_mut(&key) {
            bucket.entries.retain(|entry| entry.as_ptr() != owner);
            if bucket.entries.is_empty() {
                state.buckets.remove(&key);
            }
            // Removing entries cannot introduce a new equal candidate. Inserts
            // change the globally unique revision to protect against ABA.
        }
    }

    #[cfg(test)]
    pub fn contains(&self, key: Key) -> bool {
        self.0.lock().unwrap().buckets.contains_key(&key)
    }
}
