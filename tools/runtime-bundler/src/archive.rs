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

use crate::manifest::{expected_filename, RuntimeManifest, LINUX_TARGET, WINDOWS_TARGET};
use crate::version::RuntimeVersion;
use flate2::{Compression, GzBuilder};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use tar::{Archive, Builder, EntryType, Header};
use tempfile::NamedTempFile;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const MAXIMUM_NATIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAXIMUM_METADATA_BYTES: u64 = 1024 * 1024;
const MANIFEST_NAME: &str = "manifest.json";
const LICENSE_NAME: &str = "LICENSE.txt";
const NOTICE_NAME: &str = "NOTICE.txt";

pub struct BundleInputs<'a> {
    pub runtime_version: RuntimeVersion,
    pub release_tag: &'a str,
    pub vm_commit: &'a str,
    pub rustc: &'a str,
    pub target: &'a str,
    pub native_library: &'a Path,
    pub license: &'a Path,
    pub notice: &'a Path,
    pub formats: BTreeMap<String, u32>,
}

#[derive(Debug)]
pub struct BundleError(String);

impl Display for BundleError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BundleError {}

impl From<io::Error> for BundleError {
    fn from(error: io::Error) -> Self {
        Self(format!("runtime bundle I/O failed: {error}"))
    }
}

pub fn create_bundle(inputs: &BundleInputs<'_>, output_dir: &Path) -> Result<PathBuf, BundleError> {
    let expected_native = expected_filename(inputs.target)
        .ok_or_else(|| BundleError(format!("unsupported runtime target: {}", inputs.target)))?;
    require(
        inputs
            .native_library
            .file_name()
            .and_then(|name| name.to_str())
            == Some(expected_native),
        "native library filename does not match its target",
    )?;

    let native = read_regular_bounded(inputs.native_library, MAXIMUM_NATIVE_BYTES, "native")?;
    let license = read_regular_bounded(inputs.license, MAXIMUM_METADATA_BYTES, "license")?;
    let notice = read_regular_bounded(inputs.notice, MAXIMUM_METADATA_BYTES, "notice")?;
    require(!license.is_empty(), "runtime license must not be empty")?;
    require(!notice.is_empty(), "runtime notice must not be empty")?;

    let manifest = RuntimeManifest {
        schema: 1,
        runtime_version: format!(
            "{}.{}",
            inputs.runtime_version.abi, inputs.runtime_version.revision
        ),
        release_tag: inputs.release_tag.to_owned(),
        vm_commit: inputs.vm_commit.to_owned(),
        ffi_abi: inputs.runtime_version.abi,
        formats: inputs.formats.clone(),
        rustc: inputs.rustc.to_owned(),
        target: inputs.target.to_owned(),
        filename: expected_native.to_owned(),
        size: native.len() as u64,
        sha256: sha256(&native),
        profile: "release".to_owned(),
    };
    manifest
        .validate_for(
            inputs.runtime_version,
            inputs.release_tag,
            inputs.runtime_version.abi,
        )
        .map_err(BundleError)?;
    let manifest_json = manifest.to_json().map_err(BundleError)?.into_bytes();

    fs::create_dir_all(output_dir)?;
    let output = output_dir.join(bundle_filename(inputs.runtime_version, inputs.target)?);
    require(!output.exists(), "runtime bundle output already exists")?;
    let mut temporary = NamedTempFile::new_in(output_dir)?;
    let entries = [
        (format!("native/{expected_native}"), native.as_slice()),
        (MANIFEST_NAME.to_owned(), manifest_json.as_slice()),
        (LICENSE_NAME.to_owned(), license.as_slice()),
        (NOTICE_NAME.to_owned(), notice.as_slice()),
    ];

    match inputs.target {
        LINUX_TARGET => write_tar_gz(temporary.as_file_mut(), &entries)?,
        WINDOWS_TARGET => write_zip(temporary.as_file_mut(), &entries)?,
        _ => return Err(BundleError("unsupported runtime target".to_owned())),
    }
    temporary.as_file_mut().sync_all()?;
    temporary.persist_noclobber(&output).map_err(|error| {
        BundleError(format!("failed to persist runtime bundle: {}", error.error))
    })?;

    let inspected = inspect_bundle(&output)?;
    require(
        inspected == manifest,
        "reinspected runtime manifest changed",
    )?;
    Ok(output)
}

