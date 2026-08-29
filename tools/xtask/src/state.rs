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

use crate::version::ReleaseVersion;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;

const ABI_PREFIX: &str = "pub const COMPUKTER_FFI_ABI_VERSION: u32 = ";

#[derive(Debug)]
pub struct ReleaseState {
    pub version: ReleaseVersion,
    pub exported_abi: u32,
}

impl ReleaseState {
    pub fn load(root: &Path) -> Result<Self, String> {
        let version = read_runtime_version(&root.join("runtime-version.toml"))?;
        let root_manifest = read_document(&root.join("Cargo.toml"), "workspace manifest")?;
        let mut errors = Vec::new();

        match root_manifest["workspace"]["package"]["version"].as_str() {
            Some(workspace_version) if workspace_version == version.to_string() => {}
            Some(workspace_version) => errors.push(format!(
                "workspace version {workspace_version} does not match runtime version {version}"
            )),
            None => errors.push("workspace package version is missing".to_owned()),
        }

        let members = workspace_members(&root_manifest)?;
        let mut package_names = Vec::new();
        for member in members {
            let manifest = if member == Path::new(".") {
                &root_manifest
            } else {
                let path = root.join(&member).join("Cargo.toml");
                let document = read_document(&path, "workspace member manifest")?;
                package_names.push(validate_member(&document, &member, &mut errors)?);
                continue;
            };
            package_names.push(validate_member(manifest, &member, &mut errors)?);
        }
        validate_lock(root, &package_names, version, &mut errors)?;

        let exported_abi = read_exported_abi(&root.join("ffi/src/lib.rs"))?;
        validate_workflow(root, version, &mut errors)?;
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }
        Ok(Self {
            version,
            exported_abi,
        })
    }

    pub fn require_current_abi(&self) -> Result<(), String> {
        if self.version.abi == self.exported_abi {
            Ok(())
        } else {
            Err(format!(
                "runtime ABI {} does not match exported FFI ABI {}",
                self.version.abi, self.exported_abi
            ))
        }
    }
}

fn read_runtime_version(path: &Path) -> Result<ReleaseVersion, String> {
    let contents = read(path, "runtime version")?;
    let line = contents
        .strip_suffix('\n')
        .ok_or_else(|| "runtime-version.toml must end with one LF".to_owned())?;
    let value = line
        .strip_prefix("version = \"")
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| {
            "runtime-version.toml must contain only version = \"0.<abi>.<revision>\"".to_owned()
        })?;
    ReleaseVersion::parse(value)
}

fn read_document(path: &Path, purpose: &str) -> Result<DocumentMut, String> {
    read(path, purpose)?
        .parse::<DocumentMut>()
        .map_err(|error| format!("cannot parse {purpose} {}: {error}", path.display()))
}

fn read(path: &Path, purpose: &str) -> Result<String, String> {
    fs::read_to_string(path)
        .map_err(|error| format!("cannot read {purpose} {}: {error}", path.display()))
}

fn workspace_members(document: &DocumentMut) -> Result<Vec<PathBuf>, String> {
    document["workspace"]["members"]
        .as_array()
        .ok_or_else(|| "workspace members are missing".to_owned())?
        .iter()
        .map(|member| {
            member
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| "workspace member must be a string".to_owned())
        })
        .collect()
}

fn validate_member(
    document: &DocumentMut,
    path: &Path,
    errors: &mut Vec<String>,
) -> Result<String, String> {
    let name = document["package"]["name"]
        .as_str()
        .ok_or_else(|| format!("workspace member {} has no package name", path.display()))?
        .to_owned();
    if document["package"]["version"]["workspace"].as_bool() != Some(true) {
        errors.push(format!(
            "workspace member {name} must use version.workspace = true"
        ));
    }
    Ok(name)
}

fn validate_lock(
    root: &Path,
    package_names: &[String],
    version: ReleaseVersion,
    errors: &mut Vec<String>,
) -> Result<(), String> {
    let lock = read_document(&root.join("Cargo.lock"), "Cargo.lock")?;
    let packages = lock["package"]
        .as_array_of_tables()
        .ok_or_else(|| "Cargo.lock packages are missing".to_owned())?;
    for name in package_names {
        let locked = packages
            .iter()
            .find(|package| package["name"].as_str() == Some(name));
        match locked.and_then(|package| package["version"].as_str()) {
            Some(locked_version) if locked_version == version.to_string() => {}
            Some(locked_version) => errors.push(format!(
                "Cargo.lock workspace package {name} has version {locked_version}, expected {version}"
            )),
            None => errors.push(format!("Cargo.lock has no workspace package {name}")),
        }
    }
    Ok(())
}

