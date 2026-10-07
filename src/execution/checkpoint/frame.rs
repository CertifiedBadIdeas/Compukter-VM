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

checkpoint_struct!(ComponentLayout { offset, atom });

checkpoint_struct!(FrameLayout {
    byte_len,
    alignment,
    values
});

checkpoint_struct!(ValueLayout { components });

checkpoint_struct!(FrameReservation { base, byte_len });

checkpoint_struct!(FrameArena { bytes, free_head } defaults { #[cfg(test)] active_bytes: 0, #[cfg(test)] peak_active_bytes: 0, #[cfg(test)] initialized: vec![true; <Box<[u8]> as AsRef<[u8]>>::as_ref(&bytes).len()].into_boxed_slice() });

checkpoint_struct!(StaticArena {
    arena,
    layout,
    reservation
});

checkpoint_enum!(PhysicalAtom { 0 => I32; 1 => I64; 2 => F32; 3 => F64; 4 => Ref32; });
