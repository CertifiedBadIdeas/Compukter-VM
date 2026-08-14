# RV32 DBT Register-Pressure Audit — 2026-08-14

## Scope

This audit measures the current seven-register chainable cache on the shared
optimized C workload. It records actual translation-time cache events per live
guest block and joins them by guest PC with a separate exact execution-profile
run. The counters diagnose a bounded allocator experiment; they do not claim a
runtime speedup.

The product geometry is 512 sets, 16 guest instructions per block, 8 KiB
scratch space, and a 128 KiB per-VM code cache. Both the unprofiled snapshot run
and the separately instrumented profile run completed with checksum
`ee053d58`.

## Environment and command

```text
source revision: 8302658d9837882b9faa196b40a7462d64049216
Linux 7.1.8-zen1-3-zen x86_64
CPU: AMD Ryzen 9 9950X3D, 16 cores / 32 threads
rustc 1.95.0 (59807616e 2026-04-14)
```

```bash
./scripts/tests/rv32-c-codegen-audit.sh
```

The deterministic artifacts are:

```text
dbt-register-pressure.tsv          fe3cd744e2fb8c717d1bc56386aba9e30a4a8e5c64dd06417a9578403622e9c8
dbt-register-pressure-weighted.tsv 107ed7049850322946900935808545f55b08b90534e29587c05b7de32d34f700
dbt-code-cache.bin                 3d187b75e14dee9bdc42c0526b45e393c307d3a99dd894b10f1e3d94fdfdf596
```

Hardware performance counters were unavailable because `perf` was absent.
They are not required for this diagnostic selection.

## Weighted result

The profile covers 3,918,206 dynamically executed guest instructions across
89 live blocks. `entry_arch_loads` are weighted by external entries only;
body events are weighted by block executions; local-loop reconciliation stores
are weighted only by taken self-backedges.

| Event | Static total | Weighted total | Per million guest instructions |
|---|---:|---:|---:|
| Entry architectural loads | 29 | 23,006 | 5,871.565 |
| Body architectural loads | 424 | 570,162 | 145,516.086 |
| Dirty live eviction stores | 65 | 407,531 | 104,009.590 |
| Dead evictions | 20 | 10,004 | 2,553.209 |
| Clean evictions | 115 | 174,089 | 44,430.793 |
| Allocation-pressure events | 200 | 591,624 | 150,993.592 |
| `RAX` clobber sites | 264 | 1,181,580 | 301,561.480 |
| `RCX` clobber sites | 102 | 213,129 | 54,394.537 |
| `RDX` clobber sites | 130 | 606,504 | 154,791.249 |

The cache reaches all seven available stable host registers. Body reloads plus
dirty live eviction stores total 977,693 dynamic memory operations, about
249,526 per million guest instructions. This is material pressure rather than
a cold-block artifact.

## Dominant block

Guest PC `0x00000608` accounts for most of the actionable pressure:

```text
block executions:             63,000
self-backedges:               62,000
external entries:              1,000
guest instructions per block:     13
body loads per execution:           6
dirty live stores per execution:    6
allocation-pressure events:         8
RCX clobber sites:                   1
```

Its weighted contribution is 504,000 allocation-pressure events, 378,000 body
loads, and 378,000 dirty stores. It therefore contributes about 85.2% of all
allocation pressure and 92.8% of all dirty live eviction stores. The corrected
entry weighting contributes only 5,000 loads for this block; multiplying
preload traffic by all 63,000 loop iterations would be incorrect because the
self-backedge target is after the preload.

## Decision

Select **`RCX` as the first and only Phase 2 overflow-register candidate**.

`RCX` has by far the lowest fixed-use frequency: 54,395 clobber sites per
million guest instructions, versus 154,791 for `RDX` and 301,561 for `RAX`.
In the dominant block, eight allocation-pressure events occur for every one
`RCX`-clobber site. This gives the candidate useful opportunity while bounding
the number of forced releases needed for variable shifts, stores, `JALR`, and
`MULHSU`.

Phase 2 will add `RCX` only as overflow capacity, keep the existing seven
stable registers preferred, and force-release an `RCX` resident immediately
before an instruction that actually clobbers it. The audit must then report
actual forced-release stores/discards; no baseline projection is treated as
measured behavior.

Keep the allocator candidate only if the focused 21-warm-sample self-A/B shows
at least a 1.0% Cached DBT median improvement, Direct DBT regresses by no more
than 1.0%, dynamically weighted spill/reload traffic falls, checksum and full
correctness remain exact, and steady-state execution allocations remain zero.
Otherwise remove the candidate and retain this audit infrastructure.

## Phase 2 result: RCX overflow rejected

