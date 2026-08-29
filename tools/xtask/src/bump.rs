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

use crate::cli::BumpKind;
use crate::git::GitRepository;
use crate::process::ProcessRunner;
use crate::state::ReleaseState;
use crate::transaction::FileTransaction;
use crate::version::ReleaseVersion;
use std::fs;
use std::path::Path;
use toml_edit::{value, DocumentMut};

const VERSION_PATHS: [&str; 3] = ["runtime-version.toml", "Cargo.toml", "Cargo.lock"];

pub fn bump(root: &Path, kind: BumpKind, runner: &dyn ProcessRunner) -> Result<String, String> {
    let git = GitRepository::open(root);
    git.require_clean()?;
    let current = ReleaseState::load(root)?;
    let target = match kind {
        BumpKind::Revision => {
            current.require_current_abi()?;
            current.version.bump_revision()?
        }
        BumpKind::Abi => current.version.bump_abi(current.exported_abi)?,
    };
    let paths = VERSION_PATHS.map(Path::new);
    let transaction = FileTransaction::begin(root, &paths)?;
    write_versions(root, target)?;
    runner.run(
        root,
        "cargo",
        &["metadata", "--format-version", "1", "--offline"],
        "regenerate Cargo.lock",
    )?;
    let resulting = ReleaseState::load(root)?;
    resulting.require_current_abi()?;
    if resulting.version != target {
        return Err(format!(
            "regenerated release version {} does not match target {target}",
            resulting.version
        ));
    }
    let message = format!("chore(release): bump version to {target}");
    git.commit(&message, &paths)?;
    transaction.commit();
    Ok(format!("prepared release version {target}"))
}

fn write_versions(root: &Path, version: ReleaseVersion) -> Result<(), String> {
    fs::write(
        root.join("runtime-version.toml"),
        format!("version = \"{version}\"\n"),
    )
    .map_err(|error| format!("cannot update runtime-version.toml: {error}"))?;

    let manifest_path = root.join("Cargo.toml");
    let contents = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("cannot read Cargo.toml: {error}"))?;
    let mut manifest = contents
        .parse::<DocumentMut>()
        .map_err(|error| format!("cannot parse Cargo.toml: {error}"))?;
    manifest["workspace"]["package"]["version"] = value(version.to_string());
    fs::write(&manifest_path, manifest.to_string())
        .map_err(|error| format!("cannot update Cargo.toml: {error}"))
}
