#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
BUILD_DIR="${RV32_C_CODEGEN_BUILD_DIR:-$ROOT/target/rv32-c-codegen-audit}"
: "${RV32_C_CLANG:=clang}"
: "${RV32_C_LLD:=ld.lld}"
: "${RV32_C_OBJDUMP:=llvm-objdump}"
: "${RV32_C_WASMTIME:=wasmtime}"
: "${RV32_C_PERF:=perf}"

for tool in "$RV32_C_CLANG" "$RV32_C_LLD" "$RV32_C_OBJDUMP" "$RV32_C_WASMTIME" cargo grep sha256sum; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "required RV32 C codegen audit tool is unavailable: $tool" >&2
        exit 2
    fi
done
if [[ "$("$RV32_C_WASMTIME" --version | awk '{print $2}')" != "47.0.3" ]]; then
    echo "RV32 C codegen audit requires Wasmtime CLI 47.0.3" >&2
    exit 2
fi

mkdir -p "$BUILD_DIR"
bash "$ROOT/scripts/compile-rv32-c-comparison.sh" "$BUILD_DIR"

"$RV32_C_WASMTIME" compile -O opt-level=2 -o "$BUILD_DIR/module.cwasm" "$BUILD_DIR/module.wasm"
"$RV32_C_WASMTIME" objdump "$BUILD_DIR/module.cwasm" >"$BUILD_DIR/wasmtime-aot-objdump.txt"
wasmtime_output="$("$RV32_C_WASMTIME" run --allow-precompiled --invoke benchmark_batch \
    "$BUILD_DIR/module.cwasm" 1000 305419896 1 2>"$BUILD_DIR/wasmtime-invoke-warnings.txt")"
if [[ "$wasmtime_output" != "-301646504" ]]; then
    echo "Wasmtime audit oracle mismatch: $wasmtime_output" >&2
    exit 1
fi

cargo run --manifest-path "$ROOT/Cargo.toml" --release --locked --offline \
    --features dbt-code-audit,dbt-execution-profile --example rv32_c_codegen_audit -- export "$BUILD_DIR"
test -s "$BUILD_DIR/dbt-support.tsv"
test -s "$BUILD_DIR/dbt-register-pressure.tsv"
test -s "$BUILD_DIR/dbt-register-pressure-weighted.tsv"
"$RV32_C_CLANG" -c "$BUILD_DIR/dbt-code-cache.S" -o "$BUILD_DIR/dbt-code-cache.o"
"$RV32_C_OBJDUMP" -d "$BUILD_DIR/dbt-code-cache.o" >"$BUILD_DIR/dbt-disassembly.txt"

"$RV32_C_CLANG" -O3 -march=native -fno-lto -Wall -Wextra -Werror \
    -c "$ROOT/benchmarks/rv32-c-comparison/kernel.c" -o "$BUILD_DIR/native-analysis.o"
"$RV32_C_OBJDUMP" -d --disassemble-symbols=benchmark_kernel \
    "$BUILD_DIR/native-analysis.o" >"$BUILD_DIR/native-analysis-disassembly.txt"

cargo run --manifest-path "$ROOT/Cargo.toml" --release --locked --offline \
    --features dbt-code-audit,dbt-execution-profile --example rv32_c_codegen_audit -- report "$BUILD_DIR"
test -s "$BUILD_DIR/codegen-report.tsv"

AUDIT_BATCH=1024
"$RV32_C_LLD" -m elf32lriscv --no-relax --fatal-warnings --defsym=__ck_batch="$AUDIT_BATCH" \
    -T "$ROOT/benchmarks/rv32-c-comparison/product.ld" "$BUILD_DIR/product-start.o" \
    "$BUILD_DIR/product-wrapper.o" "$BUILD_DIR/kernel-rv32.o" \
    -o "$BUILD_DIR/product-audit-batch.elf"
cargo build --manifest-path "$ROOT/Cargo.toml" --release --locked --offline \
    --features dbt-code-audit,dbt-execution-profile --example rv32_c_codegen_audit

