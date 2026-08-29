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

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use xtask::bump::bump;
use xtask::cli::BumpKind;
use xtask::process::ProcessRunner;

struct TestRepository {
    directory: TempDir,
}

impl TestRepository {
    fn consistent(version: &str, abi: u32) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        write(
            root.join("Cargo.toml"),
            &format!(
                "[workspace]\nmembers = [\".\", \"ffi\"]\nresolver = \"2\"\n\n[workspace.package]\nversion = \"{version}\"\n\n[package]\nname = \"vm\"\nversion.workspace = true\nedition = \"2021\"\n"
            ),
        );
        write(
            root.join("ffi/Cargo.toml"),
            "[package]\nname = \"ffi\"\nversion.workspace = true\nedition = \"2021\"\n",
        );
        write_lock(root, version);
        write(
            root.join("runtime-version.toml"),
            &format!("version = \"{version}\"\n"),
        );
        write(
            root.join("ffi/src/lib.rs"),
            &format!("pub const COMPUKTER_FFI_ABI_VERSION: u32 = {abi};\n"),
        );
        write(
            root.join(".github/workflows/runtime-release.yml"),
            "tags:\n  - \"v0.*.*\"\nenv:\n  RUNTIME_TAG: input\n  RUNTIME_VERSION: dynamic\nasset: compukter-runtime-${RUNTIME_VERSION}\n",
        );
        git(root, &["init", "-b", "main"]);
        git(root, &["config", "user.name", "Compukters Test"]);
        git(root, &["config", "user.email", "test@compukters.invalid"]);
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "fixture"]);
        Self { directory }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn runtime_version(&self) -> String {
        fs::read_to_string(self.path().join("runtime-version.toml"))
            .unwrap()
            .trim()
            .trim_start_matches("version = \"")
            .trim_end_matches('"')
            .to_owned()
    }

    fn bytes(&self, path: &str) -> Vec<u8> {
        fs::read(self.path().join(path)).unwrap()
    }

    fn head_subject(&self) -> String {
        git_output(self.path(), &["log", "-1", "--format=%s"])
    }

    fn is_clean(&self) -> bool {
        git_output(self.path(), &["status", "--porcelain=v1"]).is_empty()
    }
}

struct UpdatingRunner;

impl ProcessRunner for UpdatingRunner {
    fn run(
        &self,
        root: &Path,
        _program: &str,
        _arguments: &[&str],
        purpose: &str,
    ) -> Result<(), String> {
        assert_eq!("regenerate Cargo.lock", purpose);
        let version = fs::read_to_string(root.join("runtime-version.toml"))
            .unwrap()
            .trim()
            .trim_start_matches("version = \"")
            .trim_end_matches('"')
            .to_owned();
        write_lock(root, &version);
        Ok(())
    }
}

struct FailingRunner;

impl ProcessRunner for FailingRunner {
    fn run(
        &self,
        _root: &Path,
        _program: &str,
        _arguments: &[&str],
        purpose: &str,
    ) -> Result<(), String> {
        Err(format!("{purpose} failed with exit status 1"))
    }
}

#[test]
fn revision_bump_updates_every_version_and_creates_one_local_commit() {
    let repository = TestRepository::consistent("0.5.1", 5);
    bump(repository.path(), BumpKind::Revision, &UpdatingRunner).unwrap();
    assert_eq!("0.5.2", repository.runtime_version());
    assert!(repository
        .bytes("Cargo.toml")
        .windows(b"version = \"0.5.2\"".len())
        .any(|window| window == b"version = \"0.5.2\""));
    assert!(repository
        .bytes("Cargo.lock")
        .windows(b"version = \"0.5.2\"".len())
        .any(|window| window == b"version = \"0.5.2\""));
    assert_eq!(
        "chore(release): bump version to 0.5.2",
        repository.head_subject()
    );
    assert!(repository.is_clean());
}

#[test]
fn failed_regeneration_restores_exact_original_bytes() {
    let repository = TestRepository::consistent("0.5.1", 5);
    let before = [
        repository.bytes("runtime-version.toml"),
        repository.bytes("Cargo.toml"),
        repository.bytes("Cargo.lock"),
    ];
    let error = bump(repository.path(), BumpKind::Revision, &FailingRunner).unwrap_err();
    assert!(error.contains("regenerate Cargo.lock"), "{error}");
    assert_eq!(before[0], repository.bytes("runtime-version.toml"));
    assert_eq!(before[1], repository.bytes("Cargo.toml"));
    assert_eq!(before[2], repository.bytes("Cargo.lock"));
    assert!(repository.is_clean());
}

#[test]
fn dirty_repository_is_rejected_before_mutation() {
    let repository = TestRepository::consistent("0.5.1", 5);
    write(repository.path().join("dirty.txt"), "mine\n");
    let before = repository.bytes("runtime-version.toml");
    assert!(bump(repository.path(), BumpKind::Revision, &UpdatingRunner).is_err());
    assert_eq!(before, repository.bytes("runtime-version.toml"));
}

#[test]
fn abi_bump_requires_the_exported_next_abi() {
    let repository = TestRepository::consistent("0.5.2", 6);
    bump(repository.path(), BumpKind::Abi, &UpdatingRunner).unwrap();
    assert_eq!("0.6.0", repository.runtime_version());

    let rejected = TestRepository::consistent("0.5.2", 5);
    assert!(bump(rejected.path(), BumpKind::Abi, &UpdatingRunner).is_err());
    assert_eq!("0.5.2", rejected.runtime_version());
}

fn write_lock(root: &Path, version: &str) {
    write(
        root.join("Cargo.lock"),
        &format!(
            "version = 4\n\n[[package]]\nname = \"ffi\"\nversion = \"{version}\"\n\n[[package]]\nname = \"vm\"\nversion = \"{version}\"\n"
        ),
    );
}

fn write(path: impl Into<PathBuf>, contents: &str) {
    let path = path.into();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn git(root: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?} failed");
}

fn git_output(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
