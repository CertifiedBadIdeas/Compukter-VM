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

checkpoint_struct!(PreparingRequest {
    id,
    task,
    capability,
    operation,
    argument,
    string_offset
});

checkpoint_struct!(QueuedReply {
    task,
    request,
    value
});

checkpoint_enum!(CopiedReply {
    0 => Scalar(v0);
    1 => String(v0);
    2 => Record(v0);
    3 => Failure(v0);
});

checkpoint_enum!(SessionTerminal {
    0 => HostFailed(v0);
    1 => Faulted(v0);
    2 => QuotaExhausted(v0);
});