The candidate was implemented in `00b4173` and its forced-release telemetry in
`0785806`. Stable hosts remained preferred; `RCX` was the ninth Direct and
eighth Chainable host. Fixed-`RCX` lowerers reserved it before operand
allocation and preserved a live dirty resident when required.

The focused audit showed that the extra resident did what the allocator model
predicted:

| Weighted event | Seven-host baseline | RCX candidate | Delta |
|---|---:|---:|---:|
| Body architectural loads | 570,162 | 254,171 | -55.4% |
| Dirty live eviction stores | 407,531 | 145,527 | -64.3% |
| Allocation-pressure events | 591,624 | 187,050 | -68.4% |
| Forced live `RCX` stores | - | 3,066 | - |
| Forced dead `RCX` discards | - | 0 | - |
| Forced clean `RCX` discards | - | 72,003 | - |

The candidate reached eight resident guest registers. At the dominant block
`0x00000608`, body loads fell from six to one per execution, dirty stores from
six to two, and allocation-pressure events from eight to two. The audit still
completed with checksum `ee053d58`.

Runtime qualification nevertheless failed the mandatory gate:

| Execution mode | RCX off, ns/kernel | RCX on, ns/kernel | RCX-on delta |
|---|---:|---:|---:|
| Cached DBT, 21 alternating warm samples | 378,112.628 | 422,637.943 | +11.776% |
| Direct DBT, 21 alternating warm samples | 448,464,048.000 | 451,960,539.000 | +0.780% |

Both sides produced checksum `ee053d58`, identical retired-instruction counts,
and zero steady-state allocations. Reversing the Cached baseline/candidate
labels confirmed the result: RCX-off was 10.806% faster than RCX-on, equivalent
to RCX-on being about 12.1% slower in that run. The regression is therefore not
an A/B ordering artifact.

Translation itself did not explain the Cached regression: in the first Cached
run, RCX-on lift, lower, and publish medians changed by -0.374%, -4.511%, and
-1.620% respectively. The failure is in generated-code steady-state behavior.
The experiment demonstrates that fewer architectural spill/reload operations
are not sufficient evidence of better host execution; no narrower
microarchitectural root cause is claimed by this audit.

Decision: **REJECT**. The candidate and candidate-specific counters were
removed by `f5b5af2` and `ab3fbcb`; the seven-host Chainable allocator remains
the product implementation. The general register-pressure audit remains
available for later allocator experiments.

The ignored raw self-A/B reports produced during qualification had these
SHA-256 hashes:

```text
cached forward  1a94c3452de498c21658715ec63b92dfaa06041a54f782f1f4e4a4e683e8587b
cached reversed 8758926e02ab364232b5ecf45918b7c6b2bb6532c1fe5688ead51be365953e31
direct          54fc17b93960d46d9a719a831101d925aa5c9e72b76ec94e82439c000afb686d
```

## Follow-up: RCX paired with 64-byte block-base alignment

The Phase 2 rejection above remains the result for RCX with the former
32-byte block-base alignment. A hardware-counter follow-up isolated a separate
generated-code layout regression: relative to the seven-register baseline,
RCX/32 reduced retired host instructions by 3.3% but increased cycles by 13.0%
and frontend op-cache misses by 169%. Changing only block-base alignment from
32 to 64 bytes removed that frontend regression. In a pinned control, the
combined RCX/64 candidate reduced time by 5.2%, cycles by 4.2%, and op-cache
misses by 38.1% relative to the original seven-register/32-byte baseline.

The combined implementation was restored in `fda05e7` and `c1253ef`, then
made the product default in `42bb39e`. The restore also corrected two latent
fixed-register declarations discovered by the all-feature lowering audit:
loads no longer reserve unused `RCX`, while `JALR` now reserves the `RCX` it
actually uses.

### Shared optimized C / QEMU control

The full 21-warm-sample C matrix used checksum `ee053d58` for every backend.
The product geometry is a 128 KiB per-VM cache, 256 sets, 16 guest
instructions per block, and 64-byte block-base alignment.

| System | ns/kernel | Versus native | Versus QEMU |
|---|---:|---:|---:|
| Native Clang `-O3 -march=native -flto` | 61,578 | 1.000x | 0.120x |
| QEMU 11.1.0 TCG | 512,310 | 8.320x | 1.000x |
| Wasmtime 47.0.3 AOT | 138,509 | 2.249x | 0.270x |
| RV32 Cached DBT, product geometry | 366,312 | 5.949x | 0.715x |

On this larger register-pressure workload the product DBT is about 1.40x as
fast as QEMU TCG, while remaining 5.95x slower than native code. Translation
published 89 blocks: 40,039 bytes of live code and 42,860 bytes including
alignment padding. Steady execution performed zero allocator allocations.

