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

use libloading::{Library, Symbol};
use std::fmt::{Display, Formatter};
use std::path::Path;

#[derive(Debug)]
pub struct SmokeError(String);

impl Display for SmokeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SmokeError {}

pub fn require_ffi_abi(path: &Path, expected: u32) -> Result<(), SmokeError> {
    type AbiVersion = unsafe extern "C" fn() -> u32;

    let library = unsafe { Library::new(path) }
        .map_err(|error| SmokeError(format!("failed to load {}: {error}", path.display())))?;
    let abi: Symbol<'_, AbiVersion> = unsafe { library.get(b"compukter_abi_version\0") }
        .map_err(|error| SmokeError(format!("missing compukter_abi_version: {error}")))?;
    let actual = unsafe { abi() };
    if actual != expected {
        return Err(SmokeError(format!(
            "exported FFI ABI {actual} does not match expected ABI {expected}"
        )));
    }
    Ok(())
}
