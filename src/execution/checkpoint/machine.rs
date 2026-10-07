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

checkpoint_enum!(Lifecycle {
    0 => Pristine;
    1 => Runnable;
    2 => Terminal(v0);
});

checkpoint_enum!(TypeInitializationState {
    0 => Uninitialized;
    1 => Initializing;
    2 => Initialized;
    3 => Failed;
});

checkpoint_struct!(Frame {
    function,
    base,
    byte_len,
    block,
    instruction,
    caller_block,
    caller_instruction,
    destination,
    initializer
});

checkpoint_struct!(FailureStack {
    frames,
    length,
    omitted
});

checkpoint_struct!(TaskFailure { exception, stack });

checkpoint_struct!(PendingException {
    failure,
    actual_type,
    next_handler,
    retires_instruction
});

checkpoint_struct!(PendingHostFailure {
    role,
    length,
    bytes
});

checkpoint_enum!(AllocationShape {
    0 => Object;
    1 => Exception;
    2 => Array { length };
});

checkpoint_struct!(AllocationRetry {
    request,
    destination,
    logical_bytes,
    shape,
    source
});

checkpoint_enum!(StringCollectionTarget {
    0 => Concat;
    1 => HostResponse;
    2 => RecordResponse;
    3 => ExceptionMessage;
});

checkpoint_struct!(PendingRaise {
    ty,
    units,
    length,
    text,
    message,
    exception,
    stack,
    retires_instruction
});
