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

use crate::{CapabilityBinding, HostValueType};

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
}

#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessResult {
    Exited = 0,
    InvalidCapabilities = 1,
    DepthLimit = 2,
    StartLimit = 3,
    InvalidPath = 4,
    NotFound = 5,
    PermissionDenied = 6,
    NotExecutable = 7,
    InvalidArtifact = 8,
    AdmissionFailed = 9,
    StartFailed = 10,
    AllocationExhausted = 11,
    QuotaExhausted = 12,
    Trapped = 13,
    Faulted = 14,
    HostFailed = 15,
    IoFailed = 16,
}

impl ProcessResult {
    pub const fn code(self) -> i32 {
        self as i32
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessContractError {
    InvalidCapabilities,
    InvalidLimits,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessCapabilityMask(u32);

impl ProcessCapabilityMask {
    pub const TERMINAL: u32 = 1 << 0;
    pub const FILESYSTEM: u32 = 1 << 1;
    pub const PROCESS: u32 = 1 << 2;
    pub const STANDARD: u32 = Self::TERMINAL | Self::FILESYSTEM | Self::PROCESS;

    pub fn new(requested: i32, available: u32) -> Result<Self, ProcessContractError> {
        let requested =
            u32::try_from(requested).map_err(|_| ProcessContractError::InvalidCapabilities)?;
        if requested & !available != 0 {
            return Err(ProcessContractError::InvalidCapabilities);
        }
        Ok(Self(requested))
    }

    pub(crate) const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn allows(self, requested: u32) -> bool {
        requested & !self.0 == 0
    }

    pub fn delegate(self, requested: i32) -> Result<Self, ProcessContractError> {
        Self::new(requested, self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessLimits {
    pub maximum_depth: u32,
    pub maximum_starts: u64,
    pub maximum_aggregate_heap_bytes: u64,
    pub maximum_aggregate_frame_storage_bytes: u64,
}

impl ProcessLimits {
    pub const fn new(
        maximum_depth: u32,
        maximum_starts: u64,
        maximum_aggregate_heap_bytes: u64,
        maximum_aggregate_frame_storage_bytes: u64,
    ) -> Result<Self, ProcessContractError> {
        if maximum_depth == 0
            || maximum_starts == 0
            || maximum_aggregate_heap_bytes == 0
            || maximum_aggregate_frame_storage_bytes == 0
        {
            return Err(ProcessContractError::InvalidLimits);
        }
        Ok(Self {
            maximum_depth,
            maximum_starts,
            maximum_aggregate_heap_bytes,
            maximum_aggregate_frame_storage_bytes,
        })
    }
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self::new(8, 4_096, 8 << 20, 8 << 20).expect("fixed process limits are valid")
    }
}
