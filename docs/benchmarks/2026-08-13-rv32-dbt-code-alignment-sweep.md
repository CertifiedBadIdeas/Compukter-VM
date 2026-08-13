# RV32 Cached DBT Code-Alignment Sweep

Issue: [#17](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/17)

## Environment

- Linux 7.1.6-zen1-1-zen x86-64
- AMD Ryzen 9 9950X3D, 16 cores / 32 threads
- Rust 1.95.0, Cargo 1.95.0
- Clang and LLD 22.1.8
- QEMU system RISC-V 11.1.0, TCG
- Wasmtime 47.0.3

The sweep changes only Cached DBT code placement. Every candidate uses the
same RV32 ELF, lowering, block formation, per-VM cache, and W/RX publication
path. The tested geometry is 512 metadata sets, 128 KiB of executable cache,
and at most 16 guest instructions per block.

## Commands

Two independent product processes:

```text
cargo run --release --locked --offline --example rv32_machine_benchmarks -- alignment-sweep 4096 21 > /tmp/rv32-dbt-alignment-product-run-1.tsv
cargo run --release --locked --offline --example rv32_machine_benchmarks -- alignment-sweep 4096 21 > /tmp/rv32-dbt-alignment-product-run-2.tsv
```

Two independent C/QEMU processes:

```text
RV32_C_COMPARISON_BUILD_DIR=/tmp/rv32-c-alignment-run-1 scripts/tests/rv32-c-qemu-comparison.sh > /tmp/rv32-c-alignment-run-1.log
RV32_C_COMPARISON_BUILD_DIR=/tmp/rv32-c-alignment-run-2 scripts/tests/rv32-c-qemu-comparison.sh > /tmp/rv32-c-alignment-run-2.log
```

Each candidate receives 21 warm samples in rotating order. Construction is
measured outside the steady-execution allocation window.

## Product Result

The table is the geometric mean of the nine per-workload median ratios to the
64-byte base candidate. Lower is faster.

| Block-base alignment | Run 1 vs 64 | Run 2 vs 64 | Decision |
|---:|---:|---:|---|
| 16 B | 1.006580x | 1.007414x | Reject |
| 32 B | **0.990588x** | **0.975711x** | Keep |
| 64 B | 1.000000x | 1.000000x | Previous default |
| 128 B | 0.999036x | 0.998634x | No material gain; denser candidate wins |

For 32 bytes, the largest slowdown against 64 was 1.03% on
`memory-sequential` in run 1. It did not repeat in run 2, where that workload
was 6.71% faster. No workload had a repeated regression, and none exceeded the
3% rejection threshold in either run. All 72 timed candidate/workload rows
reported zero steady allocations and zero steady allocated bytes.

Across these compact product images, 32-byte placement used 91--176 padding
bytes after translation. The same images at 64 bytes used 251--391 padding
bytes. Live block bytes and guest checksums were identical.

## Optimized C / QEMU Result

Times are normalized nanoseconds per kernel. Lower is faster.

| Candidate | Run 1 ns/kernel | Run 1 vs QEMU | Run 2 ns/kernel | Run 2 vs QEMU |
|---|---:|---:|---:|---:|
| Native Clang | 61,584.826 | 0.112745x | 61,300.538 | 0.120442x |
| QEMU TCG | 546,231.946 | 1.000000x | 508,963.394 | 1.000000x |
| Block base 16 B | 506,796.096 | 0.927804x | 500,584.486 | 0.983537x |
| Block base 32 B | **448,383.270** | **0.820866x** | **444,191.818** | **0.872738x** |
| Block base 64 B | 456,198.854 | 0.835174x | 449,758.675 | 0.883676x |
| Block base 128 B | 455,061.814 | 0.833093x | 450,874.688 | 0.885869x |

The 32-byte candidate beat 64 bytes by 1.71% and 1.24% in the two independent
runs. It also remained faster than QEMU by 17.91% and 12.73% respectively.
Compared with the earlier `61ce89f` result of 475,960 ns/kernel, the two
32-byte runs are 5.80% and 6.68% faster.

All four candidates contain the same 39,830 live block bytes and perform zero
steady allocations. Placement density differs:

| Alignment | Padding | Occupied code prefix |
|---:|---:|---:|
| 16 B | 743 B | 40,652 B |
| 32 B | **1,447 B** | **41,356 B** |
| 64 B | 2,695 B | 42,604 B |
| 128 B | 4,551 B | 44,460 B |

There were no metadata or overlap evictions in the steady optimized-C working
set.

## Base Decision

`Rv32DbtCodeAlignment::BlockBase(32)` replaces 64 bytes as the product default.
It wins both product geomeans and both C runs, stays below every rejection
threshold, retains the DBT advantage over QEMU, and consumes 1,248 fewer bytes
of code-prefix capacity than 64-byte placement.

## Chain-Entry Follow-Up

The focused follow-up added only `ChainEntry(32)` and compared it with the
winning `BlockBase(32)` layout. It intentionally did not open a full
base/entry cross-product. The product runs used the same command and sampling
policy as above; the C gate contained 26 checksum-validated rows and used QEMU
11.1.0 and Wasmtime 47.0.3.

| Candidate | Product run 1 vs base 32 | Product run 2 vs base 32 | C run 1 ns/kernel | C run 1 vs QEMU | C run 2 ns/kernel | C run 2 vs QEMU |
|---|---:|---:|---:|---:|---:|---:|
| Block base 32 B | 1.000000x | 1.000000x | **447,849.411** | **0.878205x** | **448,902.634** | **0.875898x** |
| Chain entry 32 B | 1.004884x | 1.007460x | 449,224.093 | 0.880900x | 449,885.941 | 0.877817x |

`ChainEntry(32)` was slower in both product geomeans by 0.49% and 0.75%, and
slower in both optimized-C runs by 0.31% and 0.22%. Both candidates retained
zero steady allocations, the same 39,830 live code bytes, identical checksums,
and no cache evictions. Aligning the chain entry saved only four bytes of
padding and occupied prefix (1,443 / 41,352 bytes instead of 1,447 / 41,356).

## Final Decision

Keep `Rv32DbtCodeAlignment::BlockBase(32)` as the product default. The focused
chain-entry alternative does not buy meaningful density and loses consistently
in execution time. Both alignment anchors remain available as explicit policy
options for future profiling and architecture experiments.
