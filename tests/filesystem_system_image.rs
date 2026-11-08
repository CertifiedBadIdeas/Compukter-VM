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

use std::sync::Arc;

use compukter_vm::{
    ComputerFileSystem, FileCapability, FileRights, FileSystemError, FileSystemLimits, SystemImage,
    SystemImageError, VirtualPath,
};
use sha2::{Digest, Sha256};

struct Entry<'a> {
    path: &'a str,
    kind: u8,
    flags: u8,
    reserved: u16,
    content: &'a [u8],
}

fn image(entries: &[Entry<'_>], trailing: &[u8]) -> Arc<[u8]> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"CPKTSYS\0");
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        bytes.extend_from_slice(&(entry.path.len() as u32).to_le_bytes());
        bytes.extend_from_slice(entry.path.as_bytes());
        bytes.push(entry.kind);
        bytes.push(entry.flags);
        bytes.extend_from_slice(&entry.reserved.to_le_bytes());
        bytes.extend_from_slice(&(entry.content.len() as u64).to_le_bytes());
        bytes.extend_from_slice(entry.content);
    }
    bytes.extend_from_slice(trailing);
    let digest = Sha256::digest(&bytes);
    bytes.extend_from_slice(&digest);
    bytes.into()
}

fn valid_image() -> Arc<[u8]> {
    image(
        &[
            Entry {
                path: "/bin/bin",
                kind: 1,
                flags: 0,
                reserved: 0,
                content: b"",
            },
            Entry {
                path: "/bin/bin/tool",
                kind: 2,
                flags: 1,
                reserved: 0,
                content: b"tool artifact",
            },
            Entry {
                path: "/boot/init",
                kind: 2,
                flags: 1,
                reserved: 0,
                content: b"boot artifact",
            },
        ],
        b"",
    )
}

fn path(value: &str) -> VirtualPath {
    VirtualPath::parse_utf16(
        &value.encode_utf16().collect::<Vec<_>>(),
        &FileSystemLimits::testing(),
    )
    .unwrap()
}

#[test]
fn admitted_system_is_hash_identified_writable_and_capability_checked() {
    let limits = FileSystemLimits::testing();
    let bytes = valid_image();
    let image = SystemImage::admit(bytes.clone(), &limits).unwrap();
    let expected: [u8; 32] = bytes[bytes.len() - 32..].try_into().unwrap();
    assert_eq!(image.identity(), expected);
    assert_eq!(
        SystemImage::admit(bytes, &limits).unwrap().identity(),
        expected
    );

    let mut filesystem = ComputerFileSystem::with_system_image(limits, image).unwrap();
    let reader = FileCapability::new(
        path("/"),
        FileRights::INSPECT | FileRights::LIST | FileRights::READ | FileRights::EXECUTE,
    );
    assert_eq!(
        filesystem.read_file_for_test(&path("/boot/init")).unwrap(),
        b"boot artifact",
    );
    assert!(
        filesystem
            .stat(&reader, &path("/boot/init"))
            .unwrap()
            .executable
    );
    assert_eq!(
        filesystem
            .read_executable(&reader, &path("/boot/init"))
            .unwrap()
            .as_ref(),
        b"boot artifact",
    );
    assert_eq!(
        filesystem.write_file(&reader, &path("/boot/init"), b"changed", true),
        Err(FileSystemError::PermissionDenied),
    );
}

#[test]
fn malformed_noncanonical_and_over_limit_images_are_rejected() {
    let limits = FileSystemLimits::testing();
    let cases = [
        image(
            &[
                Entry {
                    path: "/bin/a",
                    kind: 1,
                    flags: 0,
                    reserved: 0,
                    content: b"",
                },
                Entry {
                    path: "/bin/a",
                    kind: 1,
                    flags: 0,
                    reserved: 0,
                    content: b"",
                },
            ],
            b"",
        ),
        image(
            &[Entry {
                path: "/home/a",
                kind: 2,
                flags: 0,
                reserved: 0,
                content: b"x",
            }],
            b"",
        ),
        image(
            &[Entry {
                path: "/bin/missing/a",
                kind: 2,
                flags: 0,
                reserved: 0,
                content: b"x",
            }],
            b"",
        ),
        image(
            &[Entry {
                path: "/bin/a",
                kind: 1,
                flags: 1,
                reserved: 0,
                content: b"",
            }],
            b"",
        ),
        image(
            &[Entry {
                path: "/bin/a",
                kind: 2,
                flags: 2,
                reserved: 0,
                content: b"x",
            }],
            b"",
        ),
        image(
            &[Entry {
                path: "/bin/a",
                kind: 2,
                flags: 0,
                reserved: 1,
                content: b"x",
            }],
            b"",
        ),
        image(
            &[
                Entry {
                    path: "/bin/z",
                    kind: 2,
                    flags: 0,
                    reserved: 0,
                    content: b"z",
                },
                Entry {
                    path: "/bin/a",
                    kind: 2,
                    flags: 0,
                    reserved: 0,
                    content: b"a",
                },
            ],
            b"",
        ),
        image(
            &[Entry {
                path: "/bin/a",
                kind: 2,
                flags: 0,
                reserved: 0,
                content: b"x",
            }],
            b"trailing",
        ),
    ];
    for bytes in cases {
        assert!(matches!(
            SystemImage::admit(bytes, &limits),
            Err(SystemImageError::Malformed | SystemImageError::NonCanonical)
        ));
    }

    let mut small = limits;
    small.maximum_system_image_bytes = 32;
    assert_eq!(
        SystemImage::admit(valid_image(), &small),
        Err(SystemImageError::LimitExceeded),
    );
}

#[test]
fn digest_and_version_are_admission_boundaries() {
    let limits = FileSystemLimits::testing();
    let mut corrupt = valid_image().to_vec();
    corrupt[20] ^= 1;
    assert_eq!(
        SystemImage::admit(corrupt.into(), &limits),
        Err(SystemImageError::DigestMismatch),
    );

    let mut unsupported = valid_image().to_vec();
    unsupported[8..10].copy_from_slice(&2_u16.to_le_bytes());
    let payload = unsupported.len() - 32;
    let digest = Sha256::digest(&unsupported[..payload]);
    unsupported[payload..].copy_from_slice(&digest);
    assert_eq!(
        SystemImage::admit(unsupported.into(), &limits),
        Err(SystemImageError::UnsupportedVersion),
    );
}
