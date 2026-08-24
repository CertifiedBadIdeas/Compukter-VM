/*
 * The Compukters Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

use compukter_vm::{ProcessCapabilityMask, ProcessLimits, ProcessResult};

#[test]
fn process_results_have_stable_v1_codes() {
    let results = [
        ProcessResult::Exited,
        ProcessResult::InvalidCapabilities,
        ProcessResult::DepthLimit,
        ProcessResult::StartLimit,
        ProcessResult::InvalidPath,
        ProcessResult::NotFound,
        ProcessResult::PermissionDenied,
        ProcessResult::NotExecutable,
        ProcessResult::InvalidArtifact,
        ProcessResult::AdmissionFailed,
        ProcessResult::StartFailed,
        ProcessResult::AllocationExhausted,
        ProcessResult::QuotaExhausted,
        ProcessResult::Trapped,
        ProcessResult::Faulted,
        ProcessResult::HostFailed,
        ProcessResult::IoFailed,
    ];

    for (code, result) in results.into_iter().enumerate() {
        assert_eq!(result.code(), code as i32);
    }
}

#[test]
fn process_capability_masks_only_delegate_known_subsets() {
    let parent = ProcessCapabilityMask::new(0b111, 0b111).unwrap();

    assert_eq!(parent.delegate(0b011).unwrap().bits(), 0b011);
    assert!(parent.delegate(0b1000).is_err());
    assert!(parent.delegate(-1).is_err());
    assert!(ProcessCapabilityMask::new(0b1000, 0b111).is_err());
}

#[test]
fn process_limits_reject_zero_values() {
    assert!(ProcessLimits::new(0, 1, 1, 1).is_err());
    assert!(ProcessLimits::new(1, 0, 1, 1).is_err());
    assert!(ProcessLimits::new(1, 1, 0, 1).is_err());
    assert!(ProcessLimits::new(1, 1, 1, 0).is_err());
    assert!(ProcessLimits::new(3, 16, 3 * 64 * 1024, 3 * 64 * 1024).is_ok());
}
