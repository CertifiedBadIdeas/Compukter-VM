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

use crate::{CapabilityBinding, HostMergeSchema, HostValueType};

pub(crate) const MAXIMUM_ADDON_CAPABILITIES: usize = 28;

#[derive(Clone, Debug)]
pub(crate) struct OwnedCapabilityBinding {
    namespace: Box<str>,
    name: Box<str>,
    abi_major: u16,
    abi_minor: u16,
    operations: Box<[OwnedOperationSchema]>,
}

#[derive(Clone, Debug)]
pub(crate) struct OwnedOperationSchema {
    arguments: Box<[HostValueType]>,
    result: HostValueType,
    asynchronous: bool,
    merge: HostMergeSchema,
}

impl OwnedCapabilityBinding {
    pub(crate) fn copy_from(binding: &CapabilityBinding<'_>) -> Self {
        Self {
            namespace: binding.namespace().into(),
            name: binding.name().into(),
            abi_major: binding.abi_major(),
            abi_minor: binding.abi_minor(),
            operations: binding
                .operations()
                .iter()
                .map(|operation| OwnedOperationSchema {
                    arguments: operation.arguments.into(),
                    result: operation.result,
                    asynchronous: operation.asynchronous,
                    merge: operation.merge,
                })
                .collect(),
        }
    }

    pub(crate) fn namespace(&self) -> &str {
        &self.namespace
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) const fn abi_major(&self) -> u16 {
        self.abi_major
    }

    pub(crate) const fn abi_minor(&self) -> u16 {
        self.abi_minor
    }

    pub(crate) fn operations(&self) -> &[OwnedOperationSchema] {
        &self.operations
    }
}

impl OwnedOperationSchema {
    pub(crate) fn arguments(&self) -> &[HostValueType] {
        &self.arguments
    }

    pub(crate) const fn result(&self) -> HostValueType {
        self.result
    }

    pub(crate) const fn asynchronous(&self) -> bool {
        self.asynchronous
    }

    pub(crate) const fn merge(&self) -> HostMergeSchema {
        self.merge
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessCompletion {
    Exited(u8),
    Failed {
        reason: ProcessFailureReason,
        diagnostic: Box<[u16]>,
    },
}

impl ProcessCompletion {
    pub const fn status(&self) -> i32 {
        match self {
            Self::Exited(code) => *code as i32,
            Self::Failed { reason, .. } => reason.status(),
        }
    }
}

#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessFailureReason {
    InvalidPath = 1,
    NotFound = 2,
    AccessDenied = 3,
    NotExecutable = 4,
    InvalidProgram = 5,
    Incompatible = 6,
    LimitExceeded = 7,
    Trapped = 8,
    VmFault = 9,
    HostFailure = 10,
    IoFailure = 11,
}

impl ProcessFailureReason {
    pub const fn code(self) -> i32 {
        self as i32
    }

    pub const fn status(self) -> i32 {
        -self.code()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessContractError {
    InvalidLimits,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessArgumentLimits {
    pub maximum_count: u32,
    pub maximum_utf16_code_units: usize,
    pub maximum_total_utf16_code_units: usize,
}

impl ProcessArgumentLimits {
    pub const fn new(
        maximum_count: u32,
        maximum_utf16_code_units: usize,
        maximum_total_utf16_code_units: usize,
    ) -> Result<Self, ProcessContractError> {
        if maximum_count == 0
            || maximum_utf16_code_units == 0
            || maximum_total_utf16_code_units == 0
        {
            return Err(ProcessContractError::InvalidLimits);
        }
        Ok(Self {
            maximum_count,
            maximum_utf16_code_units,
            maximum_total_utf16_code_units,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessLimits {
    pub maximum_depth: u32,
    pub maximum_starts: u64,
    pub maximum_aggregate_heap_bytes: u64,
    pub maximum_aggregate_frame_storage_bytes: u64,
    pub arguments: ProcessArgumentLimits,
    pub maximum_diagnostic_utf16_code_units: usize,
}

impl ProcessLimits {
    pub const fn new(
        maximum_depth: u32,
        maximum_starts: u64,
        maximum_aggregate_heap_bytes: u64,
        maximum_aggregate_frame_storage_bytes: u64,
        arguments: ProcessArgumentLimits,
        maximum_diagnostic_utf16_code_units: usize,
    ) -> Result<Self, ProcessContractError> {
        if maximum_depth == 0
            || maximum_starts == 0
            || maximum_aggregate_heap_bytes == 0
            || maximum_aggregate_frame_storage_bytes == 0
            || maximum_diagnostic_utf16_code_units == 0
        {
            return Err(ProcessContractError::InvalidLimits);
        }
        Ok(Self {
            maximum_depth,
            maximum_starts,
            maximum_aggregate_heap_bytes,
            maximum_aggregate_frame_storage_bytes,
            arguments,
            maximum_diagnostic_utf16_code_units,
        })
    }
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self::new(
            8,
            4_096,
            8 << 20,
            8 << 20,
            ProcessArgumentLimits::new(256, 16_384, 65_536)
                .expect("fixed argument limits are valid"),
            4_096,
        )
        .expect("fixed process limits are valid")
    }
}
