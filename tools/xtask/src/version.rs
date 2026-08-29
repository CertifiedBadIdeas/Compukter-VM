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
pub struct ReleaseVersion {
    pub abi: u32,
    pub revision: u32,
}

impl ReleaseVersion {
    pub fn parse(value: &str) -> Result<Self, String> {
        let mut components = value.split('.');
        let major = components.next();
        let abi = components.next();
        let revision = components.next();
        if major != Some("0") || abi.is_none() || revision.is_none() || components.next().is_some()
        {
            return Err("version must use canonical 0.<abi>.<revision> form".to_owned());
        }
        Ok(Self {
            abi: parse_component(abi.unwrap(), "ABI")?,
            revision: parse_component(revision.unwrap(), "revision")?,
        })
    }

    pub fn bump_revision(self) -> Result<Self, String> {
        Ok(Self {
            abi: self.abi,
            revision: self
                .revision
                .checked_add(1)
                .ok_or_else(|| "version revision exceeds u32".to_owned())?,
        })
    }

    pub fn bump_abi(self, exported_abi: u32) -> Result<Self, String> {
        let target = self
            .abi
            .checked_add(1)
            .ok_or_else(|| "version ABI exceeds u32".to_owned())?;
        if exported_abi != target {
            return Err(format!(
                "next version ABI {target} does not match exported FFI ABI {exported_abi}"
            ));
        }
        Ok(Self {
            abi: target,
            revision: 0,
        })
    }
}

impl Display for ReleaseVersion {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "0.{}.{}", self.abi, self.revision)
    }
}

fn parse_component(value: &str, name: &str) -> Result<u32, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("version {name} must be unsigned decimal"));
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(format!("version {name} must use canonical decimal"));
    }
    value
        .parse()
        .map_err(|_| format!("version {name} exceeds u32"))
}

#[cfg(test)]
mod tests {
    use super::ReleaseVersion;

    #[test]
    fn revision_and_abi_transitions_are_strict() {
        let current = ReleaseVersion::parse("0.5.1").unwrap();
        assert_eq!("0.5.2", current.bump_revision().unwrap().to_string());
        assert_eq!("0.6.0", current.bump_abi(6).unwrap().to_string());
        assert!(current.bump_abi(5).is_err());
    }

    #[test]
    fn rejects_noncanonical_versions_and_overflow() {
        for invalid in ["1.5.1", "0.05.1", "0.5.01", "v0.5.1", "0.5"] {
            assert!(
                ReleaseVersion::parse(invalid).is_err(),
                "accepted {invalid}"
            );
        }
        let maximum = ReleaseVersion::parse("0.4294967295.4294967295").unwrap();
        assert!(maximum.bump_revision().is_err());
        assert!(maximum.bump_abi(u32::MAX).is_err());
    }
}
