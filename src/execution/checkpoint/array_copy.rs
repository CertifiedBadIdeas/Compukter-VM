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

checkpoint_struct!(PendingArrayCopy {
    source,
    destination,
    source_start,
    destination_start,
    remaining,
    width,
    backward
});

impl PendingArrayCopy {
    pub(in crate::execution) fn validate_checkpoint(
        &self,
        image: &super::super::image::ExecutionImage,
        heap: &Heap,
    ) -> crate::execution::checkpoint::Result<()> {
        use super::super::layout::RuntimeTypeLayout;
        use crate::execution::checkpoint::CheckpointError;
        for (reference, start) in [
            (self.source, self.source_start),
            (self.destination, self.destination_start),
        ] {
            let type_id = heap
                .managed_type(reference)
                .map_err(|_| CheckpointError::InvalidState)?;
            let ty = (0..image.type_count())
                .filter_map(|index| image.type_key(index))
                .find(|ty| image.type_id(*ty) == Some(type_id))
                .ok_or(CheckpointError::InvalidState)?;
            let Some(RuntimeTypeLayout::Array { element }) = image.type_layout(ty) else {
                return Err(CheckpointError::InvalidState);
            };
            let header = heap
                .read_payload(reference, 0, 8)
                .map_err(|_| CheckpointError::InvalidState)?;
            let length = u32::from_le_bytes(header[..4].try_into().unwrap());
            if element.bytes() != self.width
                || start
                    .checked_add(self.remaining)
                    .is_none_or(|end| end > length)
            {
                return Err(CheckpointError::InvalidState);
            }
        }
        if self.backward
            != (self.source == self.destination && self.destination_start > self.source_start)
        {
            return Err(CheckpointError::InvalidState);
        }
        Ok(())
    }
}
