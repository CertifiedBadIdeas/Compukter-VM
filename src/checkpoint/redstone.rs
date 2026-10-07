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
checkpoint_enum!(InputPredicate { 0 => Changed; 1 => Exact(v0); 2 => AtLeast(v0); 3 => AtMost(v0); });
checkpoint_struct!(RedstoneWaiter {
    task,
    request,
    side,
    predicate
});
checkpoint_struct!(RedstoneDevice {
    input_levels,
    confirmed_output,
    waiters,
    maximum_waiters
});
impl RedstoneDevice {
    pub(crate) fn validate_checkpoint(&self, expected: &Self) -> Result<()> {
        let mut identities = std::collections::BTreeSet::new();
        if self.maximum_waiters != expected.maximum_waiters
            || self.waiters.len() > self.maximum_waiters
            || self.input_levels & !INPUT_LEVEL_MASK != 0
            || validate_register(self.confirmed_output).is_err()
            || self.waiters.iter().any(|waiter| {
                waiter.task.get() == 0
                    || waiter.request.get() == 0
                    || !identities.insert((waiter.task, waiter.request))
                    || validate_side(waiter.side).is_err()
                    || match waiter.predicate {
                        InputPredicate::Changed => false,
                        InputPredicate::Exact(value)
                        | InputPredicate::AtLeast(value)
                        | InputPredicate::AtMost(value) => validate_signal(value).is_err(),
                    }
            })
        {
            return Err(CheckpointError::InvalidState);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::checkpoint::{Checkpoint, Reader, Writer};
    #[test]
    fn checkpoint_retains_redstone_wait_order_and_confirmed_outputs() {
        let mut original = RedstoneDevice::new(4);
        original.confirm_output(123).unwrap();
        original
            .register_changed(TaskId::ROOT, RequestId::new(7).unwrap(), 2)
            .unwrap();
        original
            .register_exact(TaskId::new(2).unwrap(), RequestId::new(8).unwrap(), 2, 10)
            .unwrap();
        let mut writer = Writer::new(4096);
        original.write(&mut writer).unwrap();
        let bytes = writer.finish();
        let mut reader = Reader::new(&bytes, 4096, 4096).unwrap();
        let mut restored = RedstoneDevice::read(&mut reader).unwrap();
        reader.finish().unwrap();
        restored
            .validate_checkpoint(&RedstoneDevice::new(4))
            .unwrap();
        assert_eq!(123, restored.confirmed_output());
        let packet = (1 << 2) | (10 << (6 + 2 * 4));
        let expected = original.submit_input(packet).unwrap();
        let actual = restored.submit_input(packet).unwrap();
        assert_eq!(expected, actual);
        assert_eq!(2, actual.len());
        assert!(restored.submit_input(packet).unwrap().is_empty());
    }
}
