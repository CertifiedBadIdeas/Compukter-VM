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
use crate::execution::checkpoint::{Checkpoint, CheckpointError, Reader, Result, Writer};
impl Checkpoint for VirtualPath {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.to_string().write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Self::parse_utf8(&String::read(reader)?, &FileSystemLimits::default())
            .map_err(|_| CheckpointError::InvalidState)
    }
}
