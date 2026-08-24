/*
 * The Compukters Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 */

use std::sync::Arc;

use compukter_vm::{
    ComputerFileSystem, FileCapability, FileRights, FileSystemError, FileSystemLimits, RomImage,
    RomImageError, VirtualPath,
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
    bytes.extend_from_slice(b"CPKTROM\0");
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
                path: "/rom/bin",
                kind: 1,
                flags: 0,
                reserved: 0,
                content: b"",
            },
            Entry {
                path: "/rom/bin/tool",
                kind: 2,
                flags: 1,
                reserved: 0,
                content: b"tool artifact",
            },
            Entry {
                path: "/rom/boot",
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
fn admitted_rom_is_hash_identified_mounted_and_immutable() {
    let limits = FileSystemLimits::testing();
    let bytes = valid_image();
    let image = RomImage::admit(bytes.clone(), &limits).unwrap();
    let expected: [u8; 32] = bytes[bytes.len() - 32..].try_into().unwrap();
    assert_eq!(image.identity(), expected);
    assert_eq!(
        RomImage::admit(bytes, &limits).unwrap().identity(),
        expected
    );

    let mut filesystem = ComputerFileSystem::with_rom(limits, image).unwrap();
    let reader = FileCapability::new(
        path("/rom"),
        FileRights::INSPECT | FileRights::LIST | FileRights::READ | FileRights::EXECUTE,
    );
    assert_eq!(
        filesystem.read_file_for_test(&path("/rom/boot")).unwrap(),
        b"boot artifact",
    );
    assert!(
        filesystem
            .stat(&reader, &path("/rom/boot"))
            .unwrap()
            .executable
    );
    assert_eq!(
        filesystem
            .read_executable(&reader, &path("/rom/boot"))
            .unwrap(),
        b"boot artifact",
    );
    assert_eq!(
        filesystem.write_file(&reader, &path("/rom/boot"), b"changed", true),
        Err(FileSystemError::ReadOnly),
    );
}

#[test]
fn malformed_noncanonical_and_over_limit_images_are_rejected() {
    let limits = FileSystemLimits::testing();
    let cases = [
        image(
            &[
                Entry {
                    path: "/rom/a",
                    kind: 1,
                    flags: 0,
                    reserved: 0,
                    content: b"",
                },
                Entry {
                    path: "/rom/a",
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
                path: "/rom/missing/a",
                kind: 2,
                flags: 0,
                reserved: 0,
                content: b"x",
            }],
            b"",
        ),
        image(
            &[Entry {
                path: "/rom/a",
                kind: 1,
                flags: 1,
                reserved: 0,
                content: b"",
            }],
            b"",
        ),
        image(
            &[Entry {
                path: "/rom/a",
                kind: 2,
                flags: 2,
                reserved: 0,
                content: b"x",
            }],
            b"",
        ),
        image(
            &[Entry {
                path: "/rom/a",
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
                    path: "/rom/z",
                    kind: 2,
                    flags: 0,
                    reserved: 0,
                    content: b"z",
                },
                Entry {
                    path: "/rom/a",
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
                path: "/rom/a",
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
            RomImage::admit(bytes, &limits),
            Err(RomImageError::Malformed | RomImageError::NonCanonical)
        ));
    }

    let mut small = limits;
    small.maximum_rom_bytes = 32;
    assert_eq!(
        RomImage::admit(valid_image(), &small),
        Err(RomImageError::LimitExceeded),
    );
}

#[test]
fn digest_and_version_are_admission_boundaries() {
    let limits = FileSystemLimits::testing();
    let mut corrupt = valid_image().to_vec();
    corrupt[20] ^= 1;
    assert_eq!(
        RomImage::admit(corrupt.into(), &limits),
        Err(RomImageError::DigestMismatch),
    );

    let mut unsupported = valid_image().to_vec();
    unsupported[8..10].copy_from_slice(&2_u16.to_le_bytes());
    let payload = unsupported.len() - 32;
    let digest = Sha256::digest(&unsupported[..payload]);
    unsupported[payload..].copy_from_slice(&digest);
    assert_eq!(
        RomImage::admit(unsupported.into(), &limits),
        Err(RomImageError::UnsupportedVersion),
    );
}
