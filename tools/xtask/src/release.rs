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

use crate::git::GitRepository;
use crate::process::ProcessRunner;
use crate::state::ReleaseState;
use std::path::Path;

pub fn release(root: &Path, runner: &dyn ProcessRunner) -> Result<String, String> {
    let git = GitRepository::open(root);
    git.require_clean()?;
    let branch = git.branch()?;
    if branch != "main" {
        return Err(format!(
            "release requires the main branch, current branch is {branch}"
        ));
    }
    let head = git.head()?;
    let state = ReleaseState::load(root)?;
    state.require_current_abi()?;
    let tag = format!("v{}", state.version);
    if git.tag_exists(&tag)? {
        return Err(format!("release tag already exists: {tag}"));
    }

    runner.run(
        root,
        "cargo",
        &["fmt", "--all", "--check"],
        "format workspace",
    )?;
    runner.run(
        root,
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--offline",
            "--",
            "-D",
            "warnings",
        ],
        "lint workspace",
    )?;
    runner.run(
        root,
        "cargo",
        &["test", "--workspace", "--locked", "--offline"],
        "test workspace",
    )?;

    let final_branch = git.branch()?;
    if final_branch != branch {
        return Err(format!(
            "branch changed while running release gates: {branch} -> {final_branch}"
        ));
    }
    let final_head = git.head()?;
    if final_head != head {
        return Err(format!(
            "HEAD changed while running release gates: {head} -> {final_head}"
        ));
    }
    git.require_clean()?;
    let final_state = ReleaseState::load(root)?;
    final_state.require_current_abi()?;
    if final_state.version != state.version || final_state.exported_abi != state.exported_abi {
        return Err("release state changed while running release gates".to_owned());
    }
    if git.tag_exists(&tag)? {
        return Err(format!(
            "release tag appeared while running release gates: {tag}"
        ));
    }

    git.create_annotated_tag(&tag, &format!("Compukter-VM {}", state.version))?;
    Ok(format!("created local release tag {tag}"))
}
