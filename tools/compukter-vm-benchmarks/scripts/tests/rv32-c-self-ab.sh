#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
BUILD_DIR="${RV32_C_COMPARISON_BUILD_DIR:-$ROOT/target/rv32-c-comparison}"
BASELINE="${RV32_C_SELF_AB_BASELINE:-rv32-cached-dbt-block-16}"
CANDIDATE="${RV32_C_SELF_AB_CANDIDATE:-rv32-cached-dbt-block-32}"
WARM_SAMPLES="${RV32_C_SELF_AB_SAMPLES:-21}"

for tool in cargo clang ld.lld awk tee; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "required focused self-A/B tool is unavailable: $tool" >&2
        exit 2
    fi
done

mkdir -p "$BUILD_DIR"
bash "$ROOT/scripts/compile-rv32-c-comparison.sh" "$BUILD_DIR"

cargo run --manifest-path "$ROOT/Cargo.toml" --release \
    --features dbt-translation-timing \
    -p compukter-vm-benchmarks --bin rv32_c_comparison --locked --offline -- \
    self-ab "$BUILD_DIR" "$BASELINE" "$CANDIDATE" "$WARM_SAMPLES" \
    | tee "$BUILD_DIR/self-ab-report.tsv"

awk -F '\t' -v baseline="$BASELINE" -v candidate="$CANDIDATE" -v samples="$WARM_SAMPLES" '
    $1 == "self_ab" && $2 == baseline && $3 == candidate && $4 == samples { metadata++ }
    $1 == "baseline" && $2 == baseline && $4 == "ee053d58" && $25 == 0 && $26 == 0 { rows++ }
    $1 == "candidate" && $2 == candidate && $4 == "ee053d58" && $25 == 0 && $26 == 0 { rows++ }
    $1 == "self_ab_phase" && $2 ~ /^(baseline|candidate)$/ && $4 ~ /^(lift|lower|publish)$/ && $5 == samples { phases++ }
    END { exit metadata == 1 && rows == 2 && phases == 6 ? 0 : 1 }
' "$BUILD_DIR/self-ab-report.tsv"

echo "Focused RV32 DBT self-A/B comparison passed; report: $BUILD_DIR/self-ab-report.tsv"
