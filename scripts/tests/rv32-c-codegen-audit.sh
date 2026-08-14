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
test -s "$BUILD_DIR/dbt-audit-config.tsv"
test -s "$BUILD_DIR/dbt-execution-profile.tsv"
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
test -s "$BUILD_DIR/dbt-hot-blocks.tsv"

AUDIT_BATCH=1024
"$RV32_C_LLD" -m elf32lriscv --no-relax --fatal-warnings --defsym=__ck_batch="$AUDIT_BATCH" \
    -T "$ROOT/benchmarks/rv32-c-comparison/product.ld" "$BUILD_DIR/product-start.o" \
    "$BUILD_DIR/product-wrapper.o" "$BUILD_DIR/kernel-rv32.o" \
    -o "$BUILD_DIR/product-audit-batch.elf"
cargo build --manifest-path "$ROOT/Cargo.toml" --release --locked --offline \
    --features dbt-code-audit,dbt-execution-profile --example rv32_c_codegen_audit

PERF_REPORT="$BUILD_DIR/perf-report.tsv"
PERF_SAMPLES_REPORT="$BUILD_DIR/perf-samples.tsv"
PERF_SAMPLES="${RV32_C_PERF_SAMPLES:-7}"
if [[ ! "$PERF_SAMPLES" =~ ^[1-9][0-9]*$ ]]; then
    echo "RV32_C_PERF_SAMPLES must be positive" >&2
    exit 2
