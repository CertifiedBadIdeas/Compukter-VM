/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */
//! Opt-in diagnostic instrumentation, excluded from performance binaries.
use super::*;
use crate::execution::image::ExecutionImage;

pub(in crate::execution) fn record_call(
    image: &ExecutionImage,
    block: usize,
    instruction: usize,
    target: usize,
) {
    let owner = image
        .code()
        .jit
        .get_or_init(|| Arc::new(JitState::default()));
    let mut calls = owner.calls.lock().unwrap_or_else(|e| e.into_inner());
    let key = (block, instruction, target);
    // A bounded overflow bucket makes truncation visible rather than silently
    // pretending the retained sites represent every invocation.
    let key = if calls.len() < 1024 || calls.contains_key(&key) {
        key
    } else {
        (usize::MAX, usize::MAX, usize::MAX)
    };
    *calls.entry(key).or_default() += 1;
}

pub(in crate::execution) fn report(image: &ExecutionImage) -> String {
    let Some(owner) = image.code().jit.get() else {
        return String::new();
    };
    let calls = owner.calls.lock().unwrap_or_else(|e| e.into_inner());
    let mut result = String::from("block\tinstruction\ttarget\tcount\tcallee\n");
    for (&(block, instruction, target), &count) in calls.iter() {
        use std::fmt::Write;
        let _ = writeln!(
            result,
            "{block}\t{instruction}\t{target}\t{count}\t{:?}",
            image.function(target).map(|f| (
                f.key,
                f.block_count,
                f.frame_layout.byte_len,
                f.static_owner,
                f.handlers.len()
            ))
        );
    }
    for target in calls
        .keys()
        .map(|&(_, _, target)| target)
        .collect::<std::collections::BTreeSet<_>>()
    {
        if let Some(f) = image.function(target) {
            use std::fmt::Write;
            for block in f.first_block..f.first_block + f.block_count {
                let info = image.block(block).unwrap();
                for pc in 0..info.instruction_count {
                    let _ = writeln!(
                        result,
                        "op\t{target}\t{block}\t{pc}\t{:?}\t{:?}",
                        image.code().operation(info.code_start + pc),
                        image.instruction(block, pc)
                    );
                }
            }
        }
    }
    result
}
