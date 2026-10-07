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

checkpoint_enum!(WaitKind {
    0 => None;
    1 => Send { value, resume_block };
    2 => Receive { destination, resume_block };
});

checkpoint_struct!(Waiter { task, next, kind });

checkpoint_struct!(Channel {
    values_start,
    capacity,
    head,
    len,
    send_head,
    send_tail,
    receive_head,
    receive_tail
});

checkpoint_struct!(ChannelArena {
    channels,
    values,
    waiters,
    channel_count,
    value_count
});

impl ChannelArena {
    pub(crate) fn validate_checkpoint(
        &self,
        channels: usize,
        values: usize,
        tasks: usize,
    ) -> Result<()> {
        use crate::execution::checkpoint::CheckpointError::InvalidState;
        let tasks = if channels == 0 { 0 } else { tasks };
        if self.channels.len() != channels
            || self.values.len() != values
            || self.waiters.len() != tasks
            || self.channel_count > channels
            || self.value_count > values
        {
            return Err(InvalidState);
        }
        let mut visited = std::collections::BTreeSet::new();
        let mut task_ids = std::collections::BTreeSet::new();
        let mut start = 0;
        for channel in &self.channels[..self.channel_count] {
            if channel.values_start != start
                || channel.capacity == 0
                || channel.head >= channel.capacity
                || channel.len > channel.capacity
            {
                return Err(InvalidState);
            }
            start = start
                .checked_add(channel.capacity)
                .filter(|end| *end <= values)
                .ok_or(InvalidState)?;
            for (send, head, tail) in [
                (true, channel.send_head, channel.send_tail),
                (false, channel.receive_head, channel.receive_tail),
            ] {
                if (head == NONE) != (tail == NONE)
                    || (head != NONE
                        && if send {
                            channel.len != channel.capacity
                        } else {
                            channel.len != 0
                        })
                {
                    return Err(InvalidState);
                }
                let mut current = head;
                let mut last = NONE;
                while current != NONE {
                    if !visited.insert(current) {
                        return Err(InvalidState);
                    }
                    let waiter = self.waiters.get(current).ok_or(InvalidState)?;
                    let task = waiter
                        .task
                        .filter(|task| task.get() != 0)
                        .ok_or(InvalidState)?;
                    if !task_ids.insert(task)
                        || !matches!(
                            (send, waiter.kind),
                            (true, WaitKind::Send { .. }) | (false, WaitKind::Receive { .. })
                        )
                    {
                        return Err(InvalidState);
                    }
                    last = current;
                    current = waiter.next;
                }
                if last != tail {
                    return Err(InvalidState);
                }
            }
        }
        if start != self.value_count {
            return Err(InvalidState);
        }
        for (slot, waiter) in self.waiters.iter().enumerate() {
            if waiter.task.is_some() != visited.contains(&slot)
                || (waiter.task.is_none()
                    && (!matches!(waiter.kind, WaitKind::None) || waiter.next != NONE))
            {
                return Err(InvalidState);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::checkpoint::{Checkpoint, CheckpointError, Reader, Writer};

    fn round_trip(value: &ChannelArena) -> ChannelArena {
        let mut writer = Writer::new(4096);
        value.write(&mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 4096, 4096).unwrap();
        let result = ChannelArena::read(&mut reader).unwrap();
        reader.finish().unwrap();
        result
            .validate_checkpoint(
                value.channels.len(),
                value.values.len(),
                value.waiters.len(),
            )
            .unwrap();
        result
    }

    #[test]
    fn checkpoint_retains_buffered_values_and_parked_sender_fifo() {
        let mut channels = ChannelArena::new(1, 1, 3).unwrap();
        let handle = channels.create(1).unwrap();
        let second = TaskId::new(2).unwrap();
        let third = TaskId::new(3).unwrap();
        channels.send(handle, 0, TaskId::ROOT, 11, 7).unwrap();
        assert_eq!(
            SendResult::Suspend,
            channels.send(handle, 1, second, 22, 8).unwrap()
        );
        assert_eq!(
            SendResult::Suspend,
            channels.send(handle, 2, third, 33, 9).unwrap()
        );
        let mut restored = round_trip(&channels);
        for expected in [
            ReceiveResult::Value {
                value: 11,
                wake_sender: Some((second, 8)),
            },
            ReceiveResult::Value {
                value: 22,
                wake_sender: Some((third, 9)),
            },
            ReceiveResult::Value {
                value: 33,
                wake_sender: None,
            },
        ] {
            assert_eq!(
                expected,
                restored.receive(handle, 0, TaskId::ROOT, 0, 10).unwrap()
            );
        }
    }

    #[test]
    fn checkpoint_retains_receiver_destination_and_continuation() {
        let mut channels = ChannelArena::new(1, 1, 2).unwrap();
        let handle = channels.create(1).unwrap();
        let receiver = TaskId::new(2).unwrap();
        channels.receive(handle, 1, receiver, 4, 12).unwrap();
        let mut restored = round_trip(&channels);
        assert_eq!(
            SendResult::WakeReceiver {
                task: receiver,
                destination: 4,
                resume_block: 12
            },
            restored.send(handle, 0, TaskId::ROOT, 42, 13).unwrap()
        );
    }

    #[test]
    fn checkpoint_rejects_waiter_cycles_and_overlapping_channel_storage() {
        let mut channels = ChannelArena::new(2, 2, 2).unwrap();
        let handle = channels.create(1).unwrap();
        channels.create(1).unwrap();
        channels.receive(handle, 0, TaskId::ROOT, 0, 1).unwrap();
        channels.waiters[0].next = 0;
        assert_eq!(
            Err(CheckpointError::InvalidState),
            channels.validate_checkpoint(2, 2, 2)
        );
        channels.waiters[0].next = NONE;
        channels.channels[1].values_start = 0;
        assert_eq!(
            Err(CheckpointError::InvalidState),
            channels.validate_checkpoint(2, 2, 2)
        );
    }
}
