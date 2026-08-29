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

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct GitRepository {
    root: PathBuf,
}

impl GitRepository {
    pub fn open(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
        }
    }

    pub fn require_clean(&self) -> Result<(), String> {
        let status = self.output(&["status", "--porcelain=v1"])?;
        if status.is_empty() {
            Ok(())
        } else {
            Err(format!("repository is dirty:\n{status}"))
        }
    }

    pub fn branch(&self) -> Result<String, String> {
        self.output(&["symbolic-ref", "--quiet", "--short", "HEAD"])
    }

    pub fn tag_exists(&self, tag: &str) -> Result<bool, String> {
        let reference = format!("refs/tags/{tag}");
        let status = Command::new("git")
            .current_dir(&self.root)
            .args(["show-ref", "--verify", "--quiet", &reference])
            .status()
            .map_err(|error| format!("cannot inspect Git tag {tag}: {error}"))?;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(format!(
                "cannot inspect Git tag {tag}: exit status {status}"
            )),
        }
    }

    pub fn commit(&self, message: &str, paths: &[&Path]) -> Result<(), String> {
        let mut add = Command::new("git");
        add.current_dir(&self.root).arg("add").arg("--");
        add.args(paths);
        require_success(add.output(), "stage release version files")?;

        let mut commit = Command::new("git");
        commit
            .current_dir(&self.root)
            .args(["commit", "-m", message, "--"])
            .args(paths);
        if let Err(error) = require_success(commit.output(), "commit release version") {
            let mut restore = Command::new("git");
            restore
                .current_dir(&self.root)
                .args(["restore", "--staged", "--"])
                .args(paths);
            let _ = restore.status();
            return Err(error);
        }
        Ok(())
    }

    pub fn create_annotated_tag(&self, tag: &str, message: &str) -> Result<(), String> {
        let mut command = Command::new("git");
        command
            .current_dir(&self.root)
            .args(["tag", "-a", tag, "-m", message]);
        require_success(command.output(), "create annotated release tag")?;
        Ok(())
    }

    fn output(&self, arguments: &[&str]) -> Result<String, String> {
        let mut command = Command::new("git");
        command.current_dir(&self.root).args(arguments);
        let output = require_success(command.output(), "inspect Git repository")?;
        String::from_utf8(output.stdout)
            .map(|value| value.trim().to_owned())
            .map_err(|_| "Git output is not UTF-8".to_owned())
    }
}

fn require_success(output: std::io::Result<Output>, purpose: &str) -> Result<Output, String> {
    let output = output.map_err(|error| format!("cannot {purpose}: {error}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if detail.is_empty() {
            Err(format!("cannot {purpose}: exit status {}", output.status))
        } else {
            Err(format!("cannot {purpose}: {detail}"))
        }
    }
}
