# Cached DBT Hit Path Optimization

> Issue: [#498](https://github.com/CertifiedBadIdeas/Compukter-Kraft/issues/498)

## Context

`CachedDbt` avoids decoding and native translation after a cache hit, but every
resident block still crosses a comparatively expensive Rust-side dispatch path.
The current path performs repeated metadata lookups for the same cache entry,
maintains timestamp-based LRU state for a two-way set, computes the set with
runtime modulo, reconstructs `DbtContext` for every block, and only then calls
native code. This slice reduces that overhead before block chaining is
introduced, so the independent value of each optimization remains measurable.

The fixed baseline is commit `ec517f0`. `CachedDbt` is the affected product
backend. `DirectDbt` and the explicit reference interpreters remain unchanged.

## Goals

- Make a resident `CachedDbt` lookup return all execution data after one cache
  entry validation.
- Replace timestamp LRU with the minimum state required for exact two-way LRU.
- Remove division from set indexing by requiring a power-of-two set count.
- Initialize stable `DbtContext` fields once per `Rv32Machine::run` call.
- Preserve exact guest instruction budgeting, traps, W^X, `FENCE.I`, LR/SC,
  diagnostics, bounded memory, and allocation-free steady-state execution.
- Measure each step independently and retain only changes supported by evidence.

## Non-goals

- Native block chaining or direct jump patching.
- A native dispatcher loop.
- Changes to the generated block ABI, prologue, epilogue, or register flushing.
- Executable-range lookup optimization.
- Changes to the direct DBT policy.
- Concurrent code-cache publication or multi-hart execution.

## Cache Hit Descriptor

`DirectDbtCodeCache::lookup` will validate one two-way set and return a private,
copyable descriptor containing the native entry address and guest instruction
count required by dispatch. Load/store lowering counters remain aggregate
translation statistics and are not copied through the execution hot path.

The descriptor is ephemeral: it is valid only until the cache is published to
or invalidated. `Rv32DbtExecution` must consume it immediately, without any
operation that can publish, wrap, or invalidate the code cache between lookup
and native entry. This invariant is private to the single-threaded per-machine
dispatcher and is covered by tests. Chaining and future multi-hart work must
introduce a stronger lifetime or invalidation protocol rather than extending
this assumption silently.

Cache misses continue to decode and translate exactly as they do at the
baseline. Publishing a new fast block returns the same execution descriptor,
so hit and newly translated dispatch share one downstream execution path.
Bounded blocks continue to use their dedicated scratch mapping.

## Cache Geometry and Replacement

The persistent cache constructor requires `sets > 0` and
`sets.is_power_of_two()`. Other values fail before allocation with
`DbtFaultKind::Capacity`. The set index is always:

```text
mixed_key & (set_count - 1)
```

Each set stores two entries and one MRU-way bit. A hit records its way as MRU.
Publication uses an invalid way first; when both ways are valid, it replaces
the way opposite the MRU bit. For a two-way set this is exact LRU behavior.
The global access clock and per-entry `last_used` timestamps are removed.

Generation remains part of the key. `FENCE.I` still increments the generation
and invalidates every resident entry. Circular code-cache overwrite still
invalidates all metadata entries whose native code ranges overlap the newly
published range.

## Per-run DBT Context

`run_dbt` creates one `DbtContext` after obtaining the stable hart and direct
RAM pointers. The following fields are initialized once for the call:

- architectural state pointer;
- RAM base and length;
- page-permission table pointer and page count.

Before every native dispatch, the dispatcher refreshes:

- remaining guest instruction budget;
- reservation validity and address;
- the exit record.

After a native return, the existing validation and hart commit logic remains
authoritative. A Rust slow-path instruction can change or clear the LR/SC
reservation, so the next dispatch refreshes reservation fields from the hart
rather than trusting stale context values. No context is retained across
separate calls to `Rv32Machine::run`.

## Failure Handling and Safety

- Invalid cache geometry is rejected during machine construction.
- A cache miss never exposes a partial descriptor.
- Translation and publication faults keep their existing typed `DbtFault` path.
- Native exits retain all current checks for attempted count, remaining budget,
  next PC, instruction identity, memory metadata, and reservation ownership.
- No executable page becomes writable through its RX alias, and no RWX mapping
  is introduced.
- The descriptor and raw entry address remain crate-private and are never part
  of the public VM API.

## Measurement Plan

Measure the following checkpoints in order against `ec517f0`:

1. one validated cache-hit descriptor;
2. two-way MRU plus power-of-two mask indexing;
3. one DBT context per `run_dbt` call;
4. the combined retained implementation.

Each checkpoint records release product workload medians, absolute native
ratios, the focused shared-C/QEMU comparison where available, resident metadata
bytes, and steady-state allocations. Benchmark ordering, workload inputs, and
sample counts remain fixed. A change that regresses materially or produces no
repeatable benefit is reverted before the final commit unless it independently
improves code size or a measured resident-memory objective.

## Verification

- Unit tests cover power-of-two geometry, two-way replacement order, descriptor
  invalidation boundaries, and circular overwrite behavior.
- Machine tests cover exact partial budgets, `FENCE.I`, traps, RAM/MMIO,
  atomics, and equivalence with the reference backend.
- `cargo test --all-targets` passes.
- `cargo fmt --all -- --check` and `git diff --check` pass.
- The release product benchmark completes with zero steady-state allocations.
- The focused C/QEMU benchmark reports absolute native and QEMU ratios using
  the same artifact and command as the baseline.

## Follow-up Boundary

Block chaining is a separate #498 slice. It may reuse the descriptor concept,
but it must separately design budget checks, stable dispatch entries,
invalidation, and native-to-native control transfer. No chaining mechanism is
hidden inside this optimization.
