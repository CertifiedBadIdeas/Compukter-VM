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

use crate::archive::{create_bundle, inspect_bundle, BundleInputs};
use crate::smoke::require_ffi_abi;
use crate::version::RuntimeVersion;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Package(Box<PackageCommand>),
    Inspect(PathBuf),
    Smoke { library: PathBuf, abi: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageCommand {
    pub version_file: PathBuf,
    pub tag: String,
    pub commit: String,
    pub target: String,
    pub library: PathBuf,
    pub license: PathBuf,
    pub notice: PathBuf,
    pub rustc: String,
    pub formats: BTreeMap<String, u32>,
    pub output: PathBuf,
}

impl Command {
    pub fn parse<I, S>(arguments: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut arguments = arguments.into_iter().map(Into::into);
        match arguments.next().as_deref() {
            Some("package") => parse_package(arguments.collect()),
            Some("inspect") => parse_inspect(arguments.collect()),
            Some("smoke") => parse_smoke(arguments.collect()),
            Some(other) => Err(format!("unknown runtime-bundler command: {other}")),
            None => Err("runtime-bundler command is required".to_owned()),
        }
    }

    pub fn package(&self) -> Option<&PackageCommand> {
        match self {
            Self::Package(package) => Some(package),
            _ => None,
        }
    }
}

pub fn execute(command: Command) -> Result<String, String> {
    match command {
        Command::Package(package) => {
            let version = read_runtime_version(&package.version_file)?;
            require_ffi_abi(&package.library, version.abi).map_err(|error| error.to_string())?;
            let bundle = create_bundle(
                &BundleInputs {
                    runtime_version: version,
                    release_tag: &package.tag,
                    vm_commit: &package.commit,
                    rustc: &package.rustc,
                    target: &package.target,
                    native_library: &package.library,
                    license: &package.license,
                    notice: &package.notice,
                    formats: package.formats,
                },
                &package.output,
            )
            .map_err(|error| error.to_string())?;
            inspect_bundle(&bundle).map_err(|error| error.to_string())?;
            Ok(bundle.display().to_string())
        }
        Command::Inspect(path) => inspect_bundle(&path)
            .map_err(|error| error.to_string())?
            .to_json(),
        Command::Smoke { library, abi } => {
            require_ffi_abi(&library, abi).map_err(|error| error.to_string())?;
            Ok(format!("{} exports FFI ABI {abi}", library.display()))
        }
    }
}

fn parse_package(arguments: Vec<String>) -> Result<Command, String> {
    let mut version_file = None;
    let mut tag = None;
    let mut commit = None;
    let mut target = None;
    let mut library = None;
    let mut license = None;
    let mut notice = None;
    let mut rustc = None;
    let mut output = None;
    let mut formats = BTreeMap::new();
    let mut index = 0;
    while index < arguments.len() {
        let flag = &arguments[index];
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("missing value for {flag}"))?
            .clone();
        match flag.as_str() {
            "--version-file" => set_once(&mut version_file, PathBuf::from(value), flag)?,
            "--tag" => set_once(&mut tag, value, flag)?,
            "--commit" => set_once(&mut commit, value, flag)?,
            "--target" => set_once(&mut target, value, flag)?,
            "--library" => set_once(&mut library, PathBuf::from(value), flag)?,
            "--license" => set_once(&mut license, PathBuf::from(value), flag)?,
            "--notice" => set_once(&mut notice, PathBuf::from(value), flag)?,
            "--rustc" => set_once(&mut rustc, value, flag)?,
            "--output" => set_once(&mut output, PathBuf::from(value), flag)?,
            "--format" => {
                let (name, version) = parse_format(&value)?;
                if formats.insert(name.clone(), version).is_some() {
                    return Err(format!("duplicate runtime format: {name}"));
                }
            }
            _ => return Err(format!("unknown package argument: {flag}")),
        }
        index += 2;
    }
    Ok(Command::Package(Box::new(PackageCommand {
        version_file: required(version_file, "--version-file")?,
        tag: required(tag, "--tag")?,
        commit: required(commit, "--commit")?,
        target: required(target, "--target")?,
        library: required(library, "--library")?,
        license: required(license, "--license")?,
        notice: required(notice, "--notice")?,
        rustc: required(rustc, "--rustc")?,
        formats,
        output: required(output, "--output")?,
    })))
}

