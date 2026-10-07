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
use crate::execution::checkpoint::{checkpoint_enum, checkpoint_struct, CheckpointError, Result};

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

impl Collector {
    pub(in crate::execution) fn validate_checkpoint(
        &self,
        heap: &Heap,
        image: &ExecutionImage,
        roots: RootSet<'_>,
        live: &[Ref32],
    ) -> Result<()> {
        let invalid = || CheckpointError::InvalidState;
        if (self.gray_head.is_some() != self.gray_tail.is_some())
            || matches!(self.phase, CollectorPhase::Idle | CollectorPhase::Sweep)
                && (self.gray_head.is_some() || self.scan.is_some())
            || self.is_active() && (self.epoch != 1 && self.epoch != 2)
        {
            return Err(invalid());
        }
        if self.is_active()
            && (self.runtime_root > roots.runtime_roots.len()
                || self.task_failure > roots.task_failures.len()
                || self.external_root > roots.external.len()
                || self.static_field > image.fields().len()
                || self.frame > roots.frame_depth
                || self.saved_frame > roots.saved_frames.len())
        {
            return Err(invalid());
        }
        if self.phase == CollectorPhase::Sweep {
            heap.checkpoint_sweep_boundary(self.sweep_offset, self.sweep_previous_size)?;
        }
        let mut cursor = self.gray_head;
        let mut last = None;
        let mut visited = std::collections::BTreeSet::new();
        while let Some(payload) = cursor {
            let reference = Ref32::managed(payload).ok_or_else(invalid)?;
            if live
                .binary_search_by_key(&reference.payload(), |value| value.payload())
                .is_err()
                || !visited.insert(payload)
            {
                return Err(invalid());
            }
            last = Some(payload);
            cursor = heap.checkpoint_gray_next(reference)?;
        }
        if last != self.gray_tail {
            return Err(invalid());
        }
        if let Some(scan) = self.scan {
            let reference = match scan {
                Scan::Object { reference, .. } | Scan::ReferenceArray { reference, .. } => {
                    reference
                }
            };
            if live
                .binary_search_by_key(&reference.payload(), |value| value.payload())
                .is_err()
                || visited.contains(&reference.payload())
            {
                return Err(invalid());
            }
            heap.checkpoint_gray_next(reference)?;
            let ty = image
                .type_key(heap.managed_type(reference).map_err(|_| invalid())? as usize)
                .ok_or_else(invalid)?;
            match (scan, image.type_layout(ty)) {
                (Scan::Object { next, .. }, Some(RuntimeTypeLayout::Object(layout)))
                    if next < layout.reference_offsets.len() => {}
                (
                    Scan::ReferenceArray { next, length, .. },
                    Some(RuntimeTypeLayout::Array {
                        element: ValueWidth::Ref,
                    }),
                ) if next < length => {
                    let bytes = heap.read_payload(reference, 0, 4).map_err(|_| invalid())?;
                    if u32::from_le_bytes(bytes[..4].try_into().map_err(|_| invalid())?) != length {
                        return Err(invalid());
                    }
                }
                _ => return Err(invalid()),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::fixtures;

    #[test]
    fn checkpoint_rejects_invalid_collector_queue_and_sweep_cursor() {
        let image =
            ExecutionImage::admit(fixtures::nested_call_artifact(), fixtures::profile()).unwrap();
        let heap = Heap::new(&image.storage_plan()).unwrap();
        let statics = StaticArena::new(image.static_layout().clone()).unwrap();
        let frame_arena = FrameArena::new(image.storage_plan().frame_arena_bytes as u32).unwrap();
        let external = ExternalRootTable::new(0).unwrap();
        let roots = RootSet {
            statics: &statics,
            frames: &[],
            saved_frames: &[],
            task_failures: &[],
            frame_arena: &frame_arena,
            frame_depth: 0,
            runtime_roots: &[],
            external: &external,
        };
        let mut collector = Collector::new();
        collector
            .validate_checkpoint(&heap, &image, roots, &[])
            .unwrap();
        collector.start();
        collector.gray_head = Some(8);
        collector.gray_tail = Some(8);
        assert_eq!(
            Err(CheckpointError::InvalidState),
            collector.validate_checkpoint(&heap, &image, roots, &[])
        );
        collector.gray_head = None;
        collector.gray_tail = None;
        collector.runtime_root = 1;
        assert_eq!(
            Err(CheckpointError::InvalidState),
            collector.validate_checkpoint(&heap, &image, roots, &[])
        );
        collector.runtime_root = 0;
        collector.phase = CollectorPhase::Sweep;
        collector.sweep_offset = 8;
        assert_eq!(
            Err(CheckpointError::InvalidState),
            collector.validate_checkpoint(&heap, &image, roots, &[])
        );
        collector.sweep_offset = 0;
        collector
            .validate_checkpoint(&heap, &image, roots, &[])
            .unwrap();
    }
}
