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

use std::fmt::{Display, Formatter};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeVersion {
    pub abi: u32,
    pub revision: u32,
}

impl RuntimeVersion {
    pub fn parse(value: &str) -> Result<Self, String> {
        let mut components = value.split('.');
        let major = components.next();
        let abi = components.next();
        let revision = components.next();
        if major != Some("0") || abi.is_none() || revision.is_none() || components.next().is_some()
        {
            return Err("runtime version must use canonical 0.<abi>.<revision> form".to_owned());
        }
        Ok(Self {
            abi: parse_component(abi.unwrap(), "ABI")?,
            revision: parse_component(revision.unwrap(), "revision")?,
        })
    }

    pub fn tag(self) -> String {
        format!("runtime-v{self}")
    }

    pub fn require_abi(self, exported_abi: u32) -> Result<(), String> {
        if self.abi == exported_abi {
            Ok(())
        } else {
            Err(format!(
                "runtime ABI {} does not match exported FFI ABI {exported_abi}",
                self.abi
            ))
        }
    }
}

impl Display for RuntimeVersion {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "0.{}.{}", self.abi, self.revision)
    }
}

fn parse_component(value: &str, name: &str) -> Result<u32, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("runtime {name} must be unsigned decimal"));
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(format!("runtime {name} must use canonical decimal"));
    }
    value
        .parse()
        .map_err(|_| format!("runtime {name} exceeds u32"))
}

#[cfg(test)]
mod tests {
    use super::RuntimeVersion;

    #[test]
    fn parses_runtime_version_and_tag() {
        let version = RuntimeVersion::parse("0.5.1").unwrap();
        assert_eq!(5, version.abi);
        assert_eq!(1, version.revision);
        assert_eq!("runtime-v0.5.1", version.tag());
    }

    #[test]
    fn rejects_non_pre_one_and_malformed_runtime_versions() {
        for invalid in [
            "5.1",
            "0.5",
            "0.5.1.0",
            "1.5.1",
            "v0.5.1",
            "0.05.1",
            "0.5.01",
            "0.5.1-alpha",
        ] {
            assert!(
                RuntimeVersion::parse(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn requires_version_abi_to_match_exported_abi() {
        let version = RuntimeVersion::parse("0.6.0").unwrap();
        assert!(version.require_abi(5).is_err());
    }
}
