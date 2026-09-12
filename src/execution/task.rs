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

use super::host::{RequestId, TaskId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TaskWait {
    Host(RequestId),
    Join(TaskId),
    Channel(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TaskState {
    Vacant,
    Ready,
    Running,
    Waiting(TaskWait),
    Completed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TaskError {
    AlreadyStarted,
    NotStarted,
    NoCapacity,
    IdExhausted,
    NoRunningTask,
    UnknownTask,
    WrongWait,
    SelfJoin,
    JoinCycle,
    CorruptQueue,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct TaskSnapshot {
    pub capacity: u64,
    pub live: u64,
    pub runnable: u64,
    pub suspended: u64,
    pub completed: u64,
}

#[derive(Clone, Copy, Debug)]
struct TaskRecord {
    id: Option<TaskId>,
    state: TaskState,
}

impl TaskRecord {
    const EMPTY: Self = Self {
        id: None,
        state: TaskState::Vacant,
    };
}

pub(super) struct TaskScheduler {
    tasks: Box<[TaskRecord]>,
    ready: Box<[usize]>,
    ready_head: usize,
    ready_len: usize,
    active: Option<usize>,
    next_id: u32,
}

impl TaskScheduler {
    pub(super) const fn resident_bytes(capacity: u64) -> Option<u64> {
        let per_task =
            (core::mem::size_of::<TaskRecord>() + core::mem::size_of::<usize>() * 2) as u64;
        per_task.checked_mul(capacity)
    }

    pub(super) fn new(capacity: usize) -> Result<Self, TaskError> {
        if capacity == 0 {
            return Err(TaskError::NoCapacity);
        }
        let mut tasks = Vec::new();
        tasks
            .try_reserve_exact(capacity)
            .map_err(|_| TaskError::NoCapacity)?;
        tasks.resize(capacity, TaskRecord::EMPTY);
        let mut ready = Vec::new();
        ready
            .try_reserve_exact(capacity)
            .map_err(|_| TaskError::NoCapacity)?;
        ready.resize(capacity, usize::MAX);
        Ok(Self {
            tasks: tasks.into_boxed_slice(),
            ready: ready.into_boxed_slice(),
            ready_head: 0,
            ready_len: 0,
            active: None,
            next_id: TaskId::ROOT.get() + 1,
        })
    }

    pub(super) fn start_root(&mut self) -> Result<(), TaskError> {
        if self.active.is_some() || self.tasks[0].state != TaskState::Vacant {
            return Err(TaskError::AlreadyStarted);
        }
        self.tasks[0] = TaskRecord {
            id: Some(TaskId::ROOT),
            state: TaskState::Running,
        };
        self.active = Some(0);
        Ok(())
    }

    pub(super) fn spawn(&mut self) -> Result<TaskId, TaskError> {
        self.active.ok_or(TaskError::NoRunningTask)?;
        let slot = self
            .tasks
            .iter()
            .position(|task| task.state == TaskState::Vacant)
            .ok_or(TaskError::NoCapacity)?;
        if self.next_id > i32::MAX as u32 {
            return Err(TaskError::IdExhausted);
        }
        let id = TaskId::new(self.next_id).ok_or(TaskError::IdExhausted)?;
        self.next_id = self.next_id.checked_add(1).ok_or(TaskError::IdExhausted)?;
        self.tasks[slot] = TaskRecord {
            id: Some(id),
            state: TaskState::Ready,
        };
        self.enqueue(slot)?;
        Ok(id)
    }

    pub(super) fn current(&self) -> Result<TaskId, TaskError> {
        let slot = self.active.ok_or(TaskError::NoRunningTask)?;
        self.tasks[slot].id.ok_or(TaskError::NoRunningTask)
    }

    pub(super) fn suspend_host(&mut self, request: RequestId) -> Result<TaskId, TaskError> {
        self.suspend(TaskWait::Host(request))
    }

    pub(super) fn suspend_channel(&mut self, channel: u32) -> Result<TaskId, TaskError> {
        self.suspend(TaskWait::Channel(channel))
    }

    pub(super) fn join(&mut self, target: TaskId) -> Result<bool, TaskError> {
        let current = self.current()?;
        if current == target {
            return Err(TaskError::SelfJoin);
        }
        let target_slot = self.slot(target).ok_or(TaskError::UnknownTask)?;
        if self.tasks[target_slot].state == TaskState::Completed {
            return Ok(false);
        }
        let mut cursor = target;
        for _ in 0..self.tasks.len() {
            let slot = self.slot(cursor).ok_or(TaskError::UnknownTask)?;
            let TaskState::Waiting(TaskWait::Join(next)) = self.tasks[slot].state else {
                self.suspend(TaskWait::Join(target))?;
                return Ok(true);
            };
            if next == current {
                return Err(TaskError::JoinCycle);
            }
            cursor = next;
        }
        Err(TaskError::JoinCycle)
    }

    pub(super) fn complete_current(&mut self) -> Result<TaskId, TaskError> {
        let slot = self.active.take().ok_or(TaskError::NoRunningTask)?;
        let completed = self.tasks[slot].id.ok_or(TaskError::NoRunningTask)?;
        self.tasks[slot].state = TaskState::Completed;
        for waiter in 0..self.tasks.len() {
            if self.tasks[waiter].state == TaskState::Waiting(TaskWait::Join(completed)) {
                self.tasks[waiter].state = TaskState::Ready;
                self.enqueue(waiter)?;
            }
        }
        Ok(completed)
    }

    pub(super) fn complete_host(
        &mut self,
        task: TaskId,
        request: RequestId,
    ) -> Result<(), TaskError> {
        let slot = self.slot(task).ok_or(TaskError::UnknownTask)?;
        if self.tasks[slot].state != TaskState::Waiting(TaskWait::Host(request)) {
            return Err(TaskError::WrongWait);
        }
        self.tasks[slot].state = TaskState::Ready;
        self.enqueue(slot)
    }

    pub(super) fn complete_channel(&mut self, task: TaskId, channel: u32) -> Result<(), TaskError> {
        let slot = self.slot(task).ok_or(TaskError::UnknownTask)?;
        if self.tasks[slot].state != TaskState::Waiting(TaskWait::Channel(channel)) {
            return Err(TaskError::WrongWait);
        }
        self.tasks[slot].state = TaskState::Ready;
        self.enqueue(slot)
    }

    pub(super) fn activate_next(&mut self) -> Result<Option<TaskId>, TaskError> {
        if self.active.is_some() {
            return Err(TaskError::NoRunningTask);
        }
        if self.ready_len == 0 {
            return Ok(None);
        }
        let slot = self.ready[self.ready_head];
        self.ready[self.ready_head] = usize::MAX;
        self.ready_head = (self.ready_head + 1) % self.ready.len();
        self.ready_len -= 1;
        let task = self.tasks.get_mut(slot).ok_or(TaskError::CorruptQueue)?;
        if task.state != TaskState::Ready {
            return Err(TaskError::CorruptQueue);
        }
        task.state = TaskState::Running;
        self.active = Some(slot);
        Ok(task.id)
    }

    pub(super) fn state(&self, task: TaskId) -> Option<TaskState> {
        self.slot(task).map(|slot| self.tasks[slot].state)
    }

    pub(super) fn capacity(&self) -> usize {
        self.tasks.len()
    }

    pub(super) fn snapshot(&self) -> TaskSnapshot {
        let mut snapshot = TaskSnapshot {
            capacity: self.tasks.len() as u64,
            ..TaskSnapshot::default()
        };
        for task in &self.tasks {
            match task.state {
                TaskState::Vacant => {}
                TaskState::Ready | TaskState::Running => {
                    snapshot.live += 1;
                    snapshot.runnable += 1;
                }
                TaskState::Waiting(_) => {
                    snapshot.live += 1;
                    snapshot.suspended += 1;
                }
                TaskState::Completed => snapshot.completed += 1,
            }
        }
        snapshot
    }

    pub(super) fn cancel_all(&mut self) {
        self.tasks.fill(TaskRecord::EMPTY);
        self.ready.fill(usize::MAX);
        self.ready_head = 0;
        self.ready_len = 0;
        self.active = None;
    }

    pub(super) fn reserved_bytes(&self) -> usize {
        self.tasks.len() * core::mem::size_of::<TaskRecord>()
            + self.ready.len() * core::mem::size_of::<usize>()
    }

    pub(super) fn slot_of(&self, id: TaskId) -> Option<usize> {
        self.slot(id)
    }

    fn suspend(&mut self, wait: TaskWait) -> Result<TaskId, TaskError> {
        let slot = self.active.take().ok_or(TaskError::NoRunningTask)?;
        let id = self.tasks[slot].id.ok_or(TaskError::NoRunningTask)?;
        self.tasks[slot].state = TaskState::Waiting(wait);
        Ok(id)
    }

    fn slot(&self, id: TaskId) -> Option<usize> {
        self.tasks.iter().position(|task| task.id == Some(id))
    }

    fn enqueue(&mut self, slot: usize) -> Result<(), TaskError> {
        if self.ready_len == self.ready.len() {
            return Err(TaskError::CorruptQueue);
        }
        let tail = (self.ready_head + self.ready_len) % self.ready.len();
        self.ready[tail] = slot;
        self.ready_len += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(value: u64) -> RequestId {
        RequestId::new(value).unwrap()
    }

    #[test]
    fn host_waits_wake_in_explicit_completion_order() {
        let mut scheduler = TaskScheduler::new(3).unwrap();
        scheduler.start_root().unwrap();
        let second = scheduler.spawn().unwrap();
        let third = scheduler.spawn().unwrap();
        scheduler.suspend_host(request(1)).unwrap();
        assert_eq!(Some(second), scheduler.activate_next().unwrap());
        scheduler.suspend_host(request(2)).unwrap();
        assert_eq!(Some(third), scheduler.activate_next().unwrap());
        scheduler.suspend_host(request(3)).unwrap();

        scheduler.complete_host(third, request(3)).unwrap();
        scheduler.complete_host(TaskId::ROOT, request(1)).unwrap();
        assert_eq!(Some(third), scheduler.activate_next().unwrap());
        scheduler.suspend_host(request(4)).unwrap();
        assert_eq!(Some(TaskId::ROOT), scheduler.activate_next().unwrap());
    }

    #[test]
    fn completion_wakes_joiners_fifo_and_completed_join_does_not_suspend() {
        let mut scheduler = TaskScheduler::new(3).unwrap();
        scheduler.start_root().unwrap();
        let child = scheduler.spawn().unwrap();
        assert!(scheduler.join(child).unwrap());
        assert_eq!(Some(child), scheduler.activate_next().unwrap());
        assert_eq!(child, scheduler.complete_current().unwrap());
        assert_eq!(Some(TaskId::ROOT), scheduler.activate_next().unwrap());
        assert!(!scheduler.join(child).unwrap());
    }

    #[test]
    fn scheduler_rejects_capacity_self_join_cycles_and_wrong_completions() {
        let mut scheduler = TaskScheduler::new(2).unwrap();
        scheduler.start_root().unwrap();
        let child = scheduler.spawn().unwrap();
        assert_eq!(Err(TaskError::NoCapacity), scheduler.spawn());
        assert_eq!(Err(TaskError::SelfJoin), scheduler.join(TaskId::ROOT));
        assert!(scheduler.join(child).unwrap());
        assert_eq!(Some(child), scheduler.activate_next().unwrap());
        assert_eq!(Err(TaskError::JoinCycle), scheduler.join(TaskId::ROOT));
        scheduler.suspend_host(request(7)).unwrap();
        assert_eq!(
            Err(TaskError::WrongWait),
            scheduler.complete_host(child, request(8)),
        );
        assert_eq!(
            Some(TaskState::Waiting(TaskWait::Host(request(7)))),
            scheduler.state(child)
        );
    }
}
