/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */
//! Benchmark-only external profiler barriers; absent from the VM library.
use std::{fs, io::Write, path::PathBuf, time::Instant};

pub(super) struct Profile(PathBuf);

impl Profile {
    pub(super) fn from_environment() -> Option<Self> {
        std::env::var_os("COMPUKTERS_BENCHMARK_PHASES").map(|path| Self(path.into()))
    }

    pub(super) fn wait(&self, phase: &str) {
        let mut ready = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.0.join(format!("{phase}.ready")))
            .expect("fresh profiler phase directory");
        writeln!(ready, "{}", std::process::id()).expect("profiler process identity");
        let go = self.0.join(format!("{phase}.go"));
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while !go.exists() {
            assert!(
                Instant::now() < deadline,
                "external profiler barrier timeout"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}
