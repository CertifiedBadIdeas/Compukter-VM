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

use crate::{RequestId, TaskId};

pub(crate) const SIDE_COUNT: u8 = 6;
pub(crate) const ALL_SIDES_MASK: u32 = 0x3f;
pub(crate) const REGISTER_MASK: u32 = 0x3fff_ffff;
const INPUT_LEVEL_MASK: u32 = 0x00ff_ffff;
const SIGNAL_MASK: u8 = 0x0f;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RedstoneError {
    ReservedBits,
    InvalidSide,
    InvalidSignal,
    DuplicateWaiter,
    WaiterLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputPredicate {
    Changed,
    Exact(u8),
    AtLeast(u8),
    AtMost(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RedstoneWaiter {
    task: TaskId,
    request: RequestId,
    side: u8,
    predicate: InputPredicate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RedstoneWakeup {
    pub task: TaskId,
    pub request: RequestId,
    pub level: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WaitRegistration {
    Ready(u8),
    Pending,
}

#[derive(Debug)]
pub(crate) struct RedstoneDevice {
    input_levels: u32,
    confirmed_output: u32,
    waiters: Vec<RedstoneWaiter>,
    maximum_waiters: usize,
}

impl RedstoneDevice {
    pub(crate) fn new(maximum_waiters: usize) -> Self {
        Self {
            input_levels: 0,
            confirmed_output: 0,
            waiters: Vec::new(),
            maximum_waiters,
        }
    }

    pub(crate) fn input(&self, side: u8) -> Result<u8, RedstoneError> {
        validate_side(side)?;
        Ok(((self.input_levels >> (u32::from(side) * 4)) & u32::from(SIGNAL_MASK)) as u8)
    }

    pub(crate) const fn confirmed_output(&self) -> u32 {
        self.confirmed_output
    }

    pub(crate) fn contains_waiter(&self, task: TaskId, request: RequestId) -> bool {
        self.waiters
            .iter()
            .any(|waiter| waiter.task == task && waiter.request == request)
    }

    pub(crate) fn confirm_output(&mut self, packed: u32) -> Result<(), RedstoneError> {
        validate_register(packed)?;
        self.confirmed_output = packed;
        Ok(())
    }

    pub(crate) fn submit_input(
        &mut self,
        packet: u32,
    ) -> Result<Vec<RedstoneWakeup>, RedstoneError> {
        validate_register(packet)?;
        let changed = packet & ALL_SIDES_MASK;
        self.input_levels = (packet >> 6) & INPUT_LEVEL_MASK;
        if changed == 0 || self.waiters.is_empty() {
            return Ok(Vec::new());
        }

        let mut ready = Vec::new();
        let mut pending = Vec::with_capacity(self.waiters.len());
        for waiter in std::mem::take(&mut self.waiters) {
            if changed & (1_u32 << waiter.side) == 0 {
                pending.push(waiter);
                continue;
            }
            let level = self.input(waiter.side)?;
            if predicate_matches(waiter.predicate, level) {
                ready.push(RedstoneWakeup {
                    task: waiter.task,
                    request: waiter.request,
                    level,
                });
            } else {
                pending.push(waiter);
            }
        }
        self.waiters = pending;
        ready.sort_by_key(|wakeup| (wakeup.task, wakeup.request));
        Ok(ready)
    }

    pub(crate) fn register_changed(
        &mut self,
        task: TaskId,
        request: RequestId,
        side: u8,
    ) -> Result<WaitRegistration, RedstoneError> {
        self.register(task, request, side, InputPredicate::Changed, false)
    }

    pub(crate) fn register_exact(
        &mut self,
        task: TaskId,
        request: RequestId,
        side: u8,
        signal: u8,
    ) -> Result<WaitRegistration, RedstoneError> {
        self.register(task, request, side, InputPredicate::Exact(signal), true)
    }

    pub(crate) fn register_at_least(
        &mut self,
        task: TaskId,
        request: RequestId,
        side: u8,
        signal: u8,
    ) -> Result<WaitRegistration, RedstoneError> {
        self.register(task, request, side, InputPredicate::AtLeast(signal), true)
    }

    pub(crate) fn register_at_most(
        &mut self,
        task: TaskId,
        request: RequestId,
        side: u8,
        signal: u8,
    ) -> Result<WaitRegistration, RedstoneError> {
        self.register(task, request, side, InputPredicate::AtMost(signal), true)
    }

    fn register(
        &mut self,
        task: TaskId,
        request: RequestId,
        side: u8,
        predicate: InputPredicate,
        level_triggered: bool,
    ) -> Result<WaitRegistration, RedstoneError> {
        validate_side(side)?;
        if let InputPredicate::Exact(signal)
        | InputPredicate::AtLeast(signal)
        | InputPredicate::AtMost(signal) = predicate
        {
            validate_signal(signal)?;
        }
        if self
            .waiters
            .iter()
            .any(|waiter| waiter.task == task && waiter.request == request)
        {
            return Err(RedstoneError::DuplicateWaiter);
        }
        let current = self.input(side)?;
        if level_triggered && predicate_matches(predicate, current) {
            return Ok(WaitRegistration::Ready(current));
        }
        if self.waiters.len() >= self.maximum_waiters {
            return Err(RedstoneError::WaiterLimit);
        }
        self.waiters.push(RedstoneWaiter {
            task,
            request,
            side,
            predicate,
        });
        Ok(WaitRegistration::Pending)
    }
}

fn validate_register(value: u32) -> Result<(), RedstoneError> {
    if value & !REGISTER_MASK == 0 {
        Ok(())
    } else {
        Err(RedstoneError::ReservedBits)
    }
}

fn validate_side(side: u8) -> Result<(), RedstoneError> {
    if side < SIDE_COUNT {
        Ok(())
    } else {
        Err(RedstoneError::InvalidSide)
    }
}

fn validate_signal(signal: u8) -> Result<(), RedstoneError> {
    if signal <= SIGNAL_MASK {
        Ok(())
    } else {
        Err(RedstoneError::InvalidSignal)
    }
}

fn predicate_matches(predicate: InputPredicate, level: u8) -> bool {
    match predicate {
        InputPredicate::Changed => true,
        InputPredicate::Exact(signal) => level == signal,
        InputPredicate::AtLeast(signal) => level >= signal,
        InputPredicate::AtMost(signal) => level <= signal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RequestId, TaskId};

    fn task(value: u32) -> TaskId {
        TaskId::new(value).unwrap()
    }

    fn request(value: u64) -> RequestId {
        RequestId::new(value).unwrap()
    }

    fn input_packet(changed: u32, levels: [u8; 6]) -> u32 {
        levels
            .iter()
            .enumerate()
            .fold(changed, |packet, (side, level)| {
                packet | (u32::from(*level) << (6 + side * 4))
            })
    }

    #[test]
    fn packet_updates_complete_snapshot_without_waiter_interest() {
        let mut device = RedstoneDevice::new(8);
        let packet = input_packet(0b001001, [7, 0, 0, 15, 0, 0]);
        let ready = device.submit_input(packet).unwrap();

        assert!(ready.is_empty());
        assert_eq!(7, device.input(0).unwrap());
        assert_eq!(15, device.input(3).unwrap());
    }

    #[test]
    fn edge_wait_ignores_current_state_and_unrelated_changes() {
        let mut device = RedstoneDevice::new(8);
        device
            .submit_input(input_packet(1, [7, 0, 0, 0, 0, 0]))
            .unwrap();
        device.register_changed(task(1), request(1), 0).unwrap();

        assert!(device
            .submit_input(input_packet(2, [7, 3, 0, 0, 0, 0]))
            .unwrap()
            .is_empty());
        assert_eq!(
            vec![RedstoneWakeup {
                task: task(1),
                request: request(1),
                level: 8,
            }],
            device
                .submit_input(input_packet(1, [8, 3, 0, 0, 0, 0]))
                .unwrap(),
        );
    }

    #[test]
    fn conditional_waits_are_level_triggered_and_ordered() {
        let mut device = RedstoneDevice::new(8);
        device
            .submit_input(input_packet(1, [7, 0, 0, 0, 0, 0]))
            .unwrap();

        assert_eq!(
            WaitRegistration::Ready(7),
            device.register_exact(task(3), request(3), 0, 7).unwrap(),
        );
        assert_eq!(
            WaitRegistration::Pending,
            device.register_at_least(task(2), request(2), 0, 8).unwrap(),
        );
        assert_eq!(
            WaitRegistration::Pending,
            device.register_at_least(task(1), request(1), 0, 8).unwrap(),
        );
        assert_eq!(
            WaitRegistration::Ready(7),
            device.register_at_most(task(4), request(4), 0, 7).unwrap(),
        );
        assert_eq!(
            vec![task(1), task(2)],
            device
                .submit_input(input_packet(1, [8, 0, 0, 0, 0, 0]))
                .unwrap()
                .into_iter()
                .map(|wakeup| wakeup.task)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn malformed_packets_waiters_and_output_are_rejected_without_mutation() {
        let mut device = RedstoneDevice::new(1);
        assert_eq!(
            Err(RedstoneError::ReservedBits),
            device.submit_input(1 << 30)
        );
        assert_eq!(Err(RedstoneError::InvalidSide), device.input(6));
        assert_eq!(
            WaitRegistration::Pending,
            device.register_exact(task(1), request(1), 0, 1).unwrap(),
        );
        assert_eq!(
            Err(RedstoneError::DuplicateWaiter),
            device.register_changed(task(1), request(1), 1),
        );
        assert_eq!(
            Err(RedstoneError::WaiterLimit),
            device.register_changed(task(2), request(2), 1),
        );
        assert_eq!(
            Err(RedstoneError::ReservedBits),
            device.confirm_output(1 << 31)
        );
        assert_eq!(0, device.confirmed_output());
    }
}
