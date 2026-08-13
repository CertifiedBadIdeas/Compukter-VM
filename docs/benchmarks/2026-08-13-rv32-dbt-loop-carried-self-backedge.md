# RV32 Cached DBT loop-carried self-backedge

Issue: [#17](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/17)

## Decision

**KEEP.** Extend the local self-backedge to blocks whose complete referenced
register set does not fit the seven-register chainable host pool, provided that
the loop-carried set and worst per-instruction temporary pressure do fit. The
hybrid implementation is 11.339% faster than the correctness-fixed baseline by
the mean of two independent 21-sample medians. It optimizes one additional hot
site, preserves `ee053d58`, and retains zero steady-state allocations.

## Correctness discovery

The original local-loop path exposed a state-materialization bug. A register
preloaded at the loop header was compile-time clean there even when a later
instruction wrote it. After a native self-backedge, an early fault in the next
iteration could therefore return to Rust without storing the runtime value
produced by the previous iteration.

The differential regression starts `x1` at `0x3ffc`. The first `lw` succeeds,
`addi` advances `x1` to `0x4000`, and the second iteration faults on `lw` from
`0x4000`. Before commit `6bad248`, the DBT exit reported address `0x4000` while
the materialized architectural `x1` remained `0x3ffc`. That state was invalid
for trap handling, diagnostics, or resumed execution.

Commit `6bad248` conservatively marks every preloaded local-loop register that
is written anywhere in the loop as dirty at the header. Early exits in later
iterations now materialize the actual runtime value. All performance comparisons
below use this corrected implementation as their baseline. The earlier
378--380 microsecond measurements are intentionally excluded because they came
from the incorrect fast path.

## Optimization

The original eligibility rule required every referenced guest register to fit
in seven host registers. That rejected hot block `0x608`: it references nine
guest registers, but only five values are read before their first definition and
must therefore survive the backedge.

The retained hybrid policy is:

1. If every referenced register fits, preserve the complete mapping exactly as
   the original optimization did.
2. Otherwise preserve only values read before their first write in the loop.
3. Reject the local path unless the carried set plus the maximum nonresident
   operands of any single instruction fits the host pool.
4. Protect carried mappings from eviction while lowering temporary operations.
5. Before the backedge, materialize dirty non-carried temporaries and discard
   their mappings; retained carried mappings jump directly to the loop body.

The first carried-only implementation applied step 2 even to small loops and
measured approximately 403,676 ns/kernel. It needlessly reconciled temporaries
that the old complete mapping retained. The hybrid policy restores the complete
mapping for small loops while admitting the larger hot block.

## Performance results

Each run uses the shared optimized C workload, block size 16, checksum
`ee053d58`, and 21 warm samples. Native Clang, QEMU system TCG, and Wasmtime AOT
are recalibrated inside every process.

| Measurement | Correct baseline run 1 | Correct baseline run 2 | Candidate run 1 | Candidate run 2 |
|---|---:|---:|---:|---:|
| Cached DBT ns/kernel | 425,294.992 | 433,914.644 | 380,829.382 | 380,956.492 |
| vs native Clang | 6.932x | 6.865x | 6.134x | 6.155x |
| vs QEMU TCG | 0.819x | 0.834x | 0.735x | 0.739x |
| vs Wasmtime AOT | 3.070x | 3.052x | 2.729x | 2.739x |

The corrected-baseline mean is 429,604.818 ns/kernel. The candidate mean is
380,892.937 ns/kernel: 11.339% less time, or 1.128x the baseline throughput.
In the two candidate processes the DBT used 73.5--73.9% of QEMU's execution
time, while remaining approximately 6.1x native and 2.73x Wasmtime AOT.

| Static/runtime statistic | Correct baseline | Candidate |
|---|---:|---:|
| translations | 89 | 89 |
| native dispatches | 2,135 | 2,135 |
| links established | 115 | 114 |
| local self-backedge sites | 4 | 5 |
| emitted/live block bytes | 39,936 | 39,990 |
| reserved DBT bytes | 278,528 | 278,528 |
| steady-state allocations/bytes | 0 / 0 | 0 / 0 |

The final focused code-generation audit reports 40,069 live resident bytes and
10,406 host instructions when its shared 79-byte support stub is included. It
finds 114 linked edges and the extended `0x608` block at 797 bytes / 201 host
instructions. Product benchmark byte counts exclude the shared support region.

## Verification

```bash
cargo fmt --all -- --check
cargo test --locked --offline
cargo test --all-features --locked --offline
bash scripts/tests/rv32-elf-boot-contract.sh
bash scripts/tests/rv32-elf-trap-contract.sh
bash scripts/tests/rv32-elf-atomic-contract.sh
bash scripts/tests/rv32-c-codegen-audit.sh
```

The default suite passed 155 tests; the all-features suite passed 166 tests.
The boot, trap, atomic, C oracle/codegen, precise-budget, fault-state, exact
profile, and zero-allocation contracts all passed.
