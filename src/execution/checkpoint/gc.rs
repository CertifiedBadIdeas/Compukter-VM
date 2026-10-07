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

checkpoint_enum!(CollectorPhase {
    0 => Idle;
    1 => Roots;
    2 => Mark;
    3 => Sweep;
});

checkpoint_enum!(Scan {
    0 => Object { reference, next };
    1 => ReferenceArray { reference, next, length };
});

checkpoint_struct!(Collector { phase, epoch, runtime_root, task_failure, external_root, static_field, frame, saved_frame, register, gray_head, gray_tail, scan, sweep_offset, sweep_previous_size } defaults { #[cfg(test)] last_action: None });
