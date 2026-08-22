# Managed heap baseline

Recorded on 2026-08-22 with the release profile (`codegen-units = 1`, thin
LTO), Rust 1.95.0 (`59807616e`), host `x86_64-unknown-linux-gnu`, and CPU label
`x86_64 local`.

The exact command was:

```sh
COMPUKTER_BENCH_CPU="x86_64 local" cargo test --release --locked --offline managed_heap_performance -- --ignored --nocapture --test-threads=1
```

Setup and output formatting are outside each timed interval. Rates are local
reference measurements, not CI thresholds.

## Allocator and fragmentation

| Workload | Iterations | Elapsed ns | Operations/s | Final free bytes | Largest free block |
| --- | ---: | ---: | ---: | ---: | ---: |
| Allocate/free 32 bytes | 100,000 | 3,081,621 | 32,450,454 | 4,096 | 4,096 |
| Allocate/free 64 bytes | 100,000 | 3,085,492 | 32,409,742 | 4,096 | 4,096 |
| Allocate/free 256 bytes | 100,000 | 3,100,451 | 32,253,372 | 4,096 | 4,096 |
| Four-block fragmentation/coalescing cycle | 100,000 | 15,135,308 | 6,607,067 | 128 | 128 |

The requested block sizes are already 16-byte aligned, so these rows have zero
rounding slack. Every cycle restores the complete arena and its largest block.
The conformance suite separately locks the non-moving fragmentation case where
`total_free = 64` but `largest_free_block = 32`, so a 48-byte request fails.

## Heap operations and compact strings

Each row admits the image once, then constructs and runs 10,000 independent VM
instances.

| Workload | Iterations | Elapsed ns | Operations/s |
| --- | ---: | ---: | ---: |
| Inherited field round trip | 10,000 | 259,623,020 | 38,517 |
| Reference-array round trip | 10,000 | 261,437,505 | 38,250 |
| Two compact concats plus equality | 10,000 | 262,398,643 | 38,110 |
| Latin-1 concat, 50 UTF-16 code units | 10,000 | 263,136,211 | 38,003 |
| UTF-16 concat, 8 UTF-16 code units | 10,000 | 262,110,194 | 38,152 |

The vertical conformance observation is identical in debug and release:
fixed/dynamic/maintenance totals `[69, 7, 12]` and digest
`a32f461af53924985dfdf3041e803325fe69d3f24aa010292187eb15b5434e64`.

## Collector work and pauses

The graph contains a cycle, shared child, duplicate logical reachability, and a
static root. Each of 10,000 cycles is driven with one maintenance unit per
slice.

| Cycles | Elapsed ns | Units | Units/s | Roots | Dequeues | Edges | Sweeps | Transitions |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 10,000 | 1,424,896 | 220,000 | 154,397,233 | 20,000 | 40,000 | 80,000 | 50,000 | 30,000 |

Every graph cycle uses exactly 22 units, so the observed distribution is
minimum 22, maximum 22. The configured slice is one unit, bounding every pause
to one collector action. A separate empty-object workload recorded 10,000 leaf
actions in 175,730 ns (56,905,480 leaf units/s).

## Idle resident bound

Constructing 10,000 idle instances took 3,387,340 ns (2,952,169 instances/s).
All instances reported exactly zero maintenance units. Each has a 32-byte guest
arena; the test-only reservation accounting reported 9,160,000 bytes total,
or 916 bytes per instance. Of that, 792 bytes are the inline `Machine` value;
the remainder is its arena, handles, frames, registers, and static-slot boxed
storage. The shared immutable execution image and native allocator bookkeeping
are explicitly excluded.
