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

use compukter_vm::{ProcessArgumentLimits, ProcessCompletion, ProcessFailureReason, ProcessLimits};

#[test]
fn process_v2_failure_reasons_have_stable_negative_statuses() {
    let reasons = [
        ProcessFailureReason::InvalidPath,
        ProcessFailureReason::NotFound,
        ProcessFailureReason::AccessDenied,
        ProcessFailureReason::NotExecutable,
        ProcessFailureReason::InvalidProgram,
        ProcessFailureReason::Incompatible,
        ProcessFailureReason::LimitExceeded,
        ProcessFailureReason::Trapped,
        ProcessFailureReason::VmFault,
        ProcessFailureReason::HostFailure,
        ProcessFailureReason::IoFailure,
    ];

    for (index, reason) in reasons.into_iter().enumerate() {
        assert_eq!(reason.code(), (index + 1) as i32);
        assert_eq!(reason.status(), -((index + 1) as i32));
    }
}

#[test]
fn process_v2_completion_preserves_every_exit_code() {
    for code in 0_u8..=u8::MAX {
        assert_eq!(ProcessCompletion::Exited(code).status(), i32::from(code));
    }
    assert_eq!(
        ProcessCompletion::Failed {
            reason: ProcessFailureReason::NotFound,
            diagnostic: "/home/nope".encode_utf16().collect(),
        }
        .status(),
        -2,
    );
}

#[test]
fn process_v2_limits_reject_every_zero_bound() {
    let valid = [1, 1, 1, 1, 1];
    for zero in 0..valid.len() {
        let mut values = valid;
        values[zero] = 0;
        assert!(ProcessLimits::new(
            values[0] as u32,
            values[1] as u64,
            values[2] as u64,
            values[3] as u64,
            ProcessArgumentLimits::new(1, 1, 1).unwrap(),
            values[4],
        )
        .is_err());
    }
    assert!(ProcessArgumentLimits::new(0, 1, 1).is_err());
    assert!(ProcessArgumentLimits::new(1, 0, 1).is_err());
    assert!(ProcessArgumentLimits::new(1, 1, 0).is_err());
    assert!(ProcessLimits::new(
        3,
        16,
        3 * 64 * 1024,
        3 * 64 * 1024,
        ProcessArgumentLimits::new(8, 32, 128).unwrap(),
        96,
    )
    .is_ok());
}
