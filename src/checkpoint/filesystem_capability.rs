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
use crate::execution::checkpoint::{
    checkpoint_struct, Checkpoint, CheckpointError, Reader, Result, Writer,
};
impl Checkpoint for FileRights {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.0.write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        let value = u16::read(reader)?;
        if value & !Self::OWNER.0 != 0 {
            return Err(CheckpointError::InvalidState);
        }
        Ok(Self(value))
    }
}
checkpoint_struct!(FileCapability {
    root,
    rights,
    logical_byte_limit,
    operation_limit,
    handle_limit
});