PERF_REPORT="$BUILD_DIR/perf-report.tsv"
printf 'system\tstatus\tcycles\tinstructions\tipc\tbranches\tbranch_misses\tcache_misses\tcommand\n' >"$PERF_REPORT"
perf_value() {
    local raw="$1"
    local event="$2"
    awk -F '\t' -v event="$event" '
        $3 == event {
            gsub(/,/, "", $1)
            if ($1 ~ /^[0-9]+$/) print $1; else print "-"
            found = 1
        }
        END { if (!found) print "-" }
    ' "$raw"
}
perf_measure() {
    local system="$1"
    local expected="$2"
    shift
    shift
    local raw="$BUILD_DIR/perf-$system.txt"
    if ! "$RV32_C_PERF" stat -x $'\t' -o "$raw" \
        -e cycles,instructions,branches,branch-misses,cache-misses -- "$@" \
        >"$BUILD_DIR/perf-$system.stdout" 2>"$BUILD_DIR/perf-$system.stderr"; then
        return 1
    fi
    if [[ "$(<"$BUILD_DIR/perf-$system.stdout")" != "$expected" ]]; then
        return 1
    fi
    local cycles instructions branches branch_misses cache_misses ipc command
    cycles="$(perf_value "$raw" cycles)"
    instructions="$(perf_value "$raw" instructions)"
    branches="$(perf_value "$raw" branches)"
    branch_misses="$(perf_value "$raw" branch-misses)"
    cache_misses="$(perf_value "$raw" cache-misses)"
    ipc="-"
    if [[ "$cycles" =~ ^[0-9]+$ && "$instructions" =~ ^[0-9]+$ && "$cycles" -ne 0 ]]; then
        ipc="$(awk -v cycles="$cycles" -v instructions="$instructions" \
            'BEGIN { printf "%.6f", instructions / cycles }')"
    fi
    printf -v command '%q ' "$@"
    command="${command% }"
    printf '%s\tavailable\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$system" "$cycles" "$instructions" "$ipc" "$branches" "$branch_misses" \
        "$cache_misses" "$command" >>"$PERF_REPORT"
}

PERF_REASON=""
if ! command -v "$RV32_C_PERF" >/dev/null 2>&1; then
    PERF_REASON="perf command is unavailable"
elif ! "$RV32_C_PERF" stat -x $'\t' -o "$BUILD_DIR/perf-probe.txt" \
    -e cycles,instructions,branches,branch-misses,cache-misses -- true \
    >"$BUILD_DIR/perf-probe.stdout" 2>"$BUILD_DIR/perf-probe.stderr"; then
    PERF_REASON="$(tr '\t\n' '  ' <"$BUILD_DIR/perf-probe.stderr")"
elif ! perf_measure native $'CK_RESULT\tee053d58' \
    "$BUILD_DIR/native-kernel" 1000 0x12345678 4096 || \
    ! perf_measure wasmtime "-301646504" "$RV32_C_WASMTIME" run --allow-precompiled \
        --invoke benchmark_batch "$BUILD_DIR/module.cwasm" 1000 305419896 2048 || \
    ! perf_measure rv32-cached-dbt $'CK_RESULT\tee053d58' \
        "$ROOT/target/release/examples/rv32_c_codegen_audit" \
        execute "$BUILD_DIR" "$AUDIT_BATCH"; then
    PERF_REASON="perf measurement or checksum validation failed"
fi
if [[ -n "$PERF_REASON" ]]; then
    PERF_REASON="$(printf '%s' "$PERF_REASON" | tr '\t\n' '  ')"
    printf 'system\tstatus\tcycles\tinstructions\tipc\tbranches\tbranch_misses\tcache_misses\tcommand\nstatus\tunavailable\t-\t-\t-\t-\t-\t-\t-\nreason\t%s\t-\t-\t-\t-\t-\t-\t-\n' \
        "$PERF_REASON" >"$PERF_REPORT"
fi

sha256sum "$BUILD_DIR/dbt-code-cache.bin" >"$BUILD_DIR/dbt-code-cache.sha256"
echo "Focused RV32 C codegen audit passed; artifacts: $BUILD_DIR"