fn read_exported_abi(path: &Path) -> Result<u32, String> {
    let contents = read(path, "FFI ABI source")?;
    let values = contents
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix(ABI_PREFIX)
                .and_then(|value| value.strip_suffix(';'))
        })
        .collect::<Vec<_>>();
    if values.len() != 1 {
        return Err("FFI ABI source must contain exactly one exported ABI constant".to_owned());
    }
    values[0]
        .parse()
        .map_err(|_| "exported FFI ABI must be u32".to_owned())
}

fn validate_workflow(
    root: &Path,
    version: ReleaseVersion,
    errors: &mut Vec<String>,
) -> Result<(), String> {
    let workflow = read(
        &root.join(".github/workflows/runtime-release.yml"),
        "Runtime release workflow",
    )?;
    if workflow.contains("runtime-v0") {
        errors.push("Runtime release workflow uses the legacy runtime-v tag prefix".to_owned());
    }
    if workflow.contains(&format!("compukter-runtime-{version}")) {
        errors.push(format!(
            "Runtime release workflow contains hard-coded runtime version {version}"
        ));
    }
    for marker in [
        "v0.*.*",
        "RUNTIME_TAG",
        "RUNTIME_VERSION",
        "compukter-runtime-${RUNTIME_VERSION}",
    ] {
        if !workflow.contains(marker) {
            errors.push(format!(
                "Runtime release workflow is missing dynamic marker {marker}"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ReleaseState;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    struct Fixture {
        directory: TempDir,
    }

    impl Fixture {
        fn consistent(version: &str, abi: u32) -> Self {
            let directory = tempfile::tempdir().unwrap();
            write(
                directory.path().join("Cargo.toml"),
                &format!(
                    "[workspace]\nmembers = [\".\", \"ffi\"]\nresolver = \"2\"\n\n[workspace.package]\nversion = \"{version}\"\n\n[package]\nname = \"vm\"\nversion.workspace = true\nedition = \"2021\"\n"
                ),
            );
            write(
                directory.path().join("ffi/Cargo.toml"),
                "[package]\nname = \"ffi\"\nversion.workspace = true\nedition = \"2021\"\n",
            );
            write(
                directory.path().join("Cargo.lock"),
                &format!(
                    "version = 4\n\n[[package]]\nname = \"ffi\"\nversion = \"{version}\"\n\n[[package]]\nname = \"vm\"\nversion = \"{version}\"\n"
                ),
            );
            write(
                directory.path().join("runtime-version.toml"),
                &format!("version = \"{version}\"\n"),
            );
            write(
                directory.path().join("ffi/src/lib.rs"),
                &format!("pub const COMPUKTER_FFI_ABI_VERSION: u32 = {abi};\n"),
            );
            write(
                directory
                    .path()
                    .join(".github/workflows/runtime-release.yml"),
                "tags:\n  - \"v0.*.*\"\nenv:\n  RUNTIME_TAG: input\n  RUNTIME_VERSION: dynamic\nasset: compukter-runtime-${RUNTIME_VERSION}\n",
            );
            Self { directory }
        }

        fn path(&self) -> &Path {
            self.directory.path()
        }

        fn overwrite(&self, path: &str, contents: &str) {
            write(self.path().join(path), contents);
        }
    }

    fn write(path: impl AsRef<Path>, contents: &str) {
        let path = path.as_ref();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn loads_one_consistent_release_state() {
        let fixture = Fixture::consistent("0.5.1", 5);
        let state = ReleaseState::load(fixture.path()).unwrap();
        state.require_current_abi().unwrap();
        assert_eq!("0.5.1", state.version.to_string());
        assert_eq!(5, state.exported_abi);
    }

    #[test]
    fn reports_independent_version_and_workflow_mismatches() {
        let fixture = Fixture::consistent("0.5.1", 5);
        fixture.overwrite(
            "Cargo.toml",
            "[workspace]\nmembers = [\".\", \"ffi\"]\nresolver = \"2\"\n\n[workspace.package]\nversion = \"0.5.2\"\n\n[package]\nname = \"vm\"\nversion.workspace = true\nedition = \"2021\"\n",
        );
        fixture.overwrite(
            ".github/workflows/runtime-release.yml",
            "tags: [runtime-v0.*.*]\nasset: compukter-runtime-0.5.1-linux-x86_64.tar.gz\n",
        );
        let error = ReleaseState::load(fixture.path()).unwrap_err();
        assert!(error.contains("workspace version 0.5.2"), "{error}");
        assert!(error.contains("legacy runtime-v tag prefix"), "{error}");
        assert!(
            error.contains("hard-coded runtime version 0.5.1"),
            "{error}"
        );
    }

    #[test]
    fn staged_next_abi_is_loadable_but_not_current() {
        let fixture = Fixture::consistent("0.5.2", 6);
        let state = ReleaseState::load(fixture.path()).unwrap();
        assert!(state.require_current_abi().is_err());
        assert_eq!(6, state.exported_abi);
    }
}
