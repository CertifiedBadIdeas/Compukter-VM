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
    checkpoint_enum, checkpoint_struct, Checkpoint, Reader, Result, Writer,
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
