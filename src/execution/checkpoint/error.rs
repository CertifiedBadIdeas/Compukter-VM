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
use crate::execution::checkpoint::{checkpoint_enum, checkpoint_struct};

checkpoint_enum!(GuestTrap {
    0 => DivisionByZero;
    1 => StackOverflow;
    2 => NegativeArraySize;
    3 => NullReference;
    4 => IndexOutOfBounds;
    5 => ClassCast;
    6 => InvalidExitCode;
    7 => InvalidArgument;
});

checkpoint_enum!(VmFault {
    0 => InvalidResolvedId;
    1 => InvalidValueType;
    2 => AccountingOverflow;
    3 => InvalidStoragePlan;
    4 => CorruptLifecycle;
    5 => ReachedUnreachable;
    6 => UnsupportedInstruction;
    7 => HandleExhausted;
    8 => CorruptHeap;
    9 => InvalidReference;
    10 => InvalidRootMap;
});

checkpoint_enum!(AllocationRequestKind {
    0 => Object;
    1 => Array;
    2 => String;
});

checkpoint_struct!(AllocationSource {
    module,
    function,
    block,
    instruction
});

checkpoint_struct!(AllocationDiagnostic {
    request_kind,
    requested,
    live,
    total_free,
    largest_free_block,
    source
});

checkpoint_struct!(AllocationExhaustion {
    exception,
    diagnostic,
    collection_attempted
});

checkpoint_enum!(Outcome {
    0 => SliceExhausted;
    1 => HostRequest;
    2 => TasksWaiting;
    3 => AllocationExhausted(v0);
    4 => Halted(v0);
    5 => Crashed(v0);
    6 => UncaughtException;
    7 => Faulted(v0);
});
