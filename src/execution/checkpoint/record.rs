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

checkpoint_struct!(HostRecordValue { type_name, fields });

checkpoint_struct!(HostRecordMember { name, value });

checkpoint_enum!(HostRecordScalar {
    0 => I32(v0);
    1 => I64(v0);
    2 => F32(v0);
    3 => F64(v0);
    4 => Bool(v0);
    5 => Char(v0);
    6 => String(v0);
    7 => Record(v0);
});

checkpoint_struct!(RecordLayout { ty, fields });

checkpoint_struct!(RecordFieldLayout {
    offset,
    width,
    record
});

checkpoint_enum!(PlannedValue {
    0 => Scalar(v0);
    1 => Node(v0);
});

checkpoint_enum!(PlannedNode {
    0 => String(v0);
    1 => Object { ty, fields };
});

checkpoint_struct!(PendingRecord {
    nodes,
    completed,
    index,
    field,
    object,
    allocation,
    string,
    collection_attempted
});
