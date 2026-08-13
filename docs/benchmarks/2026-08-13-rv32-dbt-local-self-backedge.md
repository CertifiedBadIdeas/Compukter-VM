# RV32 Cached DBT local self-backedge

Issue: [#17](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/17)

> **Correctness erratum:** the measurements in this document predate commit
> `6bad248`, which fixed architectural register materialization when a later
> local-loop iteration faults before rewriting a loop-carried value. Keep these
> numbers as the historical result of the original slice, but do not use them as
> a correctness-preserving performance baseline. The corrected baseline and the
> retained loop-carried extension are recorded in
> [the follow-up result](2026-08-13-rv32-dbt-loop-carried-self-backedge.md).

## Decision

**KEEP.** Preserving the register cache across eligible same-block backedges improved the selected 16-instruction Cached DBT by 14.7% and 14.5% in two independent 21-sample runs. The shared C checksum remained `ee053d58`, translations and host dispatches were unchanged, and the generated-code cost was 66 bytes across the reached program.

## What changed

The previous linked self-edge was already native-to-native, so it avoided returning to the Rust dispatcher. It nevertheless used the normal inter-block contract on every loop iteration:

1. store all dirty guest registers into `Rv32ArchitecturalState`;
2. add the block's attempted instruction count to the signed budget counter;
3. jump through the block chain entry and test the budget;
4. reload loop-carried guest values into host registers.

The local path proves that the block's final conditional branch targets the start of that same block and that every nonzero guest register referenced by the block fits in the seven-register chainable host pool. Those guest registers are loaded once in the preheader. A successful backedge then updates and checks the budget and jumps directly to the local body entry, retaining the same guest-to-host mapping and dirty values.

The normal materialized path remains in use for fallthrough, external entry, blocks referencing more than seven guest registers, slow memory exits, traps, invalidation boundaries, and budget exhaustion. The cold budget exit flushes the retained mapping before returning to Rust. This preserves precise architectural state at every observable exit while removing it from the repeated loop path.

`Rv32DbtStats::local_self_backedge_sites` counts optimized lowering sites without adding a counter increment to the runtime hot path. Exact execution profiling remains the source for dynamic edge frequency.

## Results

Environment and artifacts match the exact-profile baseline at commit `c81be99`: Linux x86-64, Rust 1.95.0, Clang/LLD 22.1.8, QEMU 11.1.0 TCG, and Wasmtime 47.0.3. The candidate is commit `35b03e4`; each row is the median of 21 warm samples.

| Measurement | Baseline | Candidate run 1 | Candidate run 2 |
|---|---:|---:|---:|
| Cached DBT ns/kernel | 444,193.362 | 378,773.393 | 379,754.469 |
| Change from baseline | — | -14.728% | -14.507% |
| vs native Clang | 7.257x | 6.179x | 6.181x |
| vs QEMU TCG | 0.870x | 0.742x | 0.741x |
| vs Wasmtime AOT | 3.208x | 2.753x | 2.742x |
| Cached DBT advantage over QEMU | 15.0% | 34.7% | 35.0% |

The mean of the two candidate medians is 379,263.931 ns/kernel, 14.617% below the preserved baseline.

| Static/runtime statistic | Baseline | Candidate |
|---|---:|---:|
| checksum | `ee053d58` | `ee053d58` |
| translations/publications | 89 / 89 | 89 / 89 |
| native dispatches | 2,135 | 2,135 |
| links established | 119 | 115 |
| local self-backedge sites | not reported | 4 |
| emitted code bytes | 39,830 | 39,896 |
| live code bytes | 39,830 | 39,896 |
| reserved DBT bytes | 278,528 | 278,528 |
| steady-state allocations/bytes | 0 / 0 | 0 / 0 |

The four removed links exactly match the four reported local sites. Unchanged dispatch count confirms that the improvement is not dispatcher elimination: it comes from removing repeated register materialization and chain-entry work inside already chained loops.

## Commands

```bash
bash scripts/compile-rv32-c-comparison.sh target/rv32-c-comparison
wasmtime compile -O opt-level=2 \
  -o target/rv32-c-comparison/module.cwasm \
  target/rv32-c-comparison/module.wasm
cargo build --release --features wasmtime-comparison \
  --example rv32_c_comparison --locked --offline

target/release/examples/rv32_c_comparison \
  target/rv32-c-comparison 21
```

The final command was run twice. Native, QEMU, and Wasmtime were recalibrated in each process and therefore provide contemporaneous ratios rather than ratios against an older host measurement.
