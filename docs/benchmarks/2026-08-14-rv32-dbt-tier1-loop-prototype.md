# RV32 Tier 1 loop-region prototype baseline

Date: 2026-08-14

Issue: #17

Decision: keep the prototype behind `dbt-tier1-prototype`; do not enable it by default.

## Scope

The prototype recognizes bounded single-block self-loops, builds and optimizes a fixed-capacity
value region, assigns values to eight x86-64 host registers plus bounded spills, and lowers RV32I/M
arithmetic and direct RAM loads/stores. It preserves precise memory exits, LR/SC reservation
invalidation, soft tick budgets, the existing per-VM code cache, and cached fallthrough chaining.
Ineligible regions fall back to Tier 0.

The product candidate is `rv32-cached-dbt-tier1`, exposed only with the
`dbt-tier1-prototype` Cargo feature.

## Correctness and allocation findings

The first product run found two bugs before a valid timing result was accepted:

- dead loop-entry values were reconciled even though the allocator intentionally gave them no
  location;
- lazily-created entry parameters could alias an earlier temporary because their value ID did not
  represent their true entry-time lifetime.

The region builder now creates every referenced guest parameter before expression values, and
reconciliation skips dead entries. The shared C checksum and the zero-allocation steady-state
contract both pass.

The first valid timing run also exposed missing region fallthrough links: native dispatches grew
from 2,135 to 2,049,618. Adding a normal patchable fallthrough plus cold-exit relocation reduced
the candidate to 1,111 dispatches at half the calibrated batch size.

## Final 21-sample self-A/B

Command:

```text
cargo run --release \
  --features dbt-tier1-prototype,dbt-translation-timing \
  --example rv32_c_comparison -- \
  self-ab target/rv32-c-comparison \
  rv32-cached-dbt-block-16 rv32-cached-dbt-tier1 21
```

| Metric | Tier 0 cached DBT | Tier 1 prototype | Tier 1 delta |
|---|---:|---:|---:|
| Calibrated batch | 1,024 | 512 | — |
| Median ns/kernel | 363,721.226 | 650,254.684 | +78.778% |
| p95 total ns | 379,679,989 | 337,015,259 | batches differ |
| Retired instructions/kernel | 3,918,182 | 3,918,182 | equal |
| Fixed retired overhead | 20 | 20 | equal |
| Native dispatches | 2,135 | 1,111 | equivalent after batch normalization |
| Links established | 114 | 114 | equal |
| Typed slow exits | 2 | 2 | equal |
| Tier 1 regions | 0 | 5 | — |
| Tier 1 fallbacks | 0 | 84 | — |
| Emitted bytes | 39,999 | 41,907 | +4.77% |
| Steady allocations / bytes | 0 / 0 | 0 / 0 | equal |

Translation phases for one cold workload run:

| Phase | Tier 0 median ns | Tier 1 median ns | Delta |
|---|---:|---:|---:|
| Lift | 21,470 | 21,540 | +0.326% |
| Lower | 107,809 | 130,239 | +20.805% |
| Publish | 68,478 | 70,000 | +2.223% |

## Interpretation

Dispatch, cache publication, correctness, and allocation behavior are no longer the limiting
factors. The remaining regression is in Tier 1 generated code. The current lowerer computes most
values through fixed scratch registers and performs every loop-carried reconciliation through
stack staging. The next optimization should measure and reduce reconciliation moves, beginning
with cycle-aware register-to-register parallel copies and coalescing entry/output locations where
safe. The result above is the baseline for that work.