fn parse_inspect(arguments: Vec<String>) -> Result<Command, String> {
    if arguments.len() != 1 {
        return Err("inspect requires exactly one bundle path".to_owned());
    }
    Ok(Command::Inspect(PathBuf::from(&arguments[0])))
}

fn parse_smoke(arguments: Vec<String>) -> Result<Command, String> {
    if arguments.len() != 4 || arguments[0] != "--library" || arguments[2] != "--abi" {
        return Err("smoke requires --library <path> --abi <u32>".to_owned());
    }
    let abi = arguments[3]
        .parse::<u32>()
        .map_err(|_| "smoke ABI must be u32".to_owned())?;
    Ok(Command::Smoke {
        library: PathBuf::from(&arguments[1]),
        abi,
    })
}

fn parse_format(value: &str) -> Result<(String, u32), String> {
    let (name, version) = value
        .split_once('=')
        .ok_or_else(|| "runtime format must be name=version".to_owned())?;
    if name.is_empty() || version.is_empty() || version.contains('=') {
        return Err("runtime format must be name=version".to_owned());
    }
    let version = version
        .parse::<u32>()
        .map_err(|_| "runtime format version must be u32".to_owned())?;
    Ok((name.to_owned(), version))
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        Err(format!("duplicate package argument: {flag}"))
    } else {
        Ok(())
    }
}

fn required<T>(value: Option<T>, flag: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("missing required package argument: {flag}"))
}

fn read_runtime_version(path: &Path) -> Result<RuntimeVersion, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let line = contents.strip_suffix('\n').unwrap_or(&contents);
    if line.contains('\n') || line.contains('\r') {
        return Err("runtime version file must contain exactly one canonical line".to_owned());
    }
    let value = line
        .strip_prefix("version = \"")
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| "runtime version file must contain version = \"x.y\"".to_owned())?;
    RuntimeVersion::parse(value)
}

#[cfg(test)]
mod tests {
    use super::{read_runtime_version, Command};
    use std::fs;

    #[test]
    fn parses_the_explicit_package_contract() {
        let command = Command::parse([
            "package",
            "--version-file",
            "runtime-version.toml",
            "--tag",
            "runtime-v5.1",
            "--commit",
            "0123456789abcdef0123456789abcdef01234567",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--library",
            "target/release/libcompukter_ffi.so",
            "--license",
            "LICENSE",
            "--notice",
            "NOTICE",
            "--rustc",
            "rustc 1.98.0 (88d9e12ae 2026-08-18)",
            "--format",
            "artifact=2",
            "--format",
            "filesystem-generation=1",
            "--output",
            "target/runtime-release",
        ])
        .unwrap();

        let package = command.package().unwrap();
        assert_eq!("runtime-v5.1", package.tag);
        assert_eq!(Some(&2), package.formats.get("artifact"));
        assert_eq!(Some(&1), package.formats.get("filesystem-generation"));
    }

    #[test]
    fn rejects_duplicate_formats_and_missing_package_arguments() {
        assert!(Command::parse([
            "package",
            "--format",
            "artifact=1",
            "--format",
            "artifact=2",
        ])
        .is_err());
    }

    #[test]
    fn reads_only_the_canonical_runtime_version_file() {
        let directory = tempfile::tempdir().unwrap();
        let valid = directory.path().join("valid.toml");
        let semver = directory.path().join("semver.toml");
        let extra = directory.path().join("extra.toml");
        fs::write(&valid, "version = \"5.1\"\n").unwrap();
        fs::write(&semver, "version = \"5.1.0\"\n").unwrap();
        fs::write(&extra, "version = \"5.1\"\nother = 1\n").unwrap();

        let version = read_runtime_version(&valid).unwrap();
        assert_eq!(5, version.abi);
        assert_eq!(1, version.revision);
        assert!(read_runtime_version(&semver).is_err());
        assert!(read_runtime_version(&extra).is_err());
    }
}
