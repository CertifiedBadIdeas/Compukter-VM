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

impl Checkpoint for TaskId {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.0.write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Ok(Self(Checkpoint::read(reader)?))
    }
}

impl Checkpoint for RequestId {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.0.write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Ok(Self(Checkpoint::read(reader)?))
    }
}

checkpoint_enum!(HostFailureKind {
    0 => EndOfFile;
    1 => Unavailable;
    2 => InputOutput;
    3 => Cancelled;
    4 => Other;
});

checkpoint_struct!(OwnedHostFailure { kind, detail });

checkpoint_enum!(QuotaKind {
    0 => HostRequestCodeUnits;
    1 => HostRequests;
    2 => AcceptedResponses;
});

checkpoint_struct!(QuotaExhaustion {
    kind,
    limit,
    consumed
});

checkpoint_enum!(HostValueSlot {
    0 => Empty;
    1 => I32(v0);
    2 => I64(v0);
    3 => F32(v0);
    4 => F64(v0);
    5 => Bool(v0);
    6 => Char(v0);
    7 => String { start, length };
});

checkpoint_struct!(ExecutionProfile {
    heap_bytes,
    frame_storage_bytes,
    maximum_call_depth,
    maximum_coroutines,
    maximum_channels,
    maximum_channel_values,
    maximum_host_requests,
    maximum_events,
    maximum_slice_budget,
    compiler_abi,
    platform_abi,
    maximum_host_arguments,
    maximum_outbound_utf16_code_units,
    maximum_inbound_utf16_code_units,
    maximum_accepted_responses,
    entry_argument_limits
});

checkpoint_struct!(EntryArgumentLimits {
    maximum_count,
    maximum_code_units_per_argument,
    maximum_total_code_units
});

checkpoint_enum!(HostValueType { 0 => Unit; 1 => I32; 2 => I64; 3 => F32; 4 => F64; 5 => Bool; 6 => Char; 7 => String; 8 => Record; });
checkpoint_enum!(HostMergeEntrySource { 0 => ArgumentPair { key, value }; 1 => PackedFields { argument, width, count }; });
checkpoint_enum!(HostMergeSchema { 0 => Ordinary; 1 => LastWriteWins { group, source }; });
checkpoint_struct!(ResolvedOperation {
    arguments,
    result,
    asynchronous,
    merge,
    result_record
});
checkpoint_struct!(ResolvedCapability {
    namespace,
    name,
    abi_major,
    abi_minor,
    operations
});
