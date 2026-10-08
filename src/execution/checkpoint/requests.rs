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

use super::*;
use crate::execution::checkpoint::{
    checkpoint_enum, checkpoint_struct, Checkpoint, CheckpointError, Reader, Result, Writer,
};

checkpoint_struct!(HostRequestIdentity { task, request });

impl Checkpoint for HostMergeGroup {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.0.write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Ok(Self(Checkpoint::read(reader)?))
    }
}

checkpoint_struct!(HostMergeEntry { key, value });

checkpoint_enum!(HostRequestMerge {
    0 => Ordinary;
    1 => LastWriteWins { group, entries };
});

checkpoint_struct!(PendingHostRequest {
    identity,
    capability,
    operation,
    arguments,
    utf16,
    merge
});

checkpoint_struct!(RequestTableLimits {
    maximum_requests,
    maximum_arguments_per_request,
    maximum_total_arguments,
    maximum_utf16_per_request,
    maximum_total_utf16,
    maximum_merge_entries_per_request,
    maximum_total_merge_entries
});

checkpoint_struct!(PendingRequestTable {
    limits,
    requests,
    total_arguments,
    total_utf16,
    total_merge_entries
});

impl PendingRequestTable {
    pub(crate) fn validate_checkpoint(&self, expected: &Self) -> Result<()> {
        if self.limits != expected.limits || self.requests.len() > self.limits.maximum_requests {
            return Err(CheckpointError::InvalidState);
        }
        let mut identities = std::collections::BTreeSet::new();
        let mut arguments = 0_usize;
        let mut utf16 = 0_usize;
        let mut merges = 0_usize;
        for request in &self.requests {
            let identity = request.identity;
            if identity.task.get() == 0
                || identity.request.get() == 0
                || !identities.insert((identity.task, identity.request))
                || request.arguments.len() > self.limits.maximum_arguments_per_request
                || request.utf16.len() > self.limits.maximum_utf16_per_request
                || request.merge_entry_count() > self.limits.maximum_merge_entries_per_request
                || request.arguments.iter().any(|slot| match slot {
                    HostValueSlot::String { start, length } => start
                        .checked_add(*length)
                        .is_none_or(|end| end as usize > request.utf16.len()),
                    _ => false,
                })
            {
                return Err(CheckpointError::InvalidState);
            }
            arguments = arguments
                .checked_add(request.arguments.len())
                .ok_or(CheckpointError::Limit)?;
            utf16 = utf16
                .checked_add(request.utf16.len())
                .ok_or(CheckpointError::Limit)?;
            merges = merges
                .checked_add(request.merge_entry_count())
                .ok_or(CheckpointError::Limit)?;
        }
        if arguments != self.total_arguments
            || arguments > self.limits.maximum_total_arguments
            || utf16 != self.total_utf16
            || utf16 > self.limits.maximum_total_utf16
            || merges != self.total_merge_entries
            || merges > self.limits.maximum_total_merge_entries
        {
            return Err(CheckpointError::InvalidState);
        }
        Ok(())
    }
}
