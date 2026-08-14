#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
BUILD_DIR="${RV32_C_COMPARISON_BUILD_DIR:-$ROOT/target/rv32-c-comparison}"

for tool in cargo awk tee; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "required register/alignment matrix tool is unavailable: $tool" >&2
        exit 2
    fi
done

mkdir -p "$BUILD_DIR"
bash "$ROOT/scripts/compile-rv32-c-comparison.sh" "$BUILD_DIR"

cargo run --manifest-path "$ROOT/Cargo.toml" --release --locked --offline \
    --example rv32_c_comparison -- \
    register-alignment-matrix "$BUILD_DIR" 21 \
    | tee "$BUILD_DIR/register-alignment-matrix-report.tsv"

awk -F '\t' '
    $1 ~ /^(stable7-base32|stable7-base64|rcx8-base32|rcx8-base64)$/ &&
        $6 == "ee053d58" && $23 == 0 && $24 == 0 { candidates++ }
    $1 == "register_alignment_interaction" && $2 > 0 { interaction++ }
    END { exit candidates == 4 && interaction == 1 ? 0 : 1 }
' "$BUILD_DIR/register-alignment-matrix-report.tsv"

echo "RV32 optimized C register/alignment matrix passed; report: $BUILD_DIR/register-alignment-matrix-report.tsv"
