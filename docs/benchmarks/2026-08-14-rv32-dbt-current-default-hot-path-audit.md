# RV32 Cached DBT current-default hot-path audit — 2026-08-14

Issue: [#17](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/17)

## Decision

The next bounded optimization slice is **fused local-self-backedge budget
branching**. The existing local-loop path performs `add`, `test`, a conditional
branch, and an unconditional jump on every successful backedge. The `add`
already produces the sign flag required by the current signed-budget contract,
so the candidate will reuse that flag and make the hot loop body the conditional
target. This removes two host instructions and one branch per successful local
self-backedge without changing the guest-visible budget model.

The first follow-up candidate is outlining native-RAM slow exits so that a
successful RAM access falls through instead of jumping over its cold fault body.
It remains separate because its precise-state and invalidation surface is wider.

## Exact audited configuration

The audit uses exported product defaults rather than parallel benchmark
constants:

| Setting | Value |
|---|---:|
| Cache sets | 256 |
| Maximum guest instructions per block | 16 |
| Translation scratch | 8 KiB |
| Per-VM code cache | 128 KiB |
| Code alignment | 64-byte block base |
| Register profile | `RcxOverflow8` |

The shared optimized C workload completed with checksum `ee053d58`. The audit
revision is `f34a7348ef9eda965eead45669c85ab1e727127a`.

## Environment and reproduction

```text
Linux 7.1.8-zen1-3-zen x86-64
AMD Ryzen 9 9950X3D 16-Core Processor
rustc 1.95.0 (59807616e 2026-04-14)
clang 22.1.8
LLD 22.1.8
QEMU 11.1.0
Wasmtime 47.0.3 (5554cc1a6 2026-07-31)
```

```bash
RV32_C_CODEGEN_BUILD_DIR=/tmp/compukter-current-audit-1 \
  scripts/tests/rv32-c-codegen-audit.sh
RV32_C_CODEGEN_BUILD_DIR=/tmp/compukter-current-audit-2 \
  scripts/tests/rv32-c-codegen-audit.sh
scripts/tests/rv32-c-qemu-comparison.sh
```

The focused audit takes seven `perf stat` samples for native Clang, embedded
Wasmtime, and Cached DBT. Every sample validates the checksum. Timings are not
hashed; all code, configuration, profile, and static-analysis artifacts are.

## Deterministic identity

Both clean audit directories produced the same hashes:

| Artifact | SHA-256 |
|---|---|
| DBT code cache | `669442fed8008acde4c84b6d662028757d4a55f2a76381e9f10a347886563b8f` |
| Audit configuration | `86db145140076b1cfa5911d5064da702fedb2bf9fd6e64d0ff03196ad3ab2601` |
| Exact execution profile | `50f43240db5b1495e31563b37fbd8f3afcc9eca8773e87b4d6fba0048fa72a2e` |
| Execution-ranked hot blocks | `1e3ac3697e5ad2f63ca885c3bdd0fd864a56e1561e90dc2bc6baa7e53511ddac` |
| Register pressure | `d593ce1b96761a9b5ca6cba3e667f6b31769ec5970e019dfd7ee34aab8c1c478` |
| Weighted register pressure | `0dccb38872b93090870892e1c122fdd770b0ced9b5a37cf0246c943ec7929d98` |
| Static codegen report | `5ff98aa520914efb28938bebc2ad95e33cfef99422f27ddaf9cfa24416ab6894` |

## Exact hot blocks

The four hottest blocks account for 93.2955% of all block entries:

| PC | Entries | Share | Guest insns | Bytes | Host insns | Memory operands | Moves |
|---:|---:|---:|---:|---:|---:|---:|---:|
| `0x6a0` | 256,000 | 53.6711% | 6 | 328 | 84 | 29 | 56 |
| `0x608` | 63,000 | 13.2081% | 13 | 771 | 195 | 70 | 130 |
| `0x648` | 63,000 | 13.2081% | 9 | 385 | 100 | 36 | 66 |
| `0x678` | 63,000 | 13.2081% | 10 | 383 | 98 | 34 | 63 |

The resident cache contains 88 live blocks, 39,644 code bytes, 10,317 static
host instructions for 863 guest slots, 3,361 memory operands, and 6,779 moves.
Native Clang's bounded analysis region has 806 instructions; the Wasmtime AOT
region has 1,029. These static boundaries are useful for code shape, not as
runtime ratios.

## Repeated hardware-counter result

Values are medians normalized per kernel. The two columns are independent
seven-sample processes.

| System | Cycles run 1 | Cycles run 2 | Instructions run 1 | Instructions run 2 | IPC run 1 | IPC run 2 | Branches run 1 | Branches run 2 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Native | 346,016 | 346,316 | 652,625 | 652,625 | 1.886 | 1.884 | 9,760 | 9,760 |
| Wasmtime | 773,134 | 774,113 | 2,889,210 | 2,889,209 | 3.737 | 3.732 | 166,563 | 166,562 |
| Cached DBT | 2,061,989 | 2,067,814 | 15,114,186 | 15,114,186 | 7.330 | 7.309 | 3,689,424 | 3,689,424 |

Cached DBT consumes approximately 2.67x Wasmtime's cycles and 5.23x its host
instructions. Against native it consumes approximately 5.96x cycles and 23.16x
instructions. Its IPC is almost twice Wasmtime's, while cache misses remain
small. The primary gap is therefore dynamic instruction and branch volume, not
front-end starvation.

## Contemporaneous absolute ceiling

The full 21-warm-sample comparison at the same revision produced:

| System | ns/kernel | vs native time | vs QEMU time |
|---|---:|---:|---:|
| Native Clang `-O3 -march=native -flto` | 61,802 | 1.000x | 0.121x |
| QEMU 11.1.0 system TCG | 510,385 | 8.259x | 1.000x |
| Wasmtime embedded AOT | 138,533 | 2.241x | 0.271x |
| Cached DBT current default | 362,566 | 5.867x | 0.710x |

The current DBT is approximately 1.408x QEMU's throughput, but Wasmtime remains
2.617x faster and native remains 5.867x faster. The full comparison's lazy
resident state has 89 blocks and 40,039 emitted bytes; the deterministic audit
captures its explicitly bounded 88-block final snapshot. Do not mix those two
snapshot boundaries when comparing byte totals.

## Selected slice and gate

The four local self-backedges execute 441,000 times per kernel. Replacing the
current successful sequence

```text
add budget, attempted
test attempted, attempted
jge cold_budget_exit
jmp hot_body
```

with an equivalent flag-reusing layout can remove an estimated 882,000 host
instructions (5.84% of the measured DBT total) and 441,000 branches (11.95%).
The implementation must use the sign flag (`js` semantics), not a signed
less-than comparison whose overflow handling would differ from the existing
`test` contract.

Keep the slice only if two independent 21-warm-sample self-A/B runs show at
least a 1% median improvement without mixed direction, while preserving:

- checksum `ee053d58`;
- exact retired and attempted-instruction budget behavior, including the
  `5 - 12` overshoot case;
- architectural materialization on budget, fault, and typed exits;
- zero steady-state allocations;
- unchanged Direct DBT behavior.

If it fails that gate, retain the audit and move to hot-fallthrough native-RAM
guards. The four hottest blocks perform about 571,000 successful dynamic RAM
accesses per kernel; outlining their cold exits could remove one unconditional
hot-path jump per access, but requires a broader correctness audit.
