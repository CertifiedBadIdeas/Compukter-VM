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
use crate::published::PublishedReleaseSource;
use crate::state::ReleaseState;
use crate::transaction::FileTransaction;
use crate::version::ReleaseVersion;
use std::fs;
use std::path::Path;
use toml_edit::{value, DocumentMut};

const VERSION_PATHS: [&str; 4] = [
    "runtime-version.toml",
    "Cargo.toml",
    "Cargo.lock",
    "ffi/src/lib.rs",
];

pub fn bump(
    root: &Path,
    kind: BumpKind,
    runner: &dyn ProcessRunner,
    published: &dyn PublishedReleaseSource,
) -> Result<String, String> {
    let git = GitRepository::open(root);
    git.require_clean()?;
    let current = ReleaseState::load(root)?;
    current.require_current_abi()?;
    let baseline = published.latest(root)?;
    let Some(target) = next_development_version(current.version, baseline, kind)? else {
        return Ok(format!(
            "development version {} is already prepared after published {baseline}",
            current.version
        ));
    };
    let paths = VERSION_PATHS.map(Path::new);
    let transaction = FileTransaction::begin(root, &paths)?;
    write_versions(root, target)?;
    if target.abi != current.exported_abi {
        let path = root.join("ffi/src/lib.rs");
        let source =
            fs::read_to_string(&path).map_err(|error| format!("cannot read FFI ABI: {error}"))?;
        let old = format!(
            "pub const COMPUKTER_FFI_ABI_VERSION: u32 = {};",
            current.exported_abi
        );
        let new = format!("pub const COMPUKTER_FFI_ABI_VERSION: u32 = {};", target.abi);
        fs::write(path, source.replacen(&old, &new, 1))
            .map_err(|error| format!("cannot update FFI ABI: {error}"))?;
    }
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

pub fn next_development_version(
    current: ReleaseVersion,
    published: ReleaseVersion,
    kind: BumpKind,
) -> Result<Option<ReleaseVersion>, String> {
    if current == published {
        return match kind {
            BumpKind::Revision => published.bump_revision().map(Some),
            BumpKind::Abi => published.bump_abi().map(Some),
        };
    }
    if current.abi == published.abi && current.revision.checked_sub(published.revision) == Some(1) {
        return match kind {
            BumpKind::Revision => Ok(None),
            BumpKind::Abi => published.bump_abi().map(Some),
        };
    }
    if published.abi.checked_add(1) == Some(current.abi) && current.revision == 0 {
        return Ok(None);
    }
    Err(format!(
        "development version {current} is not the next candidate after published {published}"
    ))
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