### Short product workloads

The general product suite was also run against the historical seven-register,
32-byte-aligned revision with identical `1000 21 7` arguments. Absolute
Cached-DBT median changes were:

| Workload | Seven-register/32 B, ns | RCX/64 B, ns | Delta |
|---|---:|---:|---:|
| compute32 | 10,757 | 10,979 | +2.1% |
| branch-mix | 8,277 | 9,142 | +10.5% |
| call-stack | 32,654 | 33,967 | +4.0% |
| memory-sequential | 10,891 | 11,046 | +1.4% |
| memory-random | 11,532 | 11,826 | +2.5% |
| copy-checksum | 274,773 | 284,097 | +3.4% |
| mmio-control | 53,177 | 54,340 | +2.2% |
| packet-ring | 34,362 | 34,458 | +0.3% |
| trap-roundtrip | 76,000 | 78,803 | +3.7% |

The absolute-time geometric mean regressed by 3.3%. The combined change is
therefore a workload-sensitive trade: it improves the large C workload and
frontend behavior but is not a universal win for small programs. Keep it as
the current product default while future work investigates the branch-mix
outlier and adaptive/shared code layout; do not use the C result alone as a
claim that every workload became faster.

### Per-machine memory footprint

The resident population benchmark measures 79,656 bytes (77.789 KiB) of live
Rust heap per constructed Cached-DBT machine. This includes the 16 KiB guest
RAM and DBT metadata, but the allocator counter cannot see executable `mmap`
regions.

The 128 KiB code cache and 8 KiB translation scratch each have RW and RX
aliases. They therefore reserve 278,528 bytes (272 KiB) of virtual address
space per machine, backed by at most 139,264 unique bytes (136 KiB); the two
aliases do not duplicate physical backing pages. The useful bounds are:

| Accounting view | One VM | 1,024 VMs |
|---|---:|---:|
| Counted Rust heap, guest RAM included | 77.789 KiB | 77.789 MiB |
| Additional RW+RX virtual mappings | 272.000 KiB | 272.000 MiB |
| Accounted virtual footprint | 349.789 KiB | 349.789 MiB |
| Maximum unique backing plus heap | 213.789 KiB | 213.789 MiB |

The physical figure is an upper bound: executable pages are demand-backed and
an ordinary workload need not fill the whole cache. Conversely it excludes
allocator/page-table overhead and shared process code, so it is not a whole
server RSS prediction. RCX itself adds no measurable per-VM storage; the
resident heap was byte-identical to the historical baseline.

## Four-corner register/alignment matrix

A same-process matrix separated the register-pool and block-alignment changes
instead of comparing only the historical Stable7/32 and combined RCX/64
endpoints. Every candidate used the product geometry (256 sets, 16 guest
instructions per block, and a 128 KiB code cache), independent calibration,
rotated 21-sample measurement, checksum `ee053d58`, identical retired guest
instructions, and zero steady-state allocations.

The independently repeated optimized-C run produced:

| Register pool | Block base | ns/kernel | Versus Stable7/32 |
|---|---:|---:|---:|
| Stable7 | 32 B | 382,340 | 1.000x |
| Stable7 | 64 B | 382,131 | 0.999x |
| RCX overflow | 32 B | 421,692 | 1.103x |
| RCX overflow | 64 B | 367,190 | 0.960x |

The first run gave the same shape: RCX/32 was 1.105x baseline and RCX/64 was
0.962x. In the repeat, changing 32 to 64 bytes was neutral for Stable7
(0.999x), but improved RCX by 13.0% (0.871x). Equivalently, adding RCX cost
10.3% at 32 bytes and saved 3.9% at 64 bytes. The ratio-of-ratios interaction
was 0.871, confirming that register capacity and generated-code placement
cannot be evaluated as independent optimizations on this workload.

The full product-workload matrix was much less polarized. Its geometric means
relative to Stable7/32 were 1.017x for Stable7/64, 1.011x for RCX/32, and
1.018x for RCX/64. Individual rows moved in both directions, with the largest
RCX/32 slowdown at 5.0% on `memory-random`; all semantic and allocation gates
still passed.

Decision: retain RCX/64 as the product default. The four-corner result explains
why the earlier RCX/32 experiment was rejected and why restoring RCX only after
the 64-byte alignment fix succeeded. It does not justify treating 64-byte
alignment as universally faster: Stable7 gained nothing on optimized C and the
short-suite geometric mean slightly favored Stable7/32. Future register-cache
experiments must therefore qualify the exact generated-code layout they ship,
not extrapolate runtime from spill counts alone.
