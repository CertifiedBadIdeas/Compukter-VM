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

use super::host::{HostValueSlot, RequestId, TaskId};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct HostRequestIdentity {
    task: TaskId,
    request: RequestId,
}

impl HostRequestIdentity {
    pub(crate) const fn new(task: TaskId, request: RequestId) -> Self {
        Self { task, request }
    }

    pub(crate) const fn task(self) -> TaskId {
        self.task
    }

    pub(crate) const fn request(self) -> RequestId {
        self.request
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostMergeGroup(u32);

impl HostMergeGroup {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HostMergeEntry {
    key: u32,
    value: u32,
}

impl HostMergeEntry {
    pub const fn new(key: u32, value: u32) -> Self {
        Self { key, value }
    }

    pub const fn key(self) -> u32 {
        self.key
    }

    pub const fn value(self) -> u32 {
        self.value
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostRequestMerge {
    Ordinary,
    LastWriteWins {
        group: HostMergeGroup,
        entries: Box<[HostMergeEntry]>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingHostRequest {
    identity: HostRequestIdentity,
    capability: u32,
    operation: u32,
    arguments: Box<[HostValueSlot]>,
    utf16: Box<[u16]>,
    merge: HostRequestMerge,
}

impl PendingHostRequest {
    pub(crate) fn ordinary(
        identity: HostRequestIdentity,
        capability: u32,
        operation: u32,
        arguments: Box<[HostValueSlot]>,
        utf16: Box<[u16]>,
    ) -> Self {
        Self {
            identity,
            capability,
            operation,
            arguments,
            utf16,
            merge: HostRequestMerge::Ordinary,
        }
    }

    pub(crate) fn last_write_wins(
        identity: HostRequestIdentity,
        capability: u32,
        operation: u32,
        group: HostMergeGroup,
        entries: Box<[HostMergeEntry]>,
        arguments: Box<[HostValueSlot]>,
        utf16: Box<[u16]>,
    ) -> Self {
        Self {
            identity,
            capability,
            operation,
            arguments,
            utf16,
            merge: HostRequestMerge::LastWriteWins { group, entries },
        }
    }

    pub(crate) const fn identity(&self) -> HostRequestIdentity {
        self.identity
    }

    pub(crate) fn with_merge_group(mut self, group: HostMergeGroup) -> Self {
        if let HostRequestMerge::LastWriteWins { group: current, .. } = &mut self.merge {
            *current = group;
        }
        self
    }

    fn merge_entry_count(&self) -> usize {
        match &self.merge {
            HostRequestMerge::Ordinary => 0,
            HostRequestMerge::LastWriteWins { entries, .. } => entries.len(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RequestTableLimits {
    pub maximum_requests: usize,
    pub maximum_arguments_per_request: usize,
    pub maximum_total_arguments: usize,
    pub maximum_utf16_per_request: usize,
    pub maximum_total_utf16: usize,
    pub maximum_merge_entries_per_request: usize,
    pub maximum_total_merge_entries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestTableError {
    DuplicateIdentity,
    UnknownIdentity,
    RequestLimit,
    ArgumentsPerRequestLimit,
    TotalArgumentsLimit,
    Utf16PerRequestLimit,
    TotalUtf16Limit,
    MergeEntriesPerRequestLimit,
    TotalMergeEntriesLimit,
    NotMergeable,
    IncompatibleMergeGroup,
    EffectiveMergeEntryLimit,
}

#[derive(Debug)]
pub(crate) struct PendingRequestTable {
    limits: RequestTableLimits,
    requests: Vec<PendingHostRequest>,
    total_arguments: usize,
    total_utf16: usize,
    total_merge_entries: usize,
}

impl PendingRequestTable {
    pub(crate) fn new(limits: RequestTableLimits) -> Self {
        Self {
            requests: Vec::with_capacity(limits.maximum_requests),
            limits,
            total_arguments: 0,
            total_utf16: 0,
            total_merge_entries: 0,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    pub(crate) fn insert(&mut self, request: PendingHostRequest) -> Result<(), RequestTableError> {
        if self
            .requests
            .iter()
            .any(|pending| pending.identity == request.identity)
        {
            return Err(RequestTableError::DuplicateIdentity);
        }
        if self.requests.len() >= self.limits.maximum_requests {
            return Err(RequestTableError::RequestLimit);
        }
        if request.arguments.len() > self.limits.maximum_arguments_per_request {
            return Err(RequestTableError::ArgumentsPerRequestLimit);
        }
        let total_arguments = self
            .total_arguments
            .checked_add(request.arguments.len())
            .ok_or(RequestTableError::TotalArgumentsLimit)?;
        if total_arguments > self.limits.maximum_total_arguments {
            return Err(RequestTableError::TotalArgumentsLimit);
        }
        if request.utf16.len() > self.limits.maximum_utf16_per_request {
            return Err(RequestTableError::Utf16PerRequestLimit);
        }
        let total_utf16 = self
            .total_utf16
            .checked_add(request.utf16.len())
            .ok_or(RequestTableError::TotalUtf16Limit)?;
        if total_utf16 > self.limits.maximum_total_utf16 {
            return Err(RequestTableError::TotalUtf16Limit);
        }
        let merge_entries = request.merge_entry_count();
        if merge_entries > self.limits.maximum_merge_entries_per_request {
            return Err(RequestTableError::MergeEntriesPerRequestLimit);
        }
        let total_merge_entries = self
            .total_merge_entries
            .checked_add(merge_entries)
            .ok_or(RequestTableError::TotalMergeEntriesLimit)?;
        if total_merge_entries > self.limits.maximum_total_merge_entries {
            return Err(RequestTableError::TotalMergeEntriesLimit);
        }
        self.total_arguments = total_arguments;
        self.total_utf16 = total_utf16;
        self.total_merge_entries = total_merge_entries;
        self.requests.push(request);
        Ok(())
    }

    pub(crate) fn take(
        &mut self,
        identity: HostRequestIdentity,
    ) -> Result<PendingHostRequest, RequestTableError> {
        let index = self
            .requests
            .iter()
            .position(|request| request.identity == identity)
            .ok_or(RequestTableError::UnknownIdentity)?;
        let request = self.requests.remove(index);
        self.remove_totals(&request);
        Ok(request)
    }

    pub(crate) fn cancel_task(&mut self, task: TaskId) -> Box<[HostRequestIdentity]> {
        let mut cancelled = Vec::new();
        let mut index = 0;
        while index < self.requests.len() {
            if self.requests[index].identity.task == task {
                let request = self.requests.remove(index);
                cancelled.push(request.identity);
                self.remove_totals(&request);
            } else {
                index += 1;
            }
        }
        cancelled.into_boxed_slice()
    }

    fn remove_totals(&mut self, request: &PendingHostRequest) {
        self.total_arguments -= request.arguments.len();
        self.total_utf16 -= request.utf16.len();
        self.total_merge_entries -= request.merge_entry_count();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReducedHostRequests {
    requests: Box<[HostRequestIdentity]>,
    entries: Box<[HostMergeEntry]>,
}

impl ReducedHostRequests {
    pub(crate) fn requests(&self) -> &[HostRequestIdentity] {
        &self.requests
    }

    pub(crate) fn entries(&self) -> &[HostMergeEntry] {
        &self.entries
    }
}

pub(crate) fn reduce_last_write_wins(
    requests: &[PendingHostRequest],
    maximum_effective_entries: usize,
) -> Result<ReducedHostRequests, RequestTableError> {
    let mut group = None;
    let mut identities = Vec::with_capacity(requests.len());
    let mut entries = Vec::<HostMergeEntry>::new();
    for request in requests {
        let HostRequestMerge::LastWriteWins {
            group: request_group,
            entries: request_entries,
        } = &request.merge
        else {
            return Err(RequestTableError::NotMergeable);
        };
        if group.is_some_and(|group| group != *request_group) {
            return Err(RequestTableError::IncompatibleMergeGroup);
        }
        group = Some(*request_group);
        identities.push(request.identity);
        for entry in request_entries {
            if let Some(existing) = entries
                .iter_mut()
                .find(|existing| existing.key == entry.key)
            {
                *existing = *entry;
            } else {
                if entries.len() >= maximum_effective_entries {
                    return Err(RequestTableError::EffectiveMergeEntryLimit);
                }
                entries.push(*entry);
            }
        }
    }
    if group.is_none() {
        return Err(RequestTableError::NotMergeable);
    }
    entries.sort_unstable_by_key(|entry| entry.key);
    Ok(ReducedHostRequests {
        requests: identities.into_boxed_slice(),
        entries: entries.into_boxed_slice(),
    })
}
