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
use crate::execution::checkpoint::{checkpoint_enum, checkpoint_struct, Result};

checkpoint_enum!(TaskWait {
    0 => Host(v0);
    1 => Join(v0);
    2 => Channel(v0);
});

checkpoint_enum!(TaskState {
    0 => Vacant;
    1 => Ready;
    2 => Running;
    3 => Waiting(v0);
    4 => Completed;
});

checkpoint_struct!(TaskRecord { id, state });

checkpoint_struct!(TaskScheduler {
    tasks,
    ready,
    ready_head,
    ready_len,
    active,
    next_id
});

impl TaskScheduler {
    pub(crate) fn validate_checkpoint(&self, capacity: usize) -> Result<()> {
        use crate::execution::checkpoint::CheckpointError::InvalidState;
        if capacity == 0
            || self.tasks.len() != capacity
            || self.ready.len() != capacity
            || self.ready_head >= capacity
            || self.ready_len > capacity
            || self.next_id < 2
            || self.next_id > i32::MAX as u32 + 1
        {
            return Err(InvalidState);
        }
        let mut ids = std::collections::BTreeSet::new();
        for (slot, task) in self.tasks.iter().enumerate() {
            match task.id {
                None if task.state == TaskState::Vacant => {}
                Some(id)
                    if id.get() != 0
                        && id.get() < self.next_id
                        && task.state != TaskState::Vacant
                        && ids.insert(id) => {}
                _ => return Err(InvalidState),
            }
            if (task.state == TaskState::Running) != (self.active == Some(slot)) {
                return Err(InvalidState);
            }
            if let TaskState::Waiting(TaskWait::Host(request)) = task.state {
                if request.get() == 0 {
                    return Err(InvalidState);
                }
            }
            if let TaskState::Waiting(TaskWait::Channel(channel)) = task.state {
                if channel == 0 {
                    return Err(InvalidState);
                }
            }
            if let TaskState::Waiting(TaskWait::Join(target)) = task.state {
                let mut cursor = target;
                for remaining in (0..capacity).rev() {
                    let target = self.slot(cursor).ok_or(InvalidState)?;
                    if target == slot || self.tasks[target].state == TaskState::Completed {
                        return Err(InvalidState);
                    }
                    let TaskState::Waiting(TaskWait::Join(next)) = self.tasks[target].state else {
                        break;
                    };
                    if remaining == 0 {
                        return Err(InvalidState);
                    }
                    cursor = next;
                }
            }
        }
        if self.active.is_some_and(|slot| slot >= capacity) {
            return Err(InvalidState);
        }
        let mut queued = std::collections::BTreeSet::new();
        for index in 0..self.ready_len {
            let slot = self.ready[(self.ready_head + index) % capacity];
            if slot >= capacity
                || self.tasks[slot].state != TaskState::Ready
                || !queued.insert(slot)
            {
                return Err(InvalidState);
            }
        }
        if self
            .tasks
            .iter()
            .filter(|task| task.state == TaskState::Ready)
            .count()
            != queued.len()
        {
            return Err(InvalidState);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::checkpoint::{Checkpoint, CheckpointError, Reader, Writer};

    fn round_trip(value: &TaskScheduler) -> TaskScheduler {
        let mut writer = Writer::new(4096);
        value.write(&mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 4096, 4096).unwrap();
        let result = TaskScheduler::read(&mut reader).unwrap();
        reader.finish().unwrap();
        result.validate_checkpoint(value.capacity()).unwrap();
        result
    }

    #[test]
    fn checkpoint_retains_host_completion_order_and_future_task_identity() {
        let mut original = TaskScheduler::new(4).unwrap();
        original.start_root().unwrap();
        let child = original.spawn().unwrap();
        let first = RequestId::new(7).unwrap();
        let second = RequestId::new(8).unwrap();
        original.suspend_host(first).unwrap();
        assert_eq!(Some(child), original.activate_next().unwrap());
        original.suspend_host(second).unwrap();
        original.complete_host(child, second).unwrap();
        original.complete_host(TaskId::ROOT, first).unwrap();
        let mut restored = round_trip(&original);
        for scheduler in [&mut original, &mut restored] {
            assert_eq!(Some(child), scheduler.activate_next().unwrap());
            assert_eq!(TaskId::new(3), Some(scheduler.spawn().unwrap()));
            scheduler.complete_current().unwrap();
            assert_eq!(Some(TaskId::ROOT), scheduler.activate_next().unwrap());
            scheduler.complete_current().unwrap();
            assert_eq!(TaskId::new(3), scheduler.activate_next().unwrap());
        }
    }

    #[test]
    fn checkpoint_retains_join_wakeup_without_restarting_root() {
        let mut scheduler = TaskScheduler::new(2).unwrap();
        scheduler.start_root().unwrap();
        let child = scheduler.spawn().unwrap();
        scheduler.join(child).unwrap();
        let mut restored = round_trip(&scheduler);
        assert_eq!(Some(child), restored.activate_next().unwrap());
        restored.complete_current().unwrap();
        assert_eq!(Some(TaskId::ROOT), restored.activate_next().unwrap());
        assert!(!restored.join(child).unwrap());
    }

    #[test]
    fn checkpoint_rejects_duplicate_queue_entries_and_cyclic_joins() {
        let mut scheduler = TaskScheduler::new(3).unwrap();
        scheduler.start_root().unwrap();
        let child = scheduler.spawn().unwrap();
        scheduler.spawn().unwrap();
        scheduler.ready[1] = scheduler.ready[0];
        assert_eq!(
            Err(CheckpointError::InvalidState),
            scheduler.validate_checkpoint(3)
        );
        scheduler.ready[1] = 2;
        scheduler.tasks[0].state = TaskState::Waiting(TaskWait::Join(child));
        scheduler.tasks[1].state = TaskState::Waiting(TaskWait::Join(TaskId::ROOT));
        scheduler.active = None;
        scheduler.ready_len = 1;
        scheduler.ready[0] = 2;
        assert_eq!(
            Err(CheckpointError::InvalidState),
            scheduler.validate_checkpoint(3)
        );
    }
}
