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
use crate::artifact::ByteRange;
use crate::execution::checkpoint::{checkpoint_enum, checkpoint_struct};

checkpoint_enum!(StringBacking {
    0 => Inline { units, start, length };
    1 => Literal(v0);
    2 => TypeName { index, length };
    3 => Managed { reference, length, encoding };
    4 => CharArray { reference, length };
});

checkpoint_enum!(PendingText {
    0 => Hash { value, index, hash, destination };
    1 => Equals { lhs, rhs, index, destination };
    2 => Compare { lhs, rhs, index, destination };
});

checkpoint_struct!(PendingConcat {
    lhs,
    lhs_start,
    lhs_length,
    rhs,
    rhs_start,
    rhs_length,
    destination,
    scan,
    latin1,
    reservation,
    layout,
    written,
    collection_attempted
});

checkpoint_struct!(PendingHostString {
    destination,
    scan,
    latin1,
    reservation,
    layout,
    written,
    collection_attempted
});

checkpoint_struct!(ResolvedLiteral { bytes, code_units });
checkpoint_struct!(ByteRange { start, end });
