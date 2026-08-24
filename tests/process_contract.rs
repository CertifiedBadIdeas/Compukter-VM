/*
 * The Compukters Developers
 *
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
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
