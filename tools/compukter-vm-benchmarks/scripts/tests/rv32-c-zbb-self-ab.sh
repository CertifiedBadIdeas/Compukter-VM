#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
BUILD_ROOT="${RV32_C_ZBB_AB_BUILD_DIR:-$ROOT/target/rv32-c-zbb-self-ab}"
WARM_SAMPLES="${RV32_C_ZBB_AB_SAMPLES:-21}"
DBT_SETS="${RV32_C_ZBB_AB_DBT_SETS:-256}"
BASELINE_DIR="$BUILD_ROOT/baseline"
CANDIDATE_DIR="$BUILD_ROOT/zbb"

for tool in cargo clang ld.lld llvm-objdump awk grep tee; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "required RV32 C Zbb self-A/B tool is unavailable: $tool" >&2
        exit 2
    fi
done

mkdir -p "$BASELINE_DIR" "$CANDIDATE_DIR"
RV32_C_RV32_MARCH=rv32im_zicsr \
    bash "$ROOT/scripts/compile-rv32-c-comparison.sh" "$BASELINE_DIR"
RV32_C_RV32_MARCH=rv32im_zicsr_zbb \
    bash "$ROOT/scripts/compile-rv32-c-comparison.sh" "$CANDIDATE_DIR"

zbb_pattern='\b(andn|orn|xnor|clz|ctz|cpop|minu?|maxu?|sext\.[bh]|zext\.h|rol|ror|rori|orc\.b|rev8)\b'
baseline_zbb="$(llvm-objdump --mattr=+zbb -d "$BASELINE_DIR/product.elf" | grep -Ec "$zbb_pattern" || true)"
candidate_zbb="$(llvm-objdump --mattr=+zbb -d "$CANDIDATE_DIR/product.elf" | grep -Ec "$zbb_pattern" || true)"
if [[ "$baseline_zbb" -ne 0 || "$candidate_zbb" -eq 0 ]]; then
    echo "unexpected Zbb codegen counts: baseline=$baseline_zbb candidate=$candidate_zbb" >&2
    exit 1
fi

cargo run --manifest-path "$ROOT/Cargo.toml" --release --locked --offline \
    --features dbt-translation-timing -p compukter-vm-benchmarks --bin rv32_c_comparison -- \
    codegen-self-ab "$BASELINE_DIR" "$CANDIDATE_DIR" "$WARM_SAMPLES" "$DBT_SETS" \
    | tee "$BUILD_ROOT/report.tsv"

awk -F '\t' -v samples="$WARM_SAMPLES" '
    $1 == "codegen_self_ab" && $4 == samples { metadata++ }
    $1 == "baseline" && $4 == "ee053d58" && $15 == 0 && $16 == 0 { rows++ }
    $1 == "candidate" && $4 == "ee053d58" && $15 == 0 && $16 == 0 { rows++ }
    END { exit metadata == 1 && rows == 2 ? 0 : 1 }
' "$BUILD_ROOT/report.tsv"

echo "Focused RV32 C Zbb self-A/B passed; Zbb instructions=$candidate_zbb; report: $BUILD_ROOT/report.tsv"
