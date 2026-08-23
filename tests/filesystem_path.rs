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

use compukter_vm::{FileSystemError, FileSystemLimits, VirtualPath};

fn units(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}

fn path(value: &str, limits: &FileSystemLimits) -> VirtualPath {
    VirtualPath::parse_utf16(&units(value), limits).unwrap()
}

#[test]
fn paths_are_absolute_exact_and_unicode_scalar_based() {
    let limits = FileSystemLimits::testing();

    let root = path("/", &limits);
    let source = path("/home/λ/😀.kt", &limits);

    assert_eq!(root.to_string(), "/");
    assert_eq!(source.to_string(), "/home/λ/😀.kt");
    assert_eq!(
        source.components().collect::<Vec<_>>(),
        ["home", "λ", "😀.kt"]
    );
    assert_eq!(source.file_name(), Some("😀.kt"));
    assert_eq!(source.parent().unwrap().to_string(), "/home/λ");
}

#[test]
fn invalid_virtual_path_forms_are_rejected_without_normalization() {
    let limits = FileSystemLimits::testing();
    for invalid in [
        "",
        "home/a",
        "/home/",
        "/home//a",
        "/home/./a",
        "/home/../a",
        "/home/a\0b",
    ] {
        assert_eq!(
            VirtualPath::parse_utf16(&units(invalid), &limits),
            Err(FileSystemError::InvalidPath),
            "{invalid:?}",
        );
    }
    assert_eq!(
        VirtualPath::parse_utf16(&[b'/' as u16, 0xD800], &limits),
        Err(FileSystemError::InvalidPath),
    );
    assert_ne!(path("/home/é", &limits), path("/home/e\u{301}", &limits));
}

#[test]
fn independent_path_limits_are_enforced_at_the_boundary() {
    let mut limits = FileSystemLimits::testing();
    limits.maximum_path_bytes = 8;
    limits.maximum_component_bytes = 4;
    limits.maximum_components = 2;

    assert_eq!(path("/ab/cd", &limits).to_string(), "/ab/cd");
    assert_eq!(
        VirtualPath::parse_utf16(&units("/abcde"), &limits),
        Err(FileSystemError::InvalidPath),
    );
    assert_eq!(
        VirtualPath::parse_utf16(&units("/ab/cd/e"), &limits),
        Err(FileSystemError::InvalidPath),
    );
    assert_eq!(
        VirtualPath::parse_utf16(&units("/abcd/efg"), &limits),
        Err(FileSystemError::InvalidPath),
    );
}

#[test]
fn subtree_checks_compare_components_not_text_prefixes() {
    let limits = FileSystemLimits::testing();
    let home = path("/home", &limits);

    assert!(path("/home", &limits).is_within(&home));
    assert!(path("/home/project/main.kt", &limits).is_within(&home));
    assert!(!path("/homebrew/file", &limits).is_within(&home));
    assert!(!home.is_within(&path("/home/project", &limits)));
}

#[test]
fn path_order_is_deterministic_exact_scalar_order() {
    let limits = FileSystemLimits::testing();
    let mut paths = ["/home/😀", "/home/z", "/home/λ", "/home/a"].map(|value| path(value, &limits));

    paths.sort();

    assert_eq!(
        paths.map(|value| value.to_string()),
        ["/home/a", "/home/z", "/home/λ", "/home/😀"],
    );
}
