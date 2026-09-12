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

use super::host::TaskId;

const NONE: usize = usize::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChannelError {
    InvalidCapacity,
    InvalidHandle,
    NoCapacity,
    CorruptQueue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SendResult {
    Complete,
    WakeReceiver {
        task: TaskId,
        destination: u16,
        resume_block: usize,
    },
    Suspend,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReceiveResult {
    Value {
        value: i32,
        wake_sender: Option<(TaskId, usize)>,
    },
    Suspend,
}

#[derive(Clone, Copy, Debug)]
enum WaitKind {
    None,
    Send {
        value: i32,
        resume_block: usize,
    },
    Receive {
        destination: u16,
        resume_block: usize,
    },
}

#[derive(Clone, Copy, Debug)]
struct Waiter {
    task: Option<TaskId>,
    next: usize,
    kind: WaitKind,
}

impl Waiter {
    const EMPTY: Self = Self {
        task: None,
        next: NONE,
        kind: WaitKind::None,
    };
}

#[derive(Clone, Copy, Debug)]
struct Channel {
    values_start: usize,
    capacity: usize,
    head: usize,
    len: usize,
    send_head: usize,
    send_tail: usize,
    receive_head: usize,
    receive_tail: usize,
}

impl Channel {
    const EMPTY: Self = Self {
        values_start: 0,
        capacity: 0,
        head: 0,
        len: 0,
        send_head: NONE,
        send_tail: NONE,
        receive_head: NONE,
        receive_tail: NONE,
    };
}

pub(super) struct ChannelArena {
    channels: Box<[Channel]>,
    values: Box<[i32]>,
    waiters: Box<[Waiter]>,
    channel_count: usize,
    value_count: usize,
}

impl ChannelArena {
    pub(super) fn resident_bytes(channels: u64, values: u64, tasks: u64) -> Option<u64> {
        let tasks = if channels == 0 { 0 } else { tasks };
        let channel_bytes = (core::mem::size_of::<Channel>() as u64).checked_mul(channels);
        let value_bytes = (core::mem::size_of::<i32>() as u64).checked_mul(values);
        let waiter_bytes = (core::mem::size_of::<Waiter>() as u64).checked_mul(tasks);
        match (channel_bytes, value_bytes, waiter_bytes) {
            (Some(channel_bytes), Some(value_bytes), Some(waiter_bytes)) => channel_bytes
                .checked_add(value_bytes)
                .and_then(|bytes| bytes.checked_add(waiter_bytes)),
            _ => None,
        }
    }

    pub(super) fn new(
        channel_capacity: usize,
        value_capacity: usize,
        task_capacity: usize,
    ) -> Result<Self, ChannelError> {
        let task_capacity = if channel_capacity == 0 {
            0
        } else {
            task_capacity
        };
        let mut channels = Vec::new();
        channels
            .try_reserve_exact(channel_capacity)
            .map_err(|_| ChannelError::NoCapacity)?;
        channels.resize(channel_capacity, Channel::EMPTY);
        let mut values = Vec::new();
        values
            .try_reserve_exact(value_capacity)
            .map_err(|_| ChannelError::NoCapacity)?;
        values.resize(value_capacity, 0);
        let mut waiters = Vec::new();
        waiters
            .try_reserve_exact(task_capacity)
            .map_err(|_| ChannelError::NoCapacity)?;
        waiters.resize(task_capacity, Waiter::EMPTY);
        Ok(Self {
            channels: channels.into_boxed_slice(),
            values: values.into_boxed_slice(),
            waiters: waiters.into_boxed_slice(),
            channel_count: 0,
            value_count: 0,
        })
    }

    pub(super) fn create(&mut self, capacity: i32) -> Result<i32, ChannelError> {
        let capacity = usize::try_from(capacity).map_err(|_| ChannelError::InvalidCapacity)?;
        if capacity == 0 || self.channel_count == self.channels.len() {
            return Err(ChannelError::NoCapacity);
        }
        let end = self
            .value_count
            .checked_add(capacity)
            .filter(|end| *end <= self.values.len())
            .ok_or(ChannelError::NoCapacity)?;
        let slot = self.channel_count;
        self.channels[slot] = Channel {
            values_start: self.value_count,
            capacity,
            ..Channel::EMPTY
        };
        self.channel_count += 1;
        self.value_count = end;
        i32::try_from(slot + 1).map_err(|_| ChannelError::NoCapacity)
    }

    pub(super) fn send(
        &mut self,
        handle: i32,
        task_slot: usize,
        task: TaskId,
        value: i32,
        resume_block: usize,
    ) -> Result<SendResult, ChannelError> {
        let channel_index = self.channel_index(handle)?;
        if self.channels[channel_index].receive_head != NONE {
            let waiter = self.pop_waiter(channel_index, false)?;
            let WaitKind::Receive {
                destination,
                resume_block,
            } = waiter.kind
            else {
                return Err(ChannelError::CorruptQueue);
            };
            return Ok(SendResult::WakeReceiver {
                task: waiter.task.ok_or(ChannelError::CorruptQueue)?,
                destination,
                resume_block,
            });
        }
        let channel = &mut self.channels[channel_index];
        if channel.len < channel.capacity {
            let tail = (channel.head + channel.len) % channel.capacity;
            self.values[channel.values_start + tail] = value;
            channel.len += 1;
            return Ok(SendResult::Complete);
        }
        self.install_waiter(
            task_slot,
            task,
            WaitKind::Send {
                value,
                resume_block,
            },
        )?;
        self.push_waiter(channel_index, task_slot, true)?;
        Ok(SendResult::Suspend)
    }

    pub(super) fn receive(
        &mut self,
        handle: i32,
        task_slot: usize,
        task: TaskId,
        destination: u16,
        resume_block: usize,
    ) -> Result<ReceiveResult, ChannelError> {
        let channel_index = self.channel_index(handle)?;
        let channel = &mut self.channels[channel_index];
        if channel.len != 0 {
            let value = self.values[channel.values_start + channel.head];
            channel.head = (channel.head + 1) % channel.capacity;
            channel.len -= 1;
            let wake_sender = if channel.send_head == NONE {
                None
            } else {
                let waiter = self.pop_waiter(channel_index, true)?;
                let WaitKind::Send {
                    value,
                    resume_block,
                } = waiter.kind
                else {
                    return Err(ChannelError::CorruptQueue);
                };
                let channel = &mut self.channels[channel_index];
                let tail = (channel.head + channel.len) % channel.capacity;
                self.values[channel.values_start + tail] = value;
                channel.len += 1;
                Some((waiter.task.ok_or(ChannelError::CorruptQueue)?, resume_block))
            };
            return Ok(ReceiveResult::Value { value, wake_sender });
        }
        self.install_waiter(
            task_slot,
            task,
            WaitKind::Receive {
                destination,
                resume_block,
            },
        )?;
        self.push_waiter(channel_index, task_slot, false)?;
        Ok(ReceiveResult::Suspend)
    }

    pub(super) fn reserved_bytes(&self) -> usize {
        self.channels.len() * core::mem::size_of::<Channel>()
            + self.values.len() * core::mem::size_of::<i32>()
            + self.waiters.len() * core::mem::size_of::<Waiter>()
    }

    fn channel_index(&self, handle: i32) -> Result<usize, ChannelError> {
        usize::try_from(handle)
            .ok()
            .and_then(|handle| handle.checked_sub(1))
            .filter(|index| *index < self.channel_count)
            .ok_or(ChannelError::InvalidHandle)
    }

    fn install_waiter(
        &mut self,
        slot: usize,
        task: TaskId,
        kind: WaitKind,
    ) -> Result<(), ChannelError> {
        let waiter = self
            .waiters
            .get_mut(slot)
            .ok_or(ChannelError::CorruptQueue)?;
        if waiter.task.is_some() {
            return Err(ChannelError::CorruptQueue);
        }
        *waiter = Waiter {
            task: Some(task),
            next: NONE,
            kind,
        };
        Ok(())
    }

    fn push_waiter(
        &mut self,
        channel_index: usize,
        slot: usize,
        send: bool,
    ) -> Result<(), ChannelError> {
        let channel = &mut self.channels[channel_index];
        let (head, tail) = if send {
            (&mut channel.send_head, &mut channel.send_tail)
        } else {
            (&mut channel.receive_head, &mut channel.receive_tail)
        };
        if *tail == NONE {
            *head = slot;
        } else {
            self.waiters
                .get_mut(*tail)
                .ok_or(ChannelError::CorruptQueue)?
                .next = slot;
        }
        *tail = slot;
        Ok(())
    }

    fn pop_waiter(&mut self, channel_index: usize, send: bool) -> Result<Waiter, ChannelError> {
        let channel = &mut self.channels[channel_index];
        let (head, tail) = if send {
            (&mut channel.send_head, &mut channel.send_tail)
        } else {
            (&mut channel.receive_head, &mut channel.receive_tail)
        };
        let slot = *head;
        let waiter = *self.waiters.get(slot).ok_or(ChannelError::CorruptQueue)?;
        *head = waiter.next;
        if *head == NONE {
            *tail = NONE;
        }
        self.waiters[slot] = Waiter::EMPTY;
        Ok(waiter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_fifo_wakes_senders_in_registration_order() {
        let mut channels = ChannelArena::new(1, 2, 3).unwrap();
        let handle = channels.create(2).unwrap();
        assert_eq!(
            SendResult::Complete,
            channels.send(handle, 0, TaskId::ROOT, 11, 7).unwrap()
        );
        assert_eq!(
            SendResult::Complete,
            channels
                .send(handle, 1, TaskId::new(2).unwrap(), 22, 8)
                .unwrap()
        );
        assert_eq!(
            SendResult::Suspend,
            channels
                .send(handle, 2, TaskId::new(3).unwrap(), 33, 9)
                .unwrap()
        );
        assert_eq!(
            ReceiveResult::Value {
                value: 11,
                wake_sender: Some((TaskId::new(3).unwrap(), 9)),
            },
            channels.receive(handle, 0, TaskId::ROOT, 0, 10).unwrap()
        );
        assert_eq!(
            ReceiveResult::Value {
                value: 22,
                wake_sender: None,
            },
            channels.receive(handle, 0, TaskId::ROOT, 0, 10).unwrap()
        );
        assert_eq!(
            ReceiveResult::Value {
                value: 33,
                wake_sender: None,
            },
            channels.receive(handle, 0, TaskId::ROOT, 0, 10).unwrap()
        );
    }

    #[test]
    fn empty_receive_is_completed_directly_by_the_next_sender() {
        let mut channels = ChannelArena::new(1, 1, 2).unwrap();
        let handle = channels.create(1).unwrap();
        let receiver = TaskId::new(2).unwrap();
        assert_eq!(
            ReceiveResult::Suspend,
            channels.receive(handle, 1, receiver, 4, 12).unwrap()
        );
        assert_eq!(
            SendResult::WakeReceiver {
                task: receiver,
                destination: 4,
                resume_block: 12,
            },
            channels.send(handle, 0, TaskId::ROOT, 42, 13).unwrap()
        );
    }
}
