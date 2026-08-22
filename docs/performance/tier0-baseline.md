# Tier 0 interpreter baseline

Recorded on 2026-08-22 with the release profile (`codegen-units = 1`, thin
LTO), Rust 1.95.0, host `x86_64-unknown-linux-gnu`, and CPU label
`x86_64 local`.

The exact command was:

```sh
COMPUKTER_BENCH_CPU="$(uname -m) local" cargo test --release --locked --offline execution::tests::tier0_performance_baseline -- --ignored --nocapture --test-threads=1
```

Each workload receives 100 warmup slices followed by 1,000 measured slices,
each with a fixed budget of 4,096. Setup, admission, machine start, Rust
version discovery, and output formatting are outside the measured interval.
The reported counters are VM-entered blocks and attempted bytecode
instructions; rates are derived from the host monotonic elapsed time only.

| Artifact SHA-256 | Workload | Blocks | Instructions | Elapsed ns | Blocks/s | Instructions/s |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `665d086d971e9de77700158863ea05f79689d407a4fe8e386dbab727bd3e0c1b` | hot integer | 1,365,000 | 4,095,000 | 103,621,407 | 13,172,954 | 39,518,861 |
| `7420e1756d28d9241448840929831f4409861a2535dba847838ce05c7716243c` | mixed branch/switch | 2,047,500 | 3,412,500 | 144,671,598 | 14,152,743 | 23,587,906 |
| `e2fb54440921be5f4d30cdf608e3fd26d5ebce7f94ec82e73058e864776d79ae` | nested direct calls | 722,000 | 1,925,334 | 64,041,903 | 11,273,869 | 30,063,660 |
| `ff822d0d5ba3f217883cb5c3b4aec380904136a14b9c2322a8016340ec4350f1` | empty quota loop | 1,365,000 | 4,095,000 | 82,232,645 | 16,599,247 | 49,797,742 |

These values are a reproducible local reference, not an absolute
hardware-specific CI throughput threshold. Semantic, cost-accounting,
conformance-digest, and steady-state allocation regressions remain hard test
failures.
