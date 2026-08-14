# RV32 Cached DBT fused local budget branch — 2026-08-14

Issue: [#17](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/17)

## Decision

**KEEP.** Reusing the sign flag from the local-loop attempted-count add reduced
the mean of two independent 21-sample medians by 1.467%. Both processes moved
in the same direction: 0.995% and 1.940%. The generated resident code is 40
bytes smaller, the exact execution profile is unchanged, all candidates return
checksum `ee053d58`, and Cached DBT retains zero steady-state allocations.

The first process is deliberately reported at full precision: it is 0.005
percentage points below 1% by itself. The retained decision follows the existing
project convention of applying the 1% gate to the mean of independent medians
while requiring every process to agree in direction.

## Change

Baseline commit `6e11448` emitted this local self-backedge:

```text
add attempted, execution_counter
test execution_counter, execution_counter
jge local_budget_exit
jmp local_loop_body
```

Candidate commit `f23ce4e` emits:

```text
add attempted, execution_counter
js local_loop_body
local_budget_exit:
```

The execution counter starts at the negated remaining budget. The add result is
negative while another complete iteration is allowed, so the x86 sign flag is
the same predicate previously recomputed by `test`. The local cold exit is now
placed immediately after the conditional branch; the unrelated chain-entry
cold exit follows it. This preserves register materialization, cumulative
attempted count, profiling, and typed-exit behavior without a new public option.

Five resident local-loop sites lose two static host instructions and eight code
bytes each. Four of those sites dominate the exact profile.

## Environment and method

```text
Linux 7.1.8-zen1-3-zen x86-64
AMD Ryzen 9 9950X3D 16-Core Processor
rustc 1.95.0
Clang/LLD 22.1.8
QEMU 11.1.0
Wasmtime 47.0.3
```

The baseline was built from a plain `git archive` snapshot of `6e11448`, not a
worktree. Baseline and candidate binaries used the same compiled C, RV32, QEMU,
and Wasm artifacts. Four full comparison processes ran in alternating order:
baseline, candidate, baseline, candidate. Each candidate received 21 warm
samples.

## Wall-time result

| Process | Baseline ns/kernel | Candidate ns/kernel | Delta |
|---|---:|---:|---:|
| 1 | 364,169.070 | 360,547.095 | -0.994586% |
| 2 | 363,717.048 | 356,660.737 | -1.940055% |
| Mean of medians | 363,943.059 | 358,603.916 | **-1.467027%** |

Candidate absolute ceilings, averaged across its two processes:

| Comparison | Candidate time ratio |
|---|---:|
| vs native Clang | 5.834x |
| vs QEMU system TCG | 0.697x |
| vs Wasmtime AOT | 2.598x |

The candidate therefore retains approximately 1.435x QEMU throughput. Native,
QEMU, and Wasmtime are recalibrated in each process; they are ceiling context,
not the keep criterion.

The two Cached DBT rows have identical checksum, 4,012,218,388 retired guest
instructions, 879 translated guest slots, and zero steady allocations. Emitted
code falls from 40,039 to 39,999 bytes. Direct DBT statistics and behavior are
unchanged; its wall time is intentionally excluded because this path cannot
form a chainable local self-backedge.

## Hardware-counter confirmation

Two clean seven-sample candidate audits were compared with the two clean
seven-sample baseline audits from the immediately preceding current-default
audit. Values below are the means of each pair of medians.

| Cached DBT metric/kernel | Baseline | Candidate | Delta |
|---|---:|---:|---:|
| Cycles | 2,064,902 | 2,027,518 | -1.810% |
| Host instructions | 15,114,186 | 14,231,641 | -5.839% |
| Host branches | 3,689,424 | 3,248,163 | -11.960% |

The measured removal is 882,545 host instructions and 441,261 branches per
kernel, matching the static prediction of two instructions and one branch per
dynamic successful local backedge. The smaller wall-time gain is consistent
with x86 macro-fusing the old `test+jge` pair and predicting the loop branches
well: removed instructions are not equivalent to removed cycles.

Both candidate audits produced byte-identical deterministic artifacts. The
exact execution-profile hash remains
`50f43240db5b1495e31563b37fbd8f3afcc9eca8773e87b4d6fba0048fa72a2e`,
while the new code-cache hash is
`00e459d1cbdebc019da62ba05728c97eab6f640ce98bcd1f5709434f55113bfb`.

## Correctness verification

The implementation added a behavioral generated-code test which locates the
real local budget add, verifies that it is followed immediately by backward
`js`, and resolves the branch target into the local body. The full suites also
exercise:

- the `5 -> 12` allowed block overshoot and cumulative attempted count;
- every partial-budget prefix against precise backends;
- architectural state materialization on a later local-loop memory fault;
- the profiled local-loop budget boundary;
- zero steady-state allocations.

Commands:

```bash
cargo fmt --all -- --check
cargo test --locked --offline
cargo test --all-features --locked --offline
RV32_C_CODEGEN_BUILD_DIR=/tmp/compukter-fused-audit-1 \
  RV32_C_PERF_SAMPLES=7 scripts/tests/rv32-c-codegen-audit.sh
RV32_C_CODEGEN_BUILD_DIR=/tmp/compukter-fused-audit-2 \
  RV32_C_PERF_SAMPLES=7 scripts/tests/rv32-c-codegen-audit.sh
```

The next candidate from the current-default audit remains hot-fallthrough
native-RAM guards, but its wider precise-fault and invalidation surface should
be designed as a separate slice.
