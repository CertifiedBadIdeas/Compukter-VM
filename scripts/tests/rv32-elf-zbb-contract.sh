#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
ARTIFACTS="$(mktemp -d)"
trap 'rm -rf "$ARTIFACTS"' EXIT HUP INT TERM

first="$ARTIFACTS/rv32-zbb-first.elf"
second="$ARTIFACTS/rv32-zbb-second.elf"
"$ROOT/scripts/compile-rv32-elf-zbb-fixture.sh" "$first"
"$ROOT/scripts/compile-rv32-elf-zbb-fixture.sh" "$second"
cmp "$first" "$second"

RV32_ELF_ZBB_FIXTURE="$first" \
    cargo test --locked --offline --manifest-path "$ROOT/Cargo.toml" \
    --test rv32_elf_zbb stock_toolchain_rv32_zbb_elf_executes_on_every_tier0_backend \
    -- --ignored --exact

echo "RV32 ELF Zbb contract passed"
