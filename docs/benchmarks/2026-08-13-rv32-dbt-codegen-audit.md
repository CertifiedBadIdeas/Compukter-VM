# RV32 Cached DBT Generated-Code Audit — 2026-08-13

## Scope

This audit captures the exact final resident x86-64 cache produced by the
Cached DBT for the shared optimized C workload. It compares bounded static
regions from the DBT, an analysis-only native Clang object, and Wasmtime 47.0.3
AOT output. Static counts describe code-generation shape; they are not runtime
performance ratios.

The product geometry is 512 sets, 16 guest instructions per block, 8 KiB
scratch space, and a 128 KiB per-VM code cache. The workload completed with
checksum `ee053d58`.

## Environment and command

```text
source revision: 44408bbd6f83401a2f737daeb2b6d8f4d1bbf82c
Linux lazyhat-station 7.1.6-zen1-1-zen x86_64
CPU: AMD Ryzen 9 9950X3D, 16 cores / 32 threads
rustc 1.95.0, LLVM 21.1.8
clang 22.1.8
Wasmtime 47.0.3 (5554cc1a6 2026-07-31)
```

```bash
RV32_C_WASMTIME=/tmp/wasmtime-v47.0.3-x86_64-linux/wasmtime \
RV32_C_CODEGEN_BUILD_DIR=/tmp/compukter-codegen-audit-1 \
./scripts/tests/rv32-c-codegen-audit.sh
```

Two clean output directories produced byte-identical `dbt-code-cache.bin`,
`dbt-blocks.tsv`, and `codegen-report.tsv`. The snapshot identity is:

```text
e8bc3e8d8008801aa35904b6d1d152f3c1dd30d8f44352c98c8f48f12cf753fc
```

Hardware performance counters were unavailable because `perf` was absent.
This does not affect the deterministic static audit.

## Comparative static report

| Region | Code bytes | Host instructions | Guest instructions | Host/guest | Memory operands | Moves | Vector |
|---|---:|---:|---:|---:|---:|---:|---:|
| Cached DBT live blocks | 56,499 | 15,318 | 879 | 17.427 | 5,354 | 11,423 | 0 |
| Native `benchmark_kernel`, O3 no-LTO | 4,140 | 806 | — | — | 212 | 193 | 223 |
| Wasmtime AOT `benchmark_batch` | — | 1,029 | — | — | 440 | 271 | 254 |

The DBT snapshot contains 89 live blocks, 119 linked edges, and three unlinked
edges. Block sizes are 635 bytes mean, 432 bytes p50, 1,653 bytes p95, and
3,273 bytes maximum. Moves account for 74.6% of all statically emitted DBT
instructions.

The comparison boundaries differ deliberately. DBT includes guards, exits,
and transitions. The native object excludes the executable's LTO boundary,
while the Wasmtime region is its exported batch wrapper. The native and
Wasmtime rows therefore provide code-shape references, not normalized ratios.

## Highest expansion and root cause

Four one-instruction conditional-branch blocks at guest PCs `0x4fc`, `0x55c`,
`0x5bc`, and `0x4a0` each contain 99 host instructions in 350 bytes. Five more
one-instruction branches contain 98 host instructions in 352 bytes.

At `0x4fc`, the guest instruction is `bnez t3, 0x524`. Its 350-byte host block
contains:

- a 36-byte callable entry prefix;
- the chain-entry budget guard and an 89-byte cold budget-exit body;
- the conditional test and two linked jump paths;
- two further 89-byte completed-edge fallback bodies.

The three cold exit bodies occupy 267 bytes, approximately 76% of this block.
Across all resident blocks, one budget fallback per block and one fallback per
122 static edges account for about 18.8 KiB before counting additional register
flushes. Successful linked transitions jump over the edge fallback bodies, so
the static move count must not be mistaken for a dynamic executed-instruction
count. The duplication still consumes the per-VM code cache and instruction
cache capacity, and unlinked or budget exits execute it.

## Decision

The next bounded optimization is to compact the cold DBT exit paths: keep the
successful chained path direct, but share or outline repeated budget and
completed-edge materialization/epilogue sequences instead of embedding a full
copy behind every guard and patchable jump. This work is tracked by
[issue #20](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/20).

Keep the change only if the same audit reduces live resident code from 56,499
bytes by at least 20% (to at most 45,199 bytes), preserves the checksum and all
DBT correctness tests, and does not regress the 21-sample optimized C/QEMU DBT
median by more than 1%. A speed improvement is desirable but not assumed from
static size alone. Dynamic counters remain optional evidence.
