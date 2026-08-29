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

pub mod bump;
pub mod cli;
pub mod git;
pub mod process;
pub mod state;
pub mod transaction;
pub mod version;

use bump::bump;
use cli::Command;
use process::SystemProcessRunner;
use state::ReleaseState;
use std::path::PathBuf;

pub fn run<I, S>(arguments: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let command = Command::parse(arguments)?;
    match command {
        Command::Check => {
            let state = ReleaseState::load(&repository_root())?;
            state.require_current_abi()?;
            Ok(format!(
                "release state {} (ABI {}) is consistent",
                state.version, state.exported_abi
            ))
        }
        Command::Bump(kind) => bump(&repository_root(), kind, &SystemProcessRunner),
        Command::Release => Err(format!("{command:?} is not implemented")),
    }
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