pub fn inspect_bundle(path: &Path) -> Result<RuntimeManifest, BundleError> {
    let contents = if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".tar.gz"))
    {
        read_tar_gz(path)?
    } else if path.extension().and_then(|extension| extension.to_str()) == Some("zip") {
        read_zip(path)?
    } else {
        return Err(BundleError(
            "unsupported runtime bundle extension".to_owned(),
        ));
    };
    let manifest_bytes = contents
        .get(MANIFEST_NAME)
        .ok_or_else(|| BundleError("runtime bundle manifest is missing".to_owned()))?;
    let manifest_text = std::str::from_utf8(manifest_bytes)
        .map_err(|_| BundleError("runtime bundle manifest is not UTF-8".to_owned()))?;
    let manifest = RuntimeManifest::from_json(manifest_text).map_err(BundleError)?;
    let version = RuntimeVersion::parse(&manifest.runtime_version).map_err(BundleError)?;
    manifest
        .validate_for(version, &version.tag(), manifest.ffi_abi)
        .map_err(BundleError)?;

    let native_name = format!("native/{}", manifest.filename);
    let expected = BTreeSet::from([
        native_name.clone(),
        MANIFEST_NAME.to_owned(),
        LICENSE_NAME.to_owned(),
        NOTICE_NAME.to_owned(),
    ]);
    require(
        contents.keys().cloned().collect::<BTreeSet<_>>() == expected,
        "runtime bundle entries do not match the fixed layout",
    )?;
    require(
        !contents.get(LICENSE_NAME).is_some_and(Vec::is_empty),
        "runtime license must not be empty",
    )?;
    require(
        !contents.get(NOTICE_NAME).is_some_and(Vec::is_empty),
        "runtime notice must not be empty",
    )?;
    let native = contents
        .get(&native_name)
        .ok_or_else(|| BundleError("runtime native payload is missing".to_owned()))?;
    require(
        native.len() as u64 == manifest.size,
        "runtime native size does not match the manifest",
    )?;
    require(
        sha256(native) == manifest.sha256,
        "runtime native SHA-256 does not match the manifest",
    )?;
    require(
        path.file_name().and_then(|name| name.to_str())
            == Some(bundle_filename(version, &manifest.target)?.as_str()),
        "runtime bundle filename does not match the manifest",
    )?;
    Ok(manifest)
}

fn bundle_filename(version: RuntimeVersion, target: &str) -> Result<String, BundleError> {
    let platform = match target {
        LINUX_TARGET => "linux-x86_64.tar.gz",
        WINDOWS_TARGET => "windows-x86_64.zip",
        _ => return Err(BundleError(format!("unsupported runtime target: {target}"))),
    };
    Ok(format!(
        "compukter-runtime-{}.{}-{platform}",
        version.abi, version.revision
    ))
}

fn read_regular_bounded(
    path: &Path,
    maximum: u64,
    description: &str,
) -> Result<Vec<u8>, BundleError> {
    let metadata = fs::symlink_metadata(path)?;
    require(
        metadata.file_type().is_file(),
        &format!("runtime {description} input must be a regular file"),
    )?;
    require(
        metadata.len() <= maximum,
        &format!("runtime {description} input exceeds its byte limit"),
    )?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    require(
        bytes.len() as u64 <= maximum,
        &format!("runtime {description} input exceeds its byte limit"),
    )?;
    Ok(bytes)
}

fn write_tar_gz(file: &mut File, entries: &[(String, &[u8])]) -> Result<(), BundleError> {
    let encoder = GzBuilder::new().mtime(0).write(file, Compression::best());
    let mut archive = Builder::new(encoder);
    for (name, bytes) in entries {
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Regular);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        archive.append_data(&mut header, name, *bytes)?;
    }
    archive.into_inner()?.finish()?;
    Ok(())
}

fn write_zip(file: &mut File, entries: &[(String, &[u8])]) -> Result<(), BundleError> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let mut archive = ZipWriter::new(file);
    for (name, bytes) in entries {
        archive
            .start_file(name, options)
            .map_err(|error| BundleError(format!("failed to start ZIP entry: {error}")))?;
        archive.write_all(bytes)?;
    }
    archive
        .finish()
        .map_err(|error| BundleError(format!("failed to finish ZIP bundle: {error}")))?;
    Ok(())
}

