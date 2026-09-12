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

use crate::version::RuntimeVersion;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const LINUX_TARGET: &str = "x86_64-unknown-linux-gnu";
pub const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";
pub const FFI_TRANSPORT: &str = "ffi";
pub const JNI_TRANSPORT: &str = "jni";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLibrary {
    pub filename: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeManifest {
    pub schema: u32,
    pub runtime_version: String,
    pub release_tag: String,
    pub vm_commit: String,
    pub ffi_abi: u32,
    pub formats: BTreeMap<String, u32>,
    pub rustc: String,
    pub target: String,
    pub libraries: BTreeMap<String, NativeLibrary>,
    pub profile: String,
}

impl RuntimeManifest {
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self)
            .map(|json| format!("{json}\n"))
            .map_err(|error| format!("failed to encode runtime manifest: {error}"))
    }

    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json)
            .map_err(|error| format!("failed to decode runtime manifest: {error}"))
    }

    pub fn validate_for(
        &self,
        expected_version: RuntimeVersion,
        expected_tag: &str,
        exported_abi: u32,
    ) -> Result<(), String> {
        require(self.schema == 2, "runtime manifest schema must be 2")?;
        let actual_version = RuntimeVersion::parse(&self.runtime_version)?;
        require(
            actual_version == expected_version,
            "runtime manifest version does not match the requested version",
        )?;
        require(
            self.release_tag == expected_tag && self.release_tag == actual_version.tag(),
            "runtime manifest tag does not match its version",
        )?;
        actual_version.require_abi(exported_abi)?;
        require(
            self.ffi_abi == exported_abi,
            "runtime manifest FFI ABI does not match the exported ABI",
        )?;
        require(
            is_lower_hex(&self.vm_commit, 40),
            "runtime manifest VM commit must be 40 lowercase hexadecimal characters",
        )?;
        require(
            self.profile == "release",
            "runtime manifest profile must be release",
        )?;
        require(
            !self.rustc.is_empty(),
            "runtime manifest rustc must not be empty",
        )?;
        let expected_libraries = [FFI_TRANSPORT, JNI_TRANSPORT];
        require(
            self.libraries
                .keys()
                .map(String::as_str)
                .eq(expected_libraries),
            "runtime manifest must contain exactly the FFI and JNI libraries",
        )?;
        for (transport, library) in &self.libraries {
            require(
                expected_filename(&self.target, transport) == Some(library.filename.as_str()),
                "runtime manifest target, transport, and filename do not match",
            )?;
            require(
                library.size > 0,
                "runtime manifest native size must be positive",
            )?;
            require(
                is_lower_hex(&library.sha256, 64),
                "runtime manifest SHA-256 must be 64 lowercase hexadecimal characters",
            )?;
        }
        require(
            !self.formats.is_empty(),
            "runtime manifest formats must not be empty",
        )?;
        require(
            self.formats
                .iter()
                .all(|(name, version)| is_format_name(name) && *version > 0),
            "runtime manifest formats must use canonical names and positive versions",
        )?;
        Ok(())
    }
}

pub fn expected_filename(target: &str, transport: &str) -> Option<&'static str> {
    match (target, transport) {
        (LINUX_TARGET, FFI_TRANSPORT) => Some("libcompukter_ffi.so"),
        (LINUX_TARGET, JNI_TRANSPORT) => Some("libcompukter_jni.so"),
        (WINDOWS_TARGET, FFI_TRANSPORT) => Some("compukter_ffi.dll"),
        (WINDOWS_TARGET, JNI_TRANSPORT) => Some("compukter_jni.dll"),
        _ => None,
    }
}

fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_format_name(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::{NativeLibrary, RuntimeManifest};
    use crate::version::RuntimeVersion;
    use std::collections::BTreeMap;

    fn manifest() -> RuntimeManifest {
        RuntimeManifest {
            schema: 2,
            runtime_version: "0.5.1".to_owned(),
            release_tag: "v0.5.1".to_owned(),
            vm_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            ffi_abi: 5,
            formats: BTreeMap::from([
                ("artifact".to_owned(), 2),
                ("compilation-request".to_owned(), 1),
                ("executable-revision".to_owned(), 1),
                ("filesystem-generation".to_owned(), 1),
            ]),
            rustc: "rustc 1.98.0 (88d9e12ae 2026-08-18)".to_owned(),
            target: "x86_64-unknown-linux-gnu".to_owned(),
            libraries: BTreeMap::from([
                (
                    "ffi".to_owned(),
                    NativeLibrary {
                        filename: "libcompukter_ffi.so".to_owned(),
                        size: 42,
                        sha256: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                            .to_owned(),
                    },
                ),
                (
                    "jni".to_owned(),
                    NativeLibrary {
                        filename: "libcompukter_jni.so".to_owned(),
                        size: 43,
                        sha256: "123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0"
                            .to_owned(),
                    },
                ),
            ]),
            profile: "release".to_owned(),
        }
    }

    #[test]
    fn round_trips_canonical_json_with_trailing_lf() {
        let expected = manifest();
        let json = expected.to_json().unwrap();

        assert!(json.ends_with('\n'));
        assert_eq!(expected, RuntimeManifest::from_json(&json).unwrap());
    }

    #[test]
    fn validates_the_runtime_identity_and_platform_contract() {
        let version = RuntimeVersion::parse("0.5.1").unwrap();

        assert!(manifest().validate_for(version, "v0.5.1", 5).is_ok());
    }

    #[test]
    fn rejects_noncanonical_commit_and_digest() {
        let version = RuntimeVersion::parse("0.5.1").unwrap();
        let mut short_commit = manifest();
        short_commit.vm_commit = "short".to_owned();
        let mut bad_digest = manifest();
        bad_digest.libraries.get_mut("jni").unwrap().sha256 = "xyz".to_owned();

        assert!(short_commit.validate_for(version, "v0.5.1", 5).is_err());
        assert!(bad_digest.validate_for(version, "v0.5.1", 5).is_err());
    }

    #[test]
    fn rejects_target_filename_mismatch_missing_transport_and_empty_formats() {
        let version = RuntimeVersion::parse("0.5.1").unwrap();
        let mut wrong_filename = manifest();
        wrong_filename.libraries.get_mut("ffi").unwrap().filename = "compukter_ffi.dll".to_owned();
        let mut missing_transport = manifest();
        missing_transport.libraries.remove("jni");
        let mut no_formats = manifest();
        no_formats.formats.clear();

        assert!(wrong_filename.validate_for(version, "v0.5.1", 5).is_err());
        assert!(missing_transport
            .validate_for(version, "v0.5.1", 5)
            .is_err());
        assert!(no_formats.validate_for(version, "v0.5.1", 5).is_err());
    }
}
