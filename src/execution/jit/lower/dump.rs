/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */
//! Bounded opt-in compiler artifacts, never enabled in timing/allocation builds.
use super::*;
use std::{fs, path::PathBuf};

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub(super) struct Capture(PathBuf);

impl Capture {
    pub(super) fn error(&self, error: &str) {
        if let Err(error) = fs::write(self.0.join("error.txt"), error) {
            eprintln!("JIT diagnostic capture: {error}");
        }
    }
    pub(super) fn begin(plan: &Plan, ctx: &mut cranelift_codegen::Context) -> Option<Self> {
        let root = std::env::var_os("COMPUKTERS_JIT_DUMP")?;
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        if index >= 256 {
            return None;
        }
        let path = PathBuf::from(root).join(format!("{index:03}"));
        let result = (|| -> std::io::Result<()> {
            fs::create_dir_all(&path)?;
            fs::write(path.join("input.clif"), ctx.func.display().to_string())?;
            fs::write(
                path.join("plan.txt"),
                format!(
                    "regions={:?}\nrequired_bytes={}\nheap={}\nops={:?}\ncopies={:?}\n",
                    plan.region, plan.required_bytes, plan.has_heap, plan.ops, plan.copies
                ),
            )
        })();
        if let Err(error) = result {
            eprintln!("JIT diagnostic capture: {error}");
            return None;
        }
        ctx.set_disasm(true);
        Some(Self(path))
    }

    pub(super) fn finish(self, ctx: &cranelift_codegen::Context) {
        let result = (|| -> std::io::Result<()> {
            fs::write(
                self.0.join("optimized.clif"),
                ctx.func.display().to_string(),
            )?;
            if let Some(code) = ctx.compiled_code() {
                fs::write(self.0.join("code.bin"), code.code_buffer())?;
                if let Some(disassembly) = &code.vcode {
                    fs::write(self.0.join("code.txt"), disassembly)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("JIT diagnostic capture: {error}");
        }
    }
}
