# Cache-local fused micro-IR for RV32 DBT

Date: 2026-08-14  
Umbrella issue: #17  
Baseline: `fdb6664`  
Selected implementation: `5c87458`, `6903cd6`, `e200be6`, `4a7e26a`, `f22a2ce`

## Decision

**KEEP.** Product DBT translation now lifts guest words directly into a compact
semantic micro-IR instead of materializing `DecodedInstruction` and then
converting it again. The existing x86-64 lowerer remains the single code
generator. The legacy decoded input remains available only to unit tests as a
byte-for-byte lowering reference; product VMs no longer reserve its buffer.

The selected IR is a 20-byte array-of-structures record containing the raw word,
compact semantic fields, and precomputed register effects. The array begins on a
64-byte cache-line boundary. Future-use analysis stores only actual two-byte
register events, fixed register ranges, and exits. Sequential lowering advances
monotonic per-register cursors, while the random-access reference query remains
available for validation and non-sequential consumers.

## Why the layout changed

The first implementation stored a dense `[FutureValue; 32]` row for every guest
instruction. At the maximum 64-instruction capacity that table alone occupied
4 KiB and made fused lift about four times slower than the old decoder. Sparse
events reduced the analysis workspace to at most 608 bytes and cut measured lift
from 51,380 ns to 24,000 ns over the reached 879 guest instructions.

SSA-style value IDs were also removed from the product record because no current
lowering optimization consumed them. Register versions can be added later as an
optional analysis when a concrete optimization needs them; paying their cost on
every translation now had no benefit.

Two additional candidates were rejected:

- a dense future-value table, because its cache footprint dominated lift;
- a 16-byte packed record, because pack/unpack moved time from lift to lower and
  left `lift + lower` unchanged (107,729 ns versus 107,670 ns).

## Shared optimized C / QEMU control

The table uses 21 warm samples, checksum `ee053d58`, block size 16, 512 cache
sets, 128 KiB code cache, and the same optimized RV32 C ELF. Native, QEMU TCG,
Wasmtime AOT, and Cached DBT were measured in the same final process run.

| Metric | Old decoded path | Cache-local micro-IR | Delta |
|---|---:|---:|---:|
| Lift/decode, 879 instructions | 13,011 ns | 19,870 ns | +52.72% |
| Lower, 879 instructions | 93,181 ns | 85,979 ns | -7.73% |
| Lift + lower | 106,192 ns | 105,849 ns | -0.32% |
| Publish | 55,940 ns | 58,340 ns | +4.29% |
| First completion | 589,163 ns | 597,779 ns | +1.46% |
| Warm Cached DBT | 376,526 ns/kernel | 378,697 ns/kernel | +0.58% |
| Cached DBT / QEMU | 0.739122x | 0.740008x | effectively unchanged |
| Emitted host code | 39,990 bytes | 39,990 bytes | unchanged |
| Steady-state allocations | 0 | 0 | unchanged |

The selected same-run absolute comparison was:

| Runtime | ns/kernel | Relative to native |
|---|---:|---:|
| Native Clang `-O3 -march=native -flto` | 61,594 | 1.000x |
| Wasmtime AOT | 139,092 | 2.258x |
| Cache-local Cached DBT | 378,697 | 6.148x |
| QEMU 11.1.0 system TCG | 511,746 | 8.308x |

The isolated `lift + lower` phase sum improves by 0.32%, and the warm execution
gate remains inside the allowed 1%. First-completion medians were visibly noisier
across the control runs (589,290 ns in the preceding identical-code run and
597,779 ns in the final run), so the decision uses the separately timed phase
sum plus the stable warm gate. This slice removes the high-cost enum pipeline,
makes future analyses linear and cache-local, and preserves one lowerer.

## Memory result

At the default 16-instruction capacity the aligned IR payload is 320 bytes and
its sparse analysis workspace is 272 bytes. Removing the old 16-slot
`Rv32ResolvedInstruction` product buffer saves 640 bytes, so the new product
translation workspace is 48 bytes smaller overall rather than retaining both
representations. Smaller non-multiple-of-16 capacities round the aligned payload
up to one 320-byte chunk; this trade-off should be revisited if an 8-instruction
DBT profile becomes the dominant microcontroller configuration.

## Reproduction

```sh
cargo build --release --features wasmtime-comparison \
  --example rv32_c_comparison --locked --offline
target/release/examples/rv32_c_comparison target/rv32-c-comparison 21
```