fi
printf 'system\tsample\tbatch\tcycles\tinstructions\tipc\tbranches\tbranch_misses\tcache_misses\tcommand\n' >"$PERF_SAMPLES_REPORT"
perf_value() {
    local raw="$1"
    local event="$2"
    awk -F '\t' -v event="$event" '
        $3 == event || index($3, event ":") == 1 {
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
    local batch="$3"
    shift
    shift
    shift
    local sample raw stdout stderr cycles instructions branches branch_misses cache_misses ipc command
    for ((sample = 1; sample <= PERF_SAMPLES; sample++)); do
        raw="$BUILD_DIR/perf-$system-$sample.txt"
        stdout="$BUILD_DIR/perf-$system-$sample.stdout"
        stderr="$BUILD_DIR/perf-$system-$sample.stderr"
        if ! "$RV32_C_PERF" stat -x $'\t' -o "$raw" \
            -e cycles,instructions,branches,branch-misses,cache-misses -- "$@" \
            >"$stdout" 2>"$stderr"; then
            return 1
        fi
        if [[ "$(<"$stdout")" != "$expected" ]]; then
            return 1
        fi
        cycles="$(perf_value "$raw" cycles)"
        instructions="$(perf_value "$raw" instructions)"
        branches="$(perf_value "$raw" branches)"
        branch_misses="$(perf_value "$raw" branch-misses)"
        cache_misses="$(perf_value "$raw" cache-misses)"
        if [[ ! "$cycles" =~ ^[0-9]+$ || ! "$instructions" =~ ^[0-9]+$ || \
              ! "$branches" =~ ^[0-9]+$ || ! "$branch_misses" =~ ^[0-9]+$ || \
              ! "$cache_misses" =~ ^[0-9]+$ || "$cycles" -eq 0 ]]; then
            return 1
        fi
        ipc="$(awk -v cycles="$cycles" -v instructions="$instructions" \
            'BEGIN { printf "%.6f", instructions / cycles }')"
        printf -v command '%q ' "$@"
        command="${command% }"
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
            "$system" "$sample" "$batch" "$cycles" "$instructions" "$ipc" "$branches" \
            "$branch_misses" "$cache_misses" "$command" >>"$PERF_SAMPLES_REPORT"
    done
}
perf_summarize() {
    awk -F '\t' '
        NR > 1 {
            count[$1]++
            batch[$1] = $3
            for (column = 4; column <= 9; column++) {
                value[$1, column, count[$1]] = $column + 0
            }
        }
        function median(owner, column, size, sorted, i, j, current) {
            size = count[owner]
            for (i = 1; i <= size; i++) sorted[i] = value[owner, column, i]
            for (i = 2; i <= size; i++) {
                current = sorted[i]
                j = i - 1
                while (j >= 1 && sorted[j] > current) {
                    sorted[j + 1] = sorted[j]
                    j--
                }
                sorted[j + 1] = current
            }
            if (size % 2) return sorted[(size + 1) / 2]
            return (sorted[size / 2] + sorted[size / 2 + 1]) / 2
        }
        function emit(owner, cycles, instructions, branches, branch_misses) {
            cycles = median(owner, 4)
            instructions = median(owner, 5)
            branches = median(owner, 7)
            branch_misses = median(owner, 8)
            printf "%s\tavailable\t%d\t%d\t%.3f\t%.3f\t%.6f\t%.3f\t%.3f\t%.6f\t%.3f\n",
                owner, count[owner], batch[owner], cycles / batch[owner],
                instructions / batch[owner], instructions / cycles,
                branches / batch[owner], branch_misses / batch[owner],
                branch_misses / branches, median(owner, 9) / batch[owner]
        }
        BEGIN {
            print "system\tstatus\tsamples\tbatch\tcycles_per_kernel\tinstructions_per_kernel\tipc_from_medians\tbranches_per_kernel\tbranch_misses_per_kernel\tbranch_miss_rate\tcache_misses_per_kernel"
        }
        END {
            emit("native")
            emit("wasmtime")
            emit("rv32-cached-dbt")
        }
    ' "$PERF_SAMPLES_REPORT" >"$PERF_REPORT"
}

PERF_REASON=""
if ! command -v "$RV32_C_PERF" >/dev/null 2>&1; then
    PERF_REASON="perf command is unavailable"
elif ! "$RV32_C_PERF" stat -x $'\t' -o "$BUILD_DIR/perf-probe.txt" \
    -e cycles,instructions,branches,branch-misses,cache-misses -- true \
    >"$BUILD_DIR/perf-probe.stdout" 2>"$BUILD_DIR/perf-probe.stderr"; then
    PERF_REASON="$(tr '\t\n' '  ' <"$BUILD_DIR/perf-probe.stderr")"
elif ! perf_measure native $'CK_RESULT\tee053d58' 4096 \
    "$BUILD_DIR/native-kernel" 1000 0x12345678 4096 || \
    ! perf_measure wasmtime "-301646504" 2048 "$RV32_C_WASMTIME" run --allow-precompiled \
        --invoke benchmark_batch "$BUILD_DIR/module.cwasm" 1000 305419896 2048 || \
    ! perf_measure rv32-cached-dbt $'CK_RESULT\tee053d58' "$AUDIT_BATCH" \
        "$ROOT/target/release/examples/rv32_c_codegen_audit" \
        execute "$BUILD_DIR" "$AUDIT_BATCH"; then
    PERF_REASON="perf measurement or checksum validation failed"
else
    perf_summarize
fi
if [[ -n "$PERF_REASON" ]]; then
    PERF_REASON="$(printf '%s' "$PERF_REASON" | tr '\t\n' '  ')"
    printf 'system\tstatus\tsamples\tbatch\tcycles_per_kernel\tinstructions_per_kernel\tipc_from_medians\tbranches_per_kernel\tbranch_misses_per_kernel\tbranch_miss_rate\tcache_misses_per_kernel\nstatus\tunavailable\t-\t-\t-\t-\t-\t-\t-\t-\t-\nreason\t%s\t-\t-\t-\t-\t-\t-\t-\t-\t-\n' \
        "$PERF_REASON" >"$PERF_REPORT"
fi

(
    cd "$BUILD_DIR"
    sha256sum \
        dbt-code-cache.bin \
        dbt-audit-config.tsv \
        dbt-execution-profile.tsv \
        dbt-hot-blocks.tsv \
        dbt-register-pressure.tsv \
        dbt-register-pressure-weighted.tsv \
        codegen-report.tsv \
        >deterministic-artifacts.sha256
)
echo "Focused RV32 C codegen audit passed; artifacts: $BUILD_DIR"
