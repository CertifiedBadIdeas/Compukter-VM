/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */
//! Opt-in cold-path observations. No event log or per-machine retained state.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};
use std::time::Instant;

pub(super) static CANCELED: AtomicU64 = AtomicU64::new(0);
pub(super) fn now() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_nanos()
        .min(u128::from(u64::MAX - 1)) as u64
        + 1
}

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        #[derive(Debug, Default)]
        pub(super) struct Counters { $(pub $name: AtomicU64,)* }
        #[derive(Debug, Default, Clone, Copy)]
        pub(crate) struct Snapshot { $(pub $name: u64,)* }
        impl Counters {
            pub fn snapshot(&self) -> Snapshot {
                Snapshot { $($name: self.$name.load(Ordering::Relaxed),)* }
            }
        }
        impl Snapshot {
            #[cfg(feature = "guest-benchmark")]
            pub fn header() -> &'static str { concat!($(stringify!($name), "\t",)* "compiler_cpu_supported") }
            #[cfg(feature = "guest-benchmark")]
            pub fn row(&self) -> String {
                let mut row = String::new();
                $(row.push_str(&format!("{}\t", self.$name));)*
                row.push_str(if cfg!(all(target_os = "linux", target_pointer_width = "64")) { "true" } else { "false" });
                row
            }
        }
    };
}
counters!(
    requests,
    denied_global,
    denied_program,
    unsupported,
    queue_full,
    channel_full,
    queue_deferred,
    submitted,
    started,
    published,
    queue_wait_ns,
    queue_wait_max_ns,
    request_wait_ns,
    request_wait_max_ns,
    compile_cpu_ns,
    first_requested_ns,
    first_published_ns
);

/// Linux thread CPU, distinct from elapsed compilation time and worker CPU.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub(super) fn cpu_ns() -> u64 {
    use std::ffi::{c_int, c_long};
    #[repr(C)]
    struct Timespec {
        seconds: c_long,
        nanoseconds: c_long,
    }
    unsafe extern "C" {
        fn clock_gettime(clock_id: c_int, time: *mut Timespec) -> c_int;
    }
    let mut time = std::mem::MaybeUninit::<Timespec>::uninit();
    // SAFETY: the pointer is valid for a complete Linux native timespec; ID 3 is THREAD_CPUTIME_ID.
    assert_eq!(unsafe { clock_gettime(3, time.as_mut_ptr()) }, 0);
    // SAFETY: a successful clock_gettime initialized every timespec field.
    let time = unsafe { time.assume_init() };
    assert!(time.seconds >= 0 && (0..1_000_000_000).contains(&time.nanoseconds));
    time.seconds as u64 * 1_000_000_000 + time.nanoseconds as u64
}
#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
pub(super) fn cpu_ns() -> u64 {
    0
}
