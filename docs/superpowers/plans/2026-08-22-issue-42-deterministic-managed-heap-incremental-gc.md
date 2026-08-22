# Deterministic Managed Heap and Incremental GC Implementation Plan

> Issue: [#42](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/42)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the crate-private artifact v1 managed heap, heap-backed instructions, zero-copy literals, compact dynamic strings, bounded incremental collection, and deterministic guest/maintenance accounting specified by #40.

**Architecture:** Admission derives portable layouts and reserves a fixed arena plus every metadata table. `Heap` owns TLSF blocks and stable handles, `Collector` owns resumable root/mark/sweep cursors, and `Machine` coordinates block-atomic execution with unpublished allocation continuations and separate guest/maintenance budgets. Artifact-backed literals occupy an immortal reference domain while dynamic objects remain in the per-instance arena.

**Tech Stack:** Rust 2021, existing verified artifact/Tier 0 interpreter, `sha2`, fixed-capacity boxed slices, unit-test fixture artifacts, allocation-counting tests, and ignored release performance workloads.

---

## File map

- Modify `src/verify/functions.rs`: enforce dedicated allocation blocks and preserve typed heap-operation guarantees.
- Modify `src/verify/tests.rs`: add allocation-boundary and heap-operation verification vectors.
- Modify `src/execution/mod.rs`: declare focused heap, layout, GC, heap-op, and text modules.
- Modify `src/execution/value.rs`: replace fixture-shaped references with compact tagged tokens and owner-aware entry arguments.
- Modify `src/execution/error.rs`: add bounded heap traps, diagnostics, phase outcomes, and two-budget validation errors.
- Modify `src/execution/image.rs`: resolve portable layouts, fields, statics, literal descriptors, root maps, and metadata reservations.
- Create `src/execution/layout.rs`: portable widths, alignment, inherited object layouts, arrays, strings, and checked charges.
- Create `src/execution/heap.rs`: admitted arena, canonical TLSF mapping, stable handles, identity hashes, and bounded diagnostics.
- Create `src/execution/gc.rs`: deterministic stop-the-world root/mark/sweep state machine.
- Create `src/execution/heap_ops.rs`: allocation continuations and object/array/field/static/type instruction semantics.
- Create `src/execution/text.rs`: artifact-backed literals and compact dynamic string operations.
- Modify `src/execution/machine.rs`: instance ownership, dispatch, guest/maintenance phases, tracing, and resumption.
- Modify `src/execution/fixtures.rs`: verified heap graphs, allocation workloads, fragmented arenas, and UTF-16 cases.
- Modify `src/execution/tests.rs`: shared allocation counter and end-to-end machine tests.
- Create `src/execution/heap_tests.rs`: layout, allocator, handle, and heap-op tests.
- Create `src/execution/gc_tests.rs`: roots, graph tracing, progress, fragmentation, and OOM tests.
- Create `src/execution/text_tests.rs`: literal and dynamic-string conformance tests.
- Modify `docs/performance/tier0-baseline.md`: retain scalar/control history and link the heap baseline.
- Create `docs/performance/managed-heap-baseline.md`: reproducible release heap/GC measurements.

### Task 1: Enforce allocation block boundaries

**Files:**
- Modify: `src/verify/functions.rs`
- Modify: `src/verify/tests.rs`
- Modify: `src/execution/fixtures.rs`
- Modify: `tests/support/mod.rs`
- Regenerate: `tests/fixtures/language-runtime.cpkt`
- Regenerate: `tests/fixtures/language-runtime.manifest.md`

- [x] **Step 1: Write failing verifier tests**

Add `cfg_rejects_allocation_after_another_instruction`,
`cfg_rejects_two_allocations_in_one_block`, and
`cfg_accepts_dedicated_object_and_array_allocation_blocks`. Build blocks with
`nop; new_object`, `new_object; new_array`, and one allocation followed only by
non-allocating instructions plus a terminator.

- [x] **Step 2: Prove the red state**

Run: `cargo test --locked --offline verify::tests::cfg_rejects_allocation_after_another_instruction -- --nocapture && cargo test --locked --offline verify::tests::cfg_rejects_two_allocations_in_one_block -- --nocapture`

Expected: both rejection tests FAIL because the current verifier accepts the
instruction sequences.

- [x] **Step 3: Add the exact verifier rule**

In the per-block instruction walk, keep `allocation_seen: bool`. Reject a
`NewObject` or `NewArray` unless its instruction index is zero, and reject a
second allocation in the same block. Do not classify `Const(String)` as an
allocation because #40 literals are immortal artifact-backed references.

```rust
let is_allocation = matches!(instruction, Instruction::NewObject { .. } | Instruction::NewArray { .. });
if is_allocation && (instruction_index != 0 || allocation_seen) {
    return Err(code_failure(limits, module_id, function_id,
        "allocation must be the first and only allocating instruction in its block"));
}
allocation_seen |= is_allocation;
```

Update every heap fixture so it remains verifier-valid. In the committed
language-runtime vector, move the array length/index constants into block zero,
make block one begin with `new_array`, and regenerate its bytes and manifest.

- [x] **Step 4: Run verifier and full tests**

Run: `cargo test --locked --offline verify::tests -- --nocapture && cargo test --locked --offline`

Expected: all tests PASS.

- [x] **Step 5: Commit**

```bash
git add src/verify/functions.rs src/verify/tests.rs src/execution/fixtures.rs tests/support/mod.rs tests/fixtures/language-runtime.cpkt tests/fixtures/language-runtime.manifest.md docs/superpowers/plans/2026-08-22-issue-42-deterministic-managed-heap-incremental-gc.md
git commit -m "fix(vm): require dedicated allocation blocks (#42)"
```

### Task 2: Derive portable heap layouts during admission

**Files:**
- Create: `src/execution/layout.rs`
- Modify: `src/execution/mod.rs`
- Modify: `src/execution/image.rs`
- Modify: `src/execution/error.rs`
- Modify: `src/execution/fixtures.rs`
- Create: `src/execution/heap_tests.rs`

- [x] **Step 1: Write failing layout tests**

Add tests for empty/minimum objects, mixed `bool/char/i64/ref` fields,
superclass prefixes, primitive/reference arrays, both string encodings, every
15/16/17-byte alignment edge, negative/overflow lengths, and exact metadata
counts. Assert these public-to-execution formulas:

```rust
assert_eq!(32, object_layout(&empty_class)?.block_bytes);
assert_eq!(48, array_layout(ValueWidth::Char, 9)?.block_bytes);
assert_eq!(32, string_layout(StringEncoding::Latin1, 8)?.block_bytes);
assert_eq!(48, string_layout(StringEncoding::Utf16, 8)?.block_bytes);
```

- [x] **Step 2: Prove the behavior is missing**

Run: `cargo test --locked --offline execution::heap_tests::portable -- --nocapture`

Expected: FAIL with compiling placeholder layouts returning the wrong sizes and
accepting invalid lengths. This keeps RED behavioral rather than treating a
compiler error as a regression test.

- [x] **Step 3: Implement checked portable layout types**

Create `ValueWidth`, `FieldLayout`, `ObjectLayout`, `ArrayLayout`,
`StringEncoding`, `StringLayout`, and `StoragePlan`. Use 16-byte block headers,
32-byte minimum blocks, natural value alignment, decreasing-alignment field
groups, superclass prefixes, eight-byte array/string headers, and checked
round-up helpers. Return `AdmissionError::StoragePlanOverflow` on every failed
conversion or arithmetic operation.

Resolve and box in `ExecutionImage`: runtime type layouts, field offsets,
static-slot IDs/types, reference-field lists, artifact-wide raw-byte literal
deduplication, and `floor(heap_bytes / 32)` handle capacity. Admission must
reserve every derived collection before publishing the image.

- [x] **Step 4: Run layout, admission, and full tests**

Run: `cargo test --locked --offline execution::heap_tests::portable -- --nocapture && cargo test --locked --offline execution::image::tests -- --nocapture && cargo test --locked --offline`

Expected: all tests PASS.

- [x] **Step 5: Commit**

```bash
git add src/execution/mod.rs src/execution/layout.rs src/execution/image.rs src/execution/heap_tests.rs
git commit -m "feat(vm): admit portable managed layouts (#42)"
```

### Task 3: Introduce compact opaque reference domains

**Files:**
- Modify: `src/execution/value.rs`
- Modify: `src/execution/image.rs`
- Modify: `src/execution/machine.rs`
- Modify: `src/execution/tests.rs`
- Modify: `src/execution/fixtures.rs`

- [x] **Step 1: Write failing token and entry-owner tests**

Cover all four domains, the `2^30 - 1` slot limit, generation equality,
cross-image entry rejection, stale admitted-host references, and unchanged
canonical trace payloads.

```rust
assert_eq!(8, core::mem::size_of::<ReferenceValue>());
assert_ne!(managed(1, 7), literal(1));
assert_eq!(ReferenceDomain::Managed, managed(1, 7).domain());
```

- [x] **Step 2: Prove the red state**

Run: `cargo test --locked --offline execution::tests::compact_reference -- --nocapture`

Expected: FAIL because the current reference stores image and type metadata and
is larger than eight bytes.

- [x] **Step 3: Implement the exact private token**

Define `ReferenceValue { tagged_slot: u32, generation: u32 }`, two high-bit
domain tags, checked constructors, and accessors. Move entry ownership to
`EntryArgument { owner: Option<[u8; 32]>, value: RuntimeValue }`. Resolve a
reference dynamic type through the image host/literal descriptors or the heap
handle table. Adapt trace encoding to receive the resolved `TypeKey` while
preserving the existing 16-byte symbolic trace payload.

- [x] **Step 4: Run reference, trace, release, and full tests**

Run: `cargo test --locked --offline execution::tests::references -- --nocapture && cargo test --locked --offline execution::tests::block_boundary_trace -- --nocapture && cargo test --release --locked --offline execution::tests::block_boundary_trace -- --nocapture && cargo test --locked --offline`

Expected: all tests PASS and existing trace digests remain unchanged.

- [x] **Step 5: Commit**

```bash
git add src/execution/value.rs src/execution/image.rs src/execution/machine.rs src/execution/tests.rs src/execution/fixtures.rs
git commit -m "refactor(vm): compact opaque reference tokens (#42)"
```

### Task 4: Build the admitted TLSF arena and stable handle table

**Files:**
- Create: `src/execution/heap.rs`
- Modify: `src/execution/mod.rs`
- Modify: `src/execution/error.rs`
- Modify: `src/execution/heap_tests.rs`
- Modify: `src/execution/tests.rs`

- [x] **Step 1: Write failing allocator/handle tests**

Test canonical `(f,j)` mapping, upward request mapping, bitmap selection, LIFO
reuse, 16-byte exact splits, 16-byte slack, neighbor coalescing, lowest valid
generation reuse, wrap retirement through injected test state, SplitMix64
identity hashes, and bounded diagnostic counters.

- [x] **Step 2: Prove the red state**

Run: `cargo test --locked --offline execution::heap_tests::allocator -- --nocapture`

Expected: FAIL with a compiling `Heap` seam returning no reservation, proving
the missing allocator behavior rather than treating a compiler error as RED.

- [x] **Step 3: Implement fixed storage and invariants**

Create boxed arena bytes, boxed handle entries, fixed class heads, and bitmap
words in `Heap::new(&StoragePlan)`. Define `BlockOffset(u32)`, `HandleEntry`,
`HandleState`, `AllocationRequest`, and `HeapDiagnostic`. Implement bounded
`find/split/free/coalesce`, private reserve/commit/abort, generation retirement,
runtime type lookup, identity hash assignment, and scalar statistics. No method
after `Heap::new` may reserve or grow a collection.

- [x] **Step 4: Run allocator tests under the allocation counter**

Run: `cargo test --locked --offline execution::heap_tests::allocator -- --nocapture --test-threads=1 && cargo test --release --locked --offline execution::heap_tests::allocator_steady_state_allocates_nothing -- --nocapture --test-threads=1`

Expected: PASS with zero counted allocations in measured operations.

- [x] **Step 5: Commit**

```bash
git add src/execution/mod.rs src/execution/error.rs src/execution/heap.rs src/execution/heap_tests.rs
git commit -m "feat(vm): add bounded TLSF managed arena (#42)"
```

### Task 5: Add resumable unpublished allocation

**Files:**
- Create: `src/execution/heap_ops.rs`
- Modify: `src/execution/mod.rs`
- Modify: `src/execution/error.rs`
- Modify: `src/execution/machine.rs`
- Modify: `src/execution/fixtures.rs`
- Modify: `src/execution/heap_tests.rs`

- [x] **Step 1: Write failing allocation-state tests**

Cover fixed-cost-once behavior, negative array length before heap mutation,
oversized immediate OOM, one 16-byte initialization unit per budget unit,
multi-slice cursor persistence, zero/null initialization, destination
non-publication, commit, and cancellation rollback.

- [x] **Step 2: Prove the red state**

Run: `cargo test --locked --offline execution::heap_tests::allocation -- --nocapture`

Expected: FAIL because heap opcodes are still rejected at admission.

- [x] **Step 3: Implement the pending allocation state machine**

Add `PendingAllocation::{Object,Array}` with request, private handle/block,
initialized cursor, fixed-cost-paid, and collection-attempted fields. Extend
admission with resolved `NewObject/NewArray`. In `Machine`, charge the block
once, validate operands, request or reserve storage, initialize at most
`guest_budget * 16` logical bytes, and atomically commit handle plus destination.
Add `GuestTrap::NegativeArraySize` and a structured allocation-exhaustion state;
do not yet start GC.

- [x] **Step 4: Run allocation and scalar regression suites**

Run: `cargo test --locked --offline execution::heap_tests::allocation -- --nocapture && cargo test --locked --offline execution::tests -- --nocapture`

Expected: all tests PASS and scalar/control accounting is unchanged.

- [x] **Step 5: Commit**

```bash
git add src/execution/mod.rs src/execution/error.rs src/execution/heap_ops.rs src/execution/machine.rs src/execution/fixtures.rs src/execution/heap_tests.rs
git commit -m "feat(vm): initialize managed allocations incrementally (#42)"
```

### Task 6: Execute object, array, static, and type operations

**Files:**
- Modify: `src/execution/image.rs`
- Modify: `src/execution/heap_ops.rs`
- Modify: `src/execution/machine.rs`
- Modify: `src/execution/error.rs`
- Modify: `src/execution/fixtures.rs`
- Modify: `src/execution/heap_tests.rs`

- [x] **Step 1: Write failing semantic vectors**

Exercise inherited field offsets, primitive/reference fields, per-instance
statics, all primitive/reference array widths, negative and upper bounds,
read-before-write aliasing, zero-initialized non-null failures, exact dynamic
type/interface tests, nullable/non-null casts, stale handles, and atomic failed
stores.

- [x] **Step 2: Prove the red state**

Run: `cargo test --locked --offline execution::heap_tests::heap_instructions -- --nocapture`

Expected: FAIL with admission rejection or unsupported instructions.

- [x] **Step 3: Implement resolved heap instruction dispatch**

Admit and dispatch `array_length/load/store`, `field_get/set`, `static_get/set`,
`is_type`, and `checked_cast`. Read all sources first, resolve the token and
runtime type, perform bounds/null/type checks, then publish one destination or
store. Add bounded traps for null, bounds, and cast failures; impossible dead,
foreign, or type-confused internal tokens produce the specified `VmFault`.

- [x] **Step 4: Run heap semantics in debug and release**

Run: `cargo test --locked --offline execution::heap_tests::heap_instructions -- --nocapture && cargo test --release --locked --offline execution::heap_tests::heap_instructions -- --nocapture`

Expected: exact values, traps, charges, and traces match.

- [x] **Step 5: Commit**

```bash
git add src/execution/image.rs src/execution/heap_ops.rs src/execution/machine.rs src/execution/error.rs src/execution/fixtures.rs src/execution/heap_tests.rs
git commit -m "feat(vm): execute managed object operations (#42)"
```

### Task 7: Implement bounded stop-the-world collection

**Files:**
- Create: `src/execution/gc.rs`
- Create: `src/execution/gc_tests.rs`
- Modify: `src/execution/mod.rs`
- Modify: `src/execution/heap.rs`
- Modify: `src/execution/heap_ops.rs`
- Modify: `src/execution/machine.rs`
- Modify: `src/execution/fixtures.rs`

- [x] **Step 1: Write failing graph/progress tests**

Build cycles, diamonds, duplicate roots, shared children, reference arrays,
unreachable islands, statics, and multiple frames. Assert one exact root/edge/
leaf/sweep action per maintenance unit, FIFO gray order, epoch switching,
ascending sweep offsets, no guest progress while active, one retry, and zero
heap reads/spend while idle.

- [x] **Step 2: Prove the red state**

Run: `cargo test --locked --offline execution::gc_tests -- --nocapture`

Expected: FAIL because no collector module exists.

- [x] **Step 3: Implement `Collector` phases and machine scheduling**

Define `CollectorPhase::{Idle,Roots,Mark,Sweep}` plus root/frame/register,
object-field/array-element, and block cursors. Use the handle entry gray link and
mark epoch; enqueue once, scan reference layouts only, invalidate/coalesce on
sweep, then retry the saved request. Extend `Machine::run_slice` to accept guest
and maintenance budgets, skip guest work when GC is active at entry, discard
unused guest credit on a new collection, and never resume guest work after the
maintenance phase in the same call.

- [x] **Step 4: Run one-unit, allocation-free, and full regressions**

Run: `cargo test --locked --offline execution::gc_tests -- --nocapture && cargo test --release --locked --offline execution::gc_tests::collector_steady_state_allocates_nothing -- --nocapture --test-threads=1 && cargo test --locked --offline`

Expected: all tests PASS with zero measured allocations and zero idle work.

- [x] **Step 5: Commit**

```bash
git add src/execution/mod.rs src/execution/gc.rs src/execution/gc_tests.rs src/execution/heap.rs src/execution/heap_ops.rs src/execution/machine.rs src/execution/fixtures.rs
git commit -m "feat(vm): collect managed graphs incrementally (#42)"
```

### Task 8: Add artifact-backed and compact dynamic strings

**Files:**
- Create: `src/execution/text.rs`
- Create: `src/execution/text_tests.rs`
- Modify: `src/artifact/mod.rs`
- Modify: `src/decode/code.rs`
- Modify: `src/verify/functions.rs`
- Modify: `src/verify/tests.rs`
- Modify: `src/test_encode.rs`
- Modify: `src/execution/mod.rs`
- Modify: `src/execution/image.rs`
- Modify: `src/execution/heap.rs`
- Modify: `src/execution/heap_ops.rs`
- Modify: `src/execution/machine.rs`
- Modify: `src/execution/fixtures.rs`

- [x] **Step 1: Write failing literal and string-operation tests**

First add decoder/verifier vectors for opcodes `0x60` through `0x66`, form-zero
and operand canonicality, exact `kotlin.String` export resolution, operand and
result types, and dedicated-block rules for concat/substring. Then cover
cross-module raw UTF-16 deduplication, zero-copy loads, empty/Latin-1/BMP/
surrogate literals, literal identity, dynamic non-interning, compact selection,
length/get, content equality, unsigned comparison, exact wrapping hash, full/
empty/proper substring identity, fresh concat, resumable publication, and
strict/replacement UTF-8 conversion.

- [x] **Step 2: Prove the red state**

Run: `cargo test --locked --offline execution::text_tests -- --nocapture`

Expected: FAIL first because the String opcodes are unknown, then because
string constants are not admitted as runtime references.

- [x] **Step 3: Implement immutable literal and dynamic string access**

Add the closed Artifact v1 instruction family `StringLength`, `StringGet`,
`StringEquals`, `StringCompare`, `StringHash`, `StringConcat`, and
`StringSubstring` at opcodes `0x60..=0x66`. Resolve exactly one public-library
`kotlin.String` final class when the family or a String constant is used, and
verify its exact register signatures. Treat concat and substring as potential
allocations for dedicated-block verification.

Resolve `Constant::String` to immortal literal-domain tokens. Keep payload as
verified artifact byte ranges. Implement a unified code-unit reader over
artifact, Latin-1, and UTF-16 backings. Add fixed-capacity resumable cursors for
hash/compare/concat/substring/UTF-8; charge one guest unit per eight code units
plus ordinary allocation initialization. Preserve isolated surrogates and
publish dynamic results only after complete initialization.

- [x] **Step 4: Run text tests in debug/release and allocation counting**

Run: `cargo test --locked --offline execution::text_tests -- --nocapture && cargo test --release --locked --offline execution::text_tests -- --nocapture && cargo test --release --locked --offline execution::text_tests::literal_load_allocates_nothing -- --nocapture --test-threads=1`

Expected: all tests PASS and literal load performs zero allocations.

- [x] **Step 5: Commit**

```bash
git add docs/superpowers/specs/2026-08-22-issue-40-deterministic-managed-heap-incremental-gc-design.md docs/superpowers/plans/2026-08-22-issue-42-deterministic-managed-heap-incremental-gc.md src/artifact/mod.rs src/decode/code.rs src/verify/functions.rs src/verify/tests.rs src/test_encode.rs src/execution/mod.rs src/execution/text.rs src/execution/text_tests.rs src/execution/image.rs src/execution/heap.rs src/execution/heap_ops.rs src/execution/machine.rs src/execution/fixtures.rs
git commit -m "feat(vm): execute compact Kotlin strings (#42)"
```

### Task 9: Guarantee emergency OOM and fragmentation recovery

**Files:**
- Modify: `src/execution/error.rs`
- Modify: `src/execution/heap.rs`
- Modify: `src/execution/heap_ops.rs`
- Modify: `src/execution/machine.rs`
- Modify: `src/execution/gc_tests.rs`

- [x] **Step 1: Write failing OOM vectors**

Cover oversized immediate OOM with zero GC units, failed-fit full collection
plus one retry, capacity and fragmentation failures, identical singleton
identity across catches, bounded scalar diagnostics, dropping roots then
successful recollection, and injected missing-emergency-state `VmFault`.

- [x] **Step 2: Prove the red state**

Run: `cargo test --locked --offline execution::gc_tests::oom -- --nocapture`

Expected: FAIL because allocation exhaustion has no managed emergency identity.

- [x] **Step 3: Implement immortal OOM delivery**

Create the OOM-domain token during instance construction, keep its message,
cause, suppressed list, and writable trace absent, and update the fixed
`HeapDiagnostic { request_kind, requested, live, total_free, largest_free,
source }`. Map exhaustion to the reusable reference after exactly the specified
GC/retry sequence. Keep exception-table unwinding out of scope by exposing the
structured trap plus emergency token to its follow-up.

- [x] **Step 4: Run OOM, debug/release determinism, and allocation tests**

Run: `cargo test --locked --offline execution::gc_tests::oom -- --nocapture && cargo test --release --locked --offline execution::gc_tests::oom -- --nocapture && cargo test --release --locked --offline execution::gc_tests::oom_delivery_allocates_nothing -- --nocapture --test-threads=1`

Expected: all tests PASS with identical accounting and zero native allocation.

- [x] **Step 5: Commit**

```bash
git add src/execution/error.rs src/execution/heap.rs src/execution/heap_ops.rs src/execution/machine.rs src/execution/gc_tests.rs
git commit -m "feat(vm): guarantee bounded managed OOM delivery (#42)"
```

### Task 10: Lock conformance, resident bounds, and performance evidence

**Files:**
- Modify: `src/execution/tests.rs`
- Modify: `src/execution/heap_tests.rs`
- Modify: `src/execution/gc_tests.rs`
- Modify: `src/execution/text_tests.rs`
- Modify: `docs/performance/tier0-baseline.md`
- Create: `docs/performance/managed-heap-baseline.md`

- [x] **Step 1: Add end-to-end golden and ignored release workloads**

Add one representative artifact that allocates inherited objects and reference
arrays, creates a cycle, retains through a static, drops it, performs one-unit
collection slices, reuses storage, loads literals, builds compact strings, and
reaches OOM/recovery. Lock exact outcomes, guest/maintenance totals, trace
digest, heap statistics, and debug/release equality. Add ignored workloads for
allocation sizes, field/array/text operations, root/edge/leaf/sweep units,
fragmentation, and 10,000 idle instances.

- [x] **Step 2: Run focused golden tests in both profiles**

Run: `cargo test --locked --offline execution::tests::managed_heap_vertical -- --nocapture && cargo test --release --locked --offline execution::tests::managed_heap_vertical -- --nocapture`

Expected: both profiles PASS with identical semantic digests and totals.

- [x] **Step 3: Run release workloads and write the observed baseline**

Run: `cargo test --release --locked --offline managed_heap_performance -- --ignored --nocapture --test-threads=1`

Record the exact command, host/build profile, workload sizes, operation rates,
GC unit/pause distributions, live/free/largest-block/slack bytes, metadata and
resident totals, and idle zero-work result in
`docs/performance/managed-heap-baseline.md`. Link it from the Tier 0 baseline.

- [x] **Step 4: Run the complete verification matrix**

Run: `cargo fmt --all -- --check && cargo test --locked --offline && cargo test --release --locked --offline && cargo clippy --all-targets --all-features --locked --offline -- -D warnings && git diff --check`

Expected: every command exits zero; non-ignored tests report zero failures.

- [x] **Step 5: Commit**

```bash
git add src/execution/tests.rs src/execution/heap_tests.rs src/execution/gc_tests.rs src/execution/text_tests.rs docs/performance/tier0-baseline.md docs/performance/managed-heap-baseline.md
git commit -m "test(vm): lock managed heap conformance and baselines (#42)"
```

### Task 11: Record roadmap completion and superproject pin

**Files:**
- Modify: `docs/superpowers/plans/2026-08-22-issue-42-deterministic-managed-heap-incremental-gc.md`
- Modify in superproject: `host/compukter-vm` gitlink

- [x] **Step 1: Mark every verified plan checkbox complete**

Change only steps supported by fresh command output from `[ ]` to `[x]`.

- [x] **Step 2: Run final clean-tree verification**

Run in VM: `cargo fmt --all -- --check && cargo test --locked --offline && cargo test --release --locked --offline && cargo clippy --all-targets --all-features --locked --offline -- -D warnings && git status --short`

Expected: verification commands exit zero and status contains only the plan
checkbox update before its final commit.

- [x] **Step 3: Commit the verified plan record**

```bash
git add docs/superpowers/plans/2026-08-22-issue-42-deterministic-managed-heap-incremental-gc.md
git commit -m "docs(vm): record managed heap verification (#42)"
```

- [ ] **Step 4: Update GitHub only after every acceptance item is evidenced**

Comment on #42 with the commit range, exact test counts/commands, zero-allocation
results, baseline path, and crate-private API confirmation. Close #42 as
completed and move it to Done. If any item lacks evidence, leave it open in Now
and identify that exact item.

- [ ] **Step 5: Pin the VM commit in the `Compukters` superproject**

Create or reuse the roadmap-tracked superproject pin issue, update only
`host/compukter-vm`, run the parent verification required by its issue, commit
the gitlink on `dev`, and close the pin issue only after the remote VM commit is
available.
