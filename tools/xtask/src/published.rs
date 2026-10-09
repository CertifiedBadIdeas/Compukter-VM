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
use serde_json::Value;
use std::path::Path;
use std::process::Command;

pub trait PublishedReleaseSource {
    fn latest(&self, root: &Path) -> Result<ReleaseVersion, String>;
}

pub struct GitHubPublishedRelease;

impl PublishedReleaseSource for GitHubPublishedRelease {
    fn latest(&self, root: &Path) -> Result<ReleaseVersion, String> {
        let repository = gh_json(root, &["repo", "view", "--json", "nameWithOwner"])?;
        let name = repository["nameWithOwner"]
            .as_str()
            .ok_or_else(|| "GitHub repository identity is missing".to_owned())?;
        let metadata = gh_json(root, &["api", &format!("repos/{name}/releases/latest")])?;
        parse_published_release(&metadata)
    }
}

fn gh_json(root: &Path, arguments: &[&str]) -> Result<Value, String> {
    let output = Command::new("gh")
        .current_dir(root)
        .args(arguments)
        .output()
        .map_err(|error| format!("cannot query published Runtime release: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot query published Runtime release: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid GitHub release response: {error}"))
}

pub fn parse_published_release(metadata: &Value) -> Result<ReleaseVersion, String> {
    if metadata["draft"].as_bool() != Some(false)
        || metadata["prerelease"].as_bool() != Some(false)
        || metadata["published_at"].as_str().is_none_or(str::is_empty)
    {
        return Err("version bump requires a published stable Runtime release".to_owned());
    }
    let tag = metadata["tag_name"]
        .as_str()
        .and_then(|tag| tag.strip_prefix('v'))
        .ok_or_else(|| "published Runtime tag must use v0.<abi>.<revision>".to_owned())?;
    let version = ReleaseVersion::parse(tag)?;
    let assets = metadata["assets"]
        .as_array()
        .ok_or_else(|| "published Runtime assets are missing".to_owned())?;
    for suffix in [
        "linux-x86_64.tar.gz",
        "windows-x86_64.zip",
        "checksums.sha256",
    ] {
        let name = format!("compukter-runtime-{version}-{suffix}");
        let matches = assets
            .iter()
            .filter(|asset| asset["name"].as_str() == Some(&name))
            .collect::<Vec<_>>();
        if matches.len() != 1
            || matches[0]["state"].as_str() != Some("uploaded")
            || matches[0]["size"].as_u64().is_none_or(|size| size == 0)
        {
            return Err(format!(
                "published Runtime release lacks complete asset {name}"
            ));
        }
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn complete() -> Value {
        json!({"draft": false, "prerelease": false, "published_at": "2026-10-09T11:46:04Z",
               "tag_name": "v0.21.2", "assets": [
                   {"name": "compukter-runtime-0.21.2-linux-x86_64.tar.gz", "state": "uploaded", "size": 10},
                   {"name": "compukter-runtime-0.21.2-windows-x86_64.zip", "state": "uploaded", "size": 10},
                   {"name": "compukter-runtime-0.21.2-checksums.sha256", "state": "uploaded", "size": 10}]})
    }

    #[test]
    fn admits_only_complete_published_runtime_releases() {
        assert_eq!(
            "0.21.2",
            parse_published_release(&complete()).unwrap().to_string()
        );
        for (field, value) in [
            ("draft", json!(true)),
            ("prerelease", json!(true)),
            ("published_at", Value::Null),
            ("published_at", json!("")),
            ("tag_name", json!("v1.21.2")),
            ("assets", json!([])),
        ] {
            let mut metadata = complete();
            metadata[field] = value;
            assert!(parse_published_release(&metadata).is_err(), "{metadata}");
        }
        for index in 0..3 {
            for (field, value) in [
                ("state", json!("new")),
                ("size", json!(0)),
                ("name", json!("other")),
            ] {
                let mut metadata = complete();
                metadata["assets"][index][field] = value;
                assert!(parse_published_release(&metadata).is_err(), "{metadata}");
            }
        }
        let mut duplicate = complete();
        let repeated = duplicate["assets"][0].clone();
        duplicate["assets"].as_array_mut().unwrap().push(repeated);
        assert!(parse_published_release(&duplicate).is_err());
    }
}
