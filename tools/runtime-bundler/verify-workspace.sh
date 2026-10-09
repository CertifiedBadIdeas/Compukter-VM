#!/usr/bin/env bash
# The Compukters Developers
# Copyright 2026 Vsevolod Petrov (lazyhat)
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

cargo xtask check
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings
cargo test --workspace --locked --offline
cargo test -p compukter-vm --test golden_fixtures --locked --offline
cargo test --release --test bounded_failures --locked --offline
cargo doc --workspace --no-deps --document-private-items --locked --offline