fn read_tar_gz(path: &Path) -> Result<BTreeMap<String, Vec<u8>>, BundleError> {
    let file = File::open(path)?;
    let mut archive = Archive::new(flate2::read::GzDecoder::new(file));
    let mut contents = BTreeMap::new();
    for entry in archive.entries()? {
        let entry = entry?;
        require(
            entry.header().entry_type().is_file(),
            "runtime TAR entries must be regular files",
        )?;
        let path = entry.path_bytes();
        let name = safe_entry_name(
            std::str::from_utf8(&path)
                .map_err(|_| BundleError("runtime bundle entry path is not UTF-8".to_owned()))?,
        )?;
        let maximum = entry_limit(&name);
        require(
            entry.size() <= maximum,
            "runtime TAR entry exceeds its byte limit",
        )?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.take(maximum + 1).read_to_end(&mut bytes)?;
        require(
            bytes.len() as u64 <= maximum,
            "runtime TAR entry exceeds its byte limit",
        )?;
        require(
            contents.insert(name, bytes).is_none(),
            "runtime TAR contains duplicate entries",
        )?;
        require(contents.len() <= 4, "runtime TAR contains too many entries")?;
    }
    Ok(contents)
}

fn read_zip(path: &Path) -> Result<BTreeMap<String, Vec<u8>>, BundleError> {
    let mut archive = ZipArchive::new(File::open(path)?)
        .map_err(|error| BundleError(format!("failed to open ZIP bundle: {error}")))?;
    require(archive.len() <= 4, "runtime ZIP contains too many entries")?;
    let mut contents = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| BundleError(format!("failed to read ZIP entry: {error}")))?;
        require(entry.is_file(), "runtime ZIP entries must be regular files")?;
        if let Some(mode) = entry.unix_mode() {
            require(
                mode & 0o170000 != 0o120000,
                "runtime ZIP symlinks are forbidden",
            )?;
        }
        let name = safe_entry_name(entry.name())?;
        let maximum = entry_limit(&name);
        require(
            entry.size() <= maximum,
            "runtime ZIP entry exceeds its byte limit",
        )?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.by_ref().take(maximum + 1).read_to_end(&mut bytes)?;
        require(
            bytes.len() as u64 <= maximum,
            "runtime ZIP entry exceeds its byte limit",
        )?;
        require(
            contents.insert(name, bytes).is_none(),
            "runtime ZIP contains duplicate entries",
        )?;
    }
    Ok(contents)
}

fn safe_entry_name(name: &str) -> Result<String, BundleError> {
    require(
        !name.is_empty()
            && !name.contains('\\')
            && name
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != ".."),
        "runtime bundle entry path is unsafe",
    )?;
    Ok(name.to_owned())
}

