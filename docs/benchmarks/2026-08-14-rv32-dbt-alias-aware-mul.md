# RV32 Cached DBT alias-aware MUL experiment

Issue: [#17](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/17)

## Decision

**REJECT.** Lowering low-word RV32 `MUL` directly into its register-cache
destination reduced generated code, but improved the current-host Cached DBT
control by only 0.721%, below the predeclared 1% keep threshold. Direct DBT
regressed by 2.133%. Commit `370d33b` implemented the candidate and commit
`6788a78` reverted it.

## Candidate

The retained lowerer computes every low-word `MUL` through `EAX`. The candidate
used x86 two-operand `imul r32, r32` directly:

- destination aliases the left source: `imul dst, rhs`;
- destination aliases the right source: `imul dst, lhs`;
- distinct destination: `mov dst, lhs; imul dst, rhs`.

Exact generated-byte tests covered all three shapes. The existing corner and
random differential corpus verified `MUL` together with every other RV32M
operation. `MULH*`, DIV/REM, precise exits, register allocation, and memory
lowering were unchanged.

## Why a contemporaneous control was required

Against the preceding accepted loop-carried reports, the two candidate medians
initially appeared 1.931% faster in absolute time. However, Native, QEMU, and
Wasmtime had also moved, and the candidate's mean DBT/QEMU ratio improved by
only about 0.55%. That was insufficient evidence for a small slice.

The source was therefore temporarily restored to the parent `EAX` lowering,
the exact release binary was rebuilt, and two new 21-sample controls were run
on the same host immediately after the candidate pair. The committed candidate
source was restored before applying the explicit git revert.

## Current-host A/B results

Each row is one 21-warm-sample process using the shared optimized C workload,
block size 16, and checksum `ee053d58`.

| Measurement | Control run 1 | Control run 2 | Candidate run 1 | Candidate run 2 |
|---|---:|---:|---:|---:|
| Cached DBT ns/kernel | 376,133.820 | 376,365.006 | 371,968.395 | 375,108.280 |
| vs native Clang | 6.157x | 6.157x | 6.088x | 6.119x |
| vs QEMU TCG | 0.739x | 0.738x | 0.730x | 0.736x |
| vs Wasmtime AOT | 2.726x | 2.728x | 2.696x | 2.726x |
| Direct DBT ns/kernel | 394,095,200 | 400,639,179 | 402,369,405 | 409,312,920 |

| Aggregate | Control mean | Candidate mean | Change |
|---|---:|---:|---:|
| Cached DBT | 376,249.413 ns/kernel | 373,538.338 ns/kernel | **-0.721%** |
| Direct DBT | 397,367,189.500 ns/kernel | 405,841,162.500 ns/kernel | **+2.133%** |

The Cached DBT candidate used 73.280% of QEMU time on average versus 73.838%
for the control. This confirms a small real improvement, but not the required
one, and it does not compensate for the Direct DBT regression.

## Static effect

| Statistic | Control | Candidate | Change |
|---|---:|---:|---:|
| Cached DBT emitted block bytes | 39,990 | 39,760 | -230 |
| Audit live resident bytes including support | 40,069 | 39,839 | -230 |
| Audit host instructions | 10,406 | 10,310 | -96 |
| Hot block `0x6a0` | 328 B / 84 instructions | 323 B / 82 instructions | -5 B / -2 |
| Hot block `0x678` | 383 B / 98 instructions | 381 B / 97 instructions | -2 B / -1 |
| Reserved DBT bytes | 278,528 | 278,528 | unchanged |
| Steady-state allocations/bytes | 0 / 0 | 0 / 0 | unchanged |

The result is useful for the planned micro-IR: fewer moves and smaller code do
not automatically improve end-to-end execution enough. Future coalescing must
be evaluated together with host register allocation, dependency chains, block
layout, and translation cost rather than as an isolated instruction-count goal.

## Verification

The candidate passed:

```bash
cargo fmt --all -- --check
cargo test --locked --offline
cargo test --all-features --locked --offline
bash scripts/tests/rv32-elf-boot-contract.sh
bash scripts/tests/rv32-elf-trap-contract.sh
bash scripts/tests/rv32-elf-atomic-contract.sh
bash scripts/tests/rv32-c-codegen-audit.sh
```

Default tests passed 156/156 and all-feature tests passed 167/167. All ELF/C,
fault-state, precise-budget, exact-profile, and zero-allocation contracts
passed. The final product source is the reverted control implementation.
