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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeVersion {
    pub abi: u32,
    pub revision: u32,
}

impl RuntimeVersion {
    pub fn parse(value: &str) -> Result<Self, String> {
        let (abi, revision) = value
            .split_once('.')
            .ok_or_else(|| "runtime version must contain exactly one dot".to_owned())?;
        if revision.contains('.') {
            return Err("runtime version must contain exactly one dot".to_owned());
        }
        Ok(Self {
            abi: parse_component(abi, "ABI")?,
            revision: parse_component(revision, "revision")?,
        })
    }

    pub fn tag(self) -> String {
        format!("runtime-v{}.{}", self.abi, self.revision)
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
        let version = RuntimeVersion::parse("5.0").unwrap();
        assert_eq!(5, version.abi);
        assert_eq!(0, version.revision);
        assert_eq!("runtime-v5.0", version.tag());
    }

    #[test]
    fn rejects_semver_and_malformed_runtime_versions() {
        for invalid in ["5", "5.0.0", "v5.0", "5.-1", "05.0", "5.0-alpha"] {
            assert!(
                RuntimeVersion::parse(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn requires_version_abi_to_match_exported_abi() {
        let version = RuntimeVersion::parse("6.0").unwrap();
        assert!(version.require_abi(5).is_err());
    }
}
