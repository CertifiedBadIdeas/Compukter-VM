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

checkpoint_struct!(PendingState {
    request,
    reservation,
    destination,
    logical_bytes,
    initialized_bytes,
    fixed_cost_paid,
    collection_attempted
});

checkpoint_enum!(PendingAllocation {
    0 => Object(v0);
    1 => Exception(v0);
    2 => Array { state, length };
});

impl PendingAllocation {
    pub(in crate::execution) fn validate_checkpoint(
        self,
        image: &super::super::image::ExecutionImage,
        heap: &Heap,
    ) -> crate::execution::checkpoint::Result<u16> {
        use super::super::layout::{array_layout, RuntimeTypeLayout};
        use crate::execution::checkpoint::CheckpointError;
        let state = self.state();
        let ty = (0..image.type_count())
            .filter_map(|index| image.type_key(index))
            .find(|ty| image.type_id(*ty) == Some(state.request.type_id))
            .ok_or(CheckpointError::InvalidState)?;
        let (block_bytes, payload_bytes) = match (self, image.type_layout(ty)) {
            (Self::Object(_) | Self::Exception(_), Some(RuntimeTypeLayout::Object(layout))) => {
                (layout.block_bytes, layout.payload_bytes)
            }
            (Self::Array { length, .. }, Some(RuntimeTypeLayout::Array { element })) => {
                let layout = array_layout(*element, length as i32, heap.header_format())
                    .map_err(|_| CheckpointError::InvalidState)?;
                (layout.block_bytes, layout.payload_bytes)
            }
            _ => return Err(CheckpointError::InvalidState),
        };
        if !state.fixed_cost_paid
            || state.request.block_bytes != block_bytes
            || state.reservation.type_id != state.request.type_id
            || state.logical_bytes != payload_bytes
            || state.initialized_bytes > payload_bytes
            || heap.checkpoint_reservation_capacity(state.reservation)? < payload_bytes
        {
            return Err(CheckpointError::InvalidState);
        }
        Ok(state.destination)
    }
}
