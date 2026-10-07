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
use crate::execution::checkpoint::checkpoint_struct;
checkpoint_struct!(OwnedCapabilityBinding {
    namespace,
    name,
    abi_major,
    abi_minor,
    operations
});
checkpoint_struct!(OwnedOperationSchema {
    arguments,
    result,
    asynchronous,
    merge,
    result_record
});
checkpoint_struct!(ProcessArgumentLimits {
    maximum_count,
    maximum_utf16_code_units,
    maximum_total_utf16_code_units
});
checkpoint_struct!(ProcessLimits {
    maximum_depth,
    maximum_starts,
    maximum_aggregate_heap_bytes,
    maximum_aggregate_frame_storage_bytes,
    arguments,
    maximum_diagnostic_utf16_code_units
});