fn entry_limit(name: &str) -> u64 {
    if name.starts_with("native/") {
        MAXIMUM_NATIVE_BYTES
    } else {
        MAXIMUM_METADATA_BYTES
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn require(condition: bool, message: &str) -> Result<(), BundleError> {
    if condition {
        Ok(())
    } else {
        Err(BundleError(message.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::{create_bundle, inspect_bundle, safe_entry_name, BundleInputs};
    use crate::manifest::{LINUX_TARGET, WINDOWS_TARGET};
    use crate::version::RuntimeVersion;
    use flate2::read::GzDecoder;
    use std::collections::BTreeMap;
    use std::fs;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use tar::Archive;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;
    use zip::ZipArchive;
    use zip::ZipWriter;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const NATIVE_BYTES: &[u8] = b"not a real library, only an archive fixture";

    #[test]
    fn archive_entry_names_use_canonical_forward_slashes() {
        assert_eq!(
            "native/compukter_ffi.dll",
            safe_entry_name("native/compukter_ffi.dll").unwrap()
        );
        for unsafe_name in [
            "native\\compukter_ffi.dll",
            "/native/library",
            "native//library",
            "native/../library",
            "native/./library",
        ] {
            assert!(safe_entry_name(unsafe_name).is_err());
        }
    }

    struct Fixture {
        root: TempDir,
        native: PathBuf,
        license: PathBuf,
        notice: PathBuf,
    }

    impl Fixture {
        fn new(filename: &str) -> Self {
            let root = tempfile::tempdir().unwrap();
            let native = root.path().join(filename);
            let license = root.path().join("LICENSE.txt");
            let notice = root.path().join("NOTICE.txt");
            fs::write(&native, NATIVE_BYTES).unwrap();
            fs::write(&license, "license\n").unwrap();
            fs::write(&notice, "notice\n").unwrap();
            Self {
                root,
                native,
                license,
                notice,
            }
        }

        fn inputs<'a>(&'a self, target: &'a str) -> BundleInputs<'a> {
            BundleInputs {
                runtime_version: RuntimeVersion::parse("5.0").unwrap(),
                release_tag: "runtime-v5.0",
                vm_commit: COMMIT,
                rustc: "rustc 1.98.0 (88d9e12ae 2026-08-18)",
                target,
                native_library: &self.native,
                license: &self.license,
                notice: &self.notice,
                formats: BTreeMap::from([
                    ("artifact".to_owned(), 2),
                    ("compilation-request".to_owned(), 1),
                    ("executable-revision".to_owned(), 1),
                    ("filesystem-generation".to_owned(), 1),
                ]),
            }
        }
    }

    #[test]
    fn creates_and_reinspects_the_exact_linux_layout() {
        let fixture = Fixture::new("libcompukter_ffi.so");
        let bundle = create_bundle(&fixture.inputs(LINUX_TARGET), fixture.root.path()).unwrap();

        assert_eq!(
            "compukter-runtime-5.0-linux-x86_64.tar.gz",
            bundle.file_name().unwrap()
        );
        assert_eq!(
            vec![
                "native/libcompukter_ffi.so",
                "manifest.json",
                "LICENSE.txt",
                "NOTICE.txt",
            ],
            tar_entry_names(&bundle)
        );
        let manifest = inspect_bundle(&bundle).unwrap();
        assert_eq!(NATIVE_BYTES.len() as u64, manifest.size);
        assert_eq!(LINUX_TARGET, manifest.target);
    }

    #[test]
    fn creates_and_reinspects_the_exact_windows_layout() {
        let fixture = Fixture::new("compukter_ffi.dll");
        let bundle = create_bundle(&fixture.inputs(WINDOWS_TARGET), fixture.root.path()).unwrap();

        assert_eq!(
            "compukter-runtime-5.0-windows-x86_64.zip",
            bundle.file_name().unwrap()
        );
        assert_eq!(
            vec![
                "native/compukter_ffi.dll",
                "manifest.json",
                "LICENSE.txt",
                "NOTICE.txt",
            ],
            zip_entry_names(&bundle)
        );
        let manifest = inspect_bundle(&bundle).unwrap();
        assert_eq!(NATIVE_BYTES.len() as u64, manifest.size);
        assert_eq!(WINDOWS_TARGET, manifest.target);
    }

    #[test]
    fn rejects_a_target_filename_mismatch_before_writing_output() {
        let fixture = Fixture::new("compukter_ffi.dll");

        assert!(create_bundle(&fixture.inputs(LINUX_TARGET), fixture.root.path()).is_err());
        assert!(!fixture
            .root
            .path()
            .join("compukter-runtime-5.0-linux-x86_64.tar.gz")
            .exists());
    }

    #[test]
    fn emits_reproducible_linux_and_windows_archives() {
        for (target, filename) in [
            (LINUX_TARGET, "libcompukter_ffi.so"),
            (WINDOWS_TARGET, "compukter_ffi.dll"),
        ] {
            let fixture = Fixture::new(filename);
            let first_dir = fixture.root.path().join("first");
            let second_dir = fixture.root.path().join("second");
            let first = create_bundle(&fixture.inputs(target), &first_dir).unwrap();
            let second = create_bundle(&fixture.inputs(target), &second_dir).unwrap();

            assert_eq!(fs::read(first).unwrap(), fs::read(second).unwrap());
        }
    }

    #[test]
    fn inspection_rejects_an_extra_archive_entry() {
        let fixture = Fixture::new("compukter_ffi.dll");
        let bundle = create_bundle(&fixture.inputs(WINDOWS_TARGET), fixture.root.path()).unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&bundle)
            .unwrap();
        let mut archive = ZipWriter::new_append(file).unwrap();
        archive
            .start_file("extra.txt", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"unexpected").unwrap();
        archive.finish().unwrap();

        assert!(inspect_bundle(&bundle).is_err());
    }

    fn tar_entry_names(path: &Path) -> Vec<String> {
        let archive = fs::File::open(path).unwrap();
        Archive::new(GzDecoder::new(archive))
            .entries()
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    fn zip_entry_names(path: &Path) -> Vec<String> {
        let mut archive = ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
        (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_owned())
            .collect()
    }
}
