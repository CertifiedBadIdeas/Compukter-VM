/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

use std::fs;
use std::path::PathBuf;

#[test]
fn elf_fixture_builders_only_read_local_fixture_sources() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for script in [
        "compile-rv32-elf-boot-fixture.sh",
        "compile-rv32-elf-trap-fixture.sh",
        "compile-rv32-elf-atomic-fixture.sh",
    ] {
        let source = fs::read_to_string(root.join("scripts").join(script)).unwrap();
        assert!(source.contains("$ROOT/fixtures/"));
        assert!(!source.contains("tools/fixtures"));
        assert!(!source.contains("../"));
    }
}
