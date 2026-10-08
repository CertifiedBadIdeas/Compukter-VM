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

checkpoint_enum!(HeaderFormat {
    0 => Legacy;
    1 => Compact { size_bits, link_bits };
});

checkpoint_enum!(ValueWidth {
    0 => I8;
    1 => I16;
    2 => U8;
    3 => U16;
    4 => Bool;
    5 => Char;
    6 => I32;
    7 => F32;
    8 => I64;
    9 => F64;
    10 => Ref;
});

checkpoint_struct!(StringLayout {
    encoding,
    length,
    payload_bytes,
    block_bytes
});

checkpoint_enum!(StringEncoding { 0 => Latin1; 1 => Utf16; });
