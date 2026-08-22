# Deterministic Managed Heap and Incremental GC Design

> Issue: [#40](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/40)

## Purpose

This specification defines the managed-reference, object, array, string,
static-storage, allocation, and garbage-collection semantics required by
artifact v1. It extends the crate-private Tier 0 execution boundary without
making native pointers, heap offsets, handle encodings, allocator bins, or
collector state part of the portable artifact ABI.

The initial collector is a device-admitted, incremental stop-the-world,
non-moving tracing mark-sweep collector. Incremental means that collector work
is resumable across bounded scheduler slices. It does not mean that artifact v1
interleaves guest mutation with an active collection cycle.

This issue specifies the boundary only. Heap-backed instruction execution,
exception-table unwinding, public execution APIs, coroutines, capabilities,
and snapshots are implemented by later issues.

## Ownership and admission

An immutable `ExecutionImage` owns shared verified code, resolved type/layout
metadata, UTF-16 literal bytes, and canonical literal descriptors. Every
`VmInstance` separately owns:

- one managed heap arena;
- one managed-handle table and its free-slot state;
- instance static-field slots;
- allocator bins and bitmaps;
- collector phase, cursors, epoch, and intrusive gray queue state;
- at most one unpublished pending allocation;
- deterministic guest and maintenance accounting;
- one immortal emergency `OutOfMemoryError` identity.

The artifact manifest's `required_heap_bytes` is a lower bound on useful guest
arena capacity. The execution profile supplies the actual `heap_bytes` for the
instance. It must be at least the manifest requirement, at least 32, and a
multiple of 16. The resulting arena capacity is identical in debug and release
builds and is not reduced by Rust representation choices.

Collector and runtime metadata are outside `heap_bytes`, but are still bounded
and admitted. Admission derives and reserves, with checked arithmetic:

- `floor(heap_bytes / 32)` managed-handle entries;
- all handle free links, mark colors, generations, and intrusive gray links;
- one descriptor per artifact-wide unique UTF-16 literal;
- one fixed slot per resolved static field;
- all TLSF-class heads and bitmaps required by the arena size;
- fixed collector cursors, pending-allocation state, diagnostics, and emergency
  exception state.

Admission attempts every required native reservation before publishing a
`VmInstance`. Arithmetic overflow, a profile-limit failure, or native
reservation failure is an `AdmissionError`, not a catchable guest OOM. After
successful admission, steady-state allocation, field/array access, string
operations, and collection never grow a Rust collection or request host memory.

The runtime reports the exact arena, static, handle, literal-descriptor,
allocator, collector, and total reserved byte counts to the scheduler. This
keeps resident-memory policy explicit while preserving `heap_bytes` as a
portable guest-capacity promise.

## Reference domains and identity

`null` remains a distinct runtime value. Every non-null reference is an opaque
logical token. Its private v1 representation is an eight-byte pair:
`tagged_slot:u32 + generation:u32`. The high two slot bits select a reference
domain and the low 30 bits select its entry. Admission rejects a domain that
would require `2^30` or more entries. The domains are:

- managed arena objects;
- immortal artifact-backed literals;
- the immortal emergency OOM;
- admitted host references.

The representation and domain encoding are crate-private. Raw integers cannot
construct or inspect references. Type checks resolve the dynamic type through
the domain descriptor or managed handle rather than trusting a type carried by
guest data.

Managed references use a stable handle entry containing the current arena
location and runtime type. Collection does not change the handle. When an
object dies, its slot generation increments before the slot is reusable. A
generation that would wrap retires that slot permanently, so an old token can
never become live again. Premature handle exhaustion despite a valid admitted
storage formula is a `VmFault`.

Reference equality compares complete live logical identities. It never compares
native addresses. Default `Any.hashCode()` is a separate deterministic `u32`
identity hash. Allocation ordinals start at one and advance with wrapping `u64`
arithmetic. The hash is the low 32 bits of the SplitMix64 finalizer applied to
that ordinal: xor-shift by 30 and multiply by `0xbf58476d1ce4e5b9`, xor-shift by
27 and multiply by `0x94d049bb133111eb`, then xor-shift by 31. Collisions and a
zero result are legal. The value and the next ordinal survive snapshots; a
handle slot, generation, or heap offset does not.

Admitted host references use their own domain, ownership, generation, liveness,
and rebind rules. They may occupy typed guest slots but are not traced as arena
objects. Cross-instance references, stale generations, and mismatched dynamic
types remain rejected at the instance boundary.

## Portable heap layout

The arena and every physical block are aligned to 16 bytes. Every block charges
a 16-byte logical allocator header. The minimum physical block is 32 bytes.
Allocated payload begins immediately after the logical header.

Portable scalar widths and natural alignments are:

| Value | Width | Alignment |
|---|---:|---:|
| `bool` | 1 | 1 |
| `char` | 2 | 2 |
| `i32`, `f32` | 4 | 4 |
| `i64`, `f64`, `ref` | 8 | 8 |

An object begins with the complete superclass payload. Each class appends only
its own instance fields, grouped by decreasing alignment and then by declaration
order. This preserves a fixed superclass prefix while minimizing padding.
Resolved field IDs retain source/semantic identity; physical offsets are
derived execution metadata and are never serialized.

An array payload begins with an eight-byte logical header containing its `u32`
length and reserved zero bytes. Elements follow densely at their portable width
and alignment. Array length, element bytes, header, allocator header, and final
16-byte block alignment are all checked before mutation.

A dynamic string payload begins with an eight-byte logical header containing
its UTF-16 code-unit length and physical encoding tag. Payload is either:

- `Latin1`, one byte per code unit when every value is at most `0x00ff`; or
- `Utf16`, one little-endian `u16` per code unit otherwise.

The two forms are physically different but semantically identical. Isolated
surrogates are valid and necessarily use the UTF-16 form. Snapshot and quota
semantics are expressed in logical UTF-16 code units, not storage encoding.

All checked size formulas reject arithmetic overflow before reserving a block.
Rust structure layout, `size_of`, pointer width, and debug padding never affect
guest-visible capacity.

## Free-space organization and fragmentation

The initial allocator is a TLSF-like two-level segregated-fit allocator. For an
aligned free-block size `s`, let `f = floor(log2(s))`,
`base = 2^f`, and `width = max(16, 2^(f - 3))`, where the exponential term is
used only when `f >= 3`. Its second-level index is
`j = floor((s - base) / width)`. Thus each first-level range has at most eight
16-byte-aligned subranges; the smallest ranges have only their meaningful
prefix of indices. Free blocks map downward into `(f, j)`.

An allocation search maps its aligned request upward: an exact subrange lower
bound searches that subrange, while any larger value advances to the next
subrange, carrying into the next first level when required. It then selects the
lexicographically first non-empty `(f, j)` at or above that search key. Every
block in a selected class is therefore large enough without list scanning.
First- and second-level bitmaps find that class in bounded time; allocation
never scans the arena or an unbounded free list. This conservative mapping may
skip a fitting block in the request's partial lower subrange, which is an
explicit form of deterministic allocator fragmentation rather than an
implementation choice.

The requested logical size is rounded only to 16-byte alignment. Class mapping
selects a candidate but does not round the allocation to the class size. The
head of the smallest admissible non-empty class is removed. A remainder of at
least 32 bytes becomes a new free block; a 16-byte remainder becomes internal
slack. Free-list insertion is LIFO. Freeing checks and coalesces at most the two
physical neighbors before inserting the combined block. These rules make an
identical admitted state and operation sequence choose identical blocks.

Non-moving v1 collection cannot eliminate external fragmentation. After a full
collection and coalescing, allocation succeeds only if a sufficiently large
physical block exists. It may therefore raise OOM while `total_free` exceeds
the request when `largest_free_block` does not. This outcome is deterministic
and is reported in bounded diagnostics.

## Object, array, and static initialization

Every newly reserved payload is initialized before publication:

- integer, floating bit-pattern, and character fields/elements become zero;
- boolean fields/elements become false;
- reference fields/elements become null;
- padding and reserved bytes become zero.

This includes reference storage declared non-null. The trusted Kotlin backend
writes constructor and initializer values before normal source-level
publication. If bytecode nevertheless reads null from a non-null field, static,
or array element, the instruction raises a catchable null failure without
writing null into a non-null destination register. `lateinit` remains an
explicit compiler/stdlib lowering with its own target exception.

Static fields live in a fixed per-instance slot array outside the managed heap.
They use the same portable value forms and zero defaults. Every reference-typed
static slot is a GC root. Static state is never shared between instances that
share an `ExecutionImage`.

Artifact field writability is a bytecode property, not a complete Kotlin
constructor-state model. The trusted Kotlin backend may make backing storage
for a source `val` bytecode-writable when initialization requires ordinary
code, while guaranteeing the source-level write-once rule. Artifact v1 does
not add uninitialized-reference verifier types, constructor-only opcodes, or a
per-object initialization bitmap. Optimizers may treat a field as immutable
only after closed-world analysis proves that property.

Module, object, and top-level initialization order is compiler-lowered into
ordinary verified functions and guards. The VM has no hidden JVM class-
initialization state machine.

## Allocation boundary and atomic publication

The verifier requires every `new_object` and `new_array` to be the first
significant instruction in a dedicated basic block and permits at most one
allocation instruction in that block. The compiler splits control flow before
allocation. This creates a precise resumption boundary without a general
mid-block continuation model.

On first entry, the complete block fixed cost is checked and charged exactly
once. The allocation then:

1. reads and validates all operands;
2. rejects a negative array length with `NegativeArraySizeException`;
3. computes the exact checked logical size and initialization charge;
4. reports immediate OOM without collection if the request cannot fit in the
   entire arena;
5. attempts a bounded allocator lookup;
6. requests one collection cycle if no suitable block exists;
7. retries the identical request once after collection;
8. reserves a block and handle privately;
9. initializes the payload in bounded chunks;
10. atomically marks the handle live, writes the destination, and continues the
    remainder of the already charged block.

The fixed block cost is not charged again after GC or across initialization
slices. Initialization charges one dynamic guest unit per 16 logical bytes.
The pending state records the block, handle, requested type/length, initialized
cursor, fixed-cost-paid state, and whether GC has already been attempted. Its
capacity is reserved at admission.

Before publication, no register, field, static, literal table, or host request
can observe the object. Collection never runs concurrently with unpublished
initialization. Reboot or cancellation may discard the pending object and
return its private block/handle without guest cleanup. An initialization slice
cannot publish a prefix.

The admitted minimum guest slice must pay the allocation block's fixed cost or
at least one pending initialization unit as appropriate. A valid request can
therefore always make bounded progress.

## Collection triggering and scheduler phases

Collection is driven strictly by allocation failure, not time, tick count, or
a percentage watermark. A request larger than the arena fails immediately.
Every other failed-fit request may start exactly one complete collection and
receive exactly one post-collection retry. If retry fails, the pending
allocation is discarded and emergency OOM delivery begins. A later distinct
allocation after guest cleanup may start another cycle.

The crate-private scheduler boundary accepts separate guest and maintenance
budgets. Conceptually:

```text
run_slice(guest_budget, maintenance_budget)
```

Pending allocation initialization and ordinary bytecode consume guest budget.
If a collector is already active at call entry, the guest phase is skipped and
only maintenance may run. Otherwise, if allocation requests collection, the
guest phase stops immediately and its unused budget is discarded. The active
collector may then consume maintenance budget. Guest bytecode does not resume
in the same call even when collection finishes; it resumes in the next slice.
Budgets never carry, borrow, or create debt, and the result reports both exact
spent totals.

An inactive collector performs no scan, polling, epoch update, or heap read.
Its maintenance spend is exactly zero. The scheduler supplies maintenance work
only to a VM with an active collector and includes that spend in both the
device and aggregate server budgets. Profile limits include a non-zero minimum
maintenance quantum capable of paying one collector unit.

## Incremental mark-sweep state machine

The collector has explicit `Idle`, `Roots`, `Mark`, and `Sweep` phases. Starting
a cycle switches the global mark epoch instead of clearing every handle mark.
Because the mutator remains stopped until `Idle`, the graph is stable and v1
requires no read or write barrier.

Roots are enumerated in this deterministic order:

1. per-instance statics by resolved field ID;
2. coroutines by scheduler ID;
3. frames from oldest to newest within each coroutine;
4. statically reference-capable registers by ascending register index;
5. pending managed exception and future VM-owned host/coroutine reference slots
   in their specified slot order.

The latter categories reserve the root-enumeration boundary for later runtime
slices without making their implementation part of this issue. Uninitialized
or null slots enqueue nothing but still consume their root unit. Artifact
literals and emergency OOM are immortal reference domains and need no arena
mark.

Marking uses a FIFO intrusive gray queue stored in handle entries. A handle is
enqueued at most once in the current epoch. Objects scan reference fields by
resolved field ID. Reference arrays scan elements by ascending index. Dynamic
strings and primitive arrays are leaf objects. A scan cursor in collector state
allows an arbitrarily large reference array or object layout to stop after any
unit.

After the gray queue empties, sweep visits physical blocks in increasing arena
offset. A live block clears transient gray state for reuse. An unreachable
block invalidates its handle, increments or retires its generation, and
coalesces with free neighbors. The sweep cursor is saved after every unit. On
completion the allocator retries the original request and the collector returns
to `Idle`.

One maintenance unit is exactly one bounded action:

- inspect one root-capable slot;
- inspect one reference field or reference-array element;
- classify one reference-free object;
- inspect one physical block during sweep, including at most two neighbor
  coalescing checks;
- perform one fixed free-list/phase-transition action.

Each action costs one maintenance unit and is charged before mutation. Large
primitive payloads are not scanned and freed payload bytes are not cleared.
Reuse initialization prevents data disclosure before later publication.

## Heap instruction semantics

Every instruction reads all operands and completes every failing check before
publishing a destination or mutation.

- `array_length` returns the stored length as a non-negative `i32`; construction
  has already proved that its internal `u32` value is at most `i32::MAX`.
- `array_load` and `array_store` reject `index < 0 || index >= length` before
  accessing payload and leave all state unchanged on failure.
- Fields use resolved portable offsets; receiver dynamic type must remain
  assignable to the verified owner.
- `static_get` and `static_set` use the instance slot array.
- `is_type(null, T)` is false. A non-null result uses the precomputed dynamic
  supertype/interface closure.
- `checked_cast` preserves null only for a nullable destination, raises a null
  failure for a non-null destination, and raises `ClassCastException` for an
  incompatible non-null value.

Artifact arrays are invariant and carry an exact reified element type. The
verifier proves store compatibility with that exact type, so v1 needs no
JVM-style covariant `ArrayStoreException`. A null found in zero-initialized
non-null reference storage raises the target null exception before destination
publication.

A dead, stale, foreign, or incorrectly typed handle that cannot be produced by
verified guest execution is a `VmFault`, not a guest exception. Ordinary null,
bounds, negative-length, cast, and allocation failures are catchable target
exceptions when the exception-unwinding layer is present.

## String objects and operations

Every module retains verified UTF-16 literal records in immutable artifact
bytes. During admission, identical raw UTF-16 payloads across all modules are
deduplicated into artifact-wide canonical literal descriptors. Each descriptor
has one per-instance immortal String identity whose payload remains a byte range
in the shared `ExecutionImage`.

Loading a string constant returns that identity without copying, guest-heap
allocation, collection, or lazy native allocation. Equal literal payloads in
the same artifact are reference-identical. Literal references remain valid for
the instance lifetime and snapshots encode their canonical literal identity
directly.

Dynamic strings are ordinary immutable managed objects in compact Latin-1 or
UTF-16 form. Equal dynamic content does not imply reference identity and is not
automatically interned. The initial intrinsic semantic surface is:

- `length`: UTF-16 code-unit count;
- indexed `get`: one `Char` code unit with ordinary bounds failure;
- content equality: exact `u16` sequence equality;
- `compareTo`: unsigned-`u16` lexicographic order;
- `hashCode`: `h = 31 * h + code_unit` with wrapping `i32` arithmetic;
- concatenation and substring: checked, atomic dynamic results.

A full-range substring returns its receiver and an empty substring returns the
canonical empty literal. Every other substring is fresh. Runtime concatenation
creates a fresh result; compiler constant folding may instead emit an existing
literal before artifact creation.

String processing charges one dynamic guest unit per eight logical UTF-16 code
units, independent of compact storage. Allocation initialization additionally
charges its ordinary 16-byte units. Equality and comparison charge the code
units actually inspected in their deterministic left-to-right traversal;
length mismatch may return at fixed cost. Hashing charges the full length even
when a derived native cache makes execution faster, so cache state and snapshot
restore cannot alter quota-visible behavior.

Large concat, substring, comparison, hashing, and host conversion operations
use pre-reserved resumable cursors. Mutating results remain unpublished until
complete. Optimized interpreter, JIT, or vectorized implementations may process
more host bytes internally only while preserving the exact semantic charge and
slice boundary.

Host UTF-8 conversion combines valid surrogate pairs into their Unicode scalar
and replaces each isolated surrogate with `U+fffd` by default. A strict variant
returns a structured conversion failure without partial publication. Invalid
incoming UTF-8 similarly fails atomically unless the capability contract
explicitly selects replacement decoding. APIs requiring lossless arbitrary
data use byte arrays rather than text.

## Emergency OOM and diagnostics

Admission creates one immortal per-instance emergency `OutOfMemoryError`
identity outside the guest arena. It has no writable stack trace, cause,
suppressed list, or allocating message. Every allocation-exhaustion delivery
may reuse the same identity, including nested or later failures after guest code
retains a previous OOM.

This is the deterministic minimum-memory analogue of a VM preallocated OOM
fallback. It guarantees catchable delivery without recursively allocating an
exception. OOM reference identity reuse is therefore explicitly observable.

A separate fixed runtime diagnostic record contains scalar fields for request
kind, requested logical bytes, live bytes, total free bytes, largest free block,
and source location. Updating it allocates nothing. Exception unwinding and
source-mapped presentation may copy or format that record only outside the
exhausted guest heap under their own bounded contracts.

If the pre-reserved OOM identity or mandatory metadata is corrupt or missing,
the instance faults with `VmFault::InvalidStoragePlan`; legal steady-state heap
exhaustion never becomes a native allocation failure or fatal process abort.

## Weak references, finalization, and resurrection

Artifact v1 has no weak, soft, or phantom references, reference queues,
finalizers, or GC-triggered guest callbacks. An unreachable object is reclaimed
without running guest code and cannot resurrect. Kotlin/JVM-specific APIs for
these facilities are absent from the target standard library. Explicit
`close`, `use`, and structured cleanup remain normal compiler/library behavior.

## Snapshot-facing logical boundary

A snapshot barrier is valid only after active collection and unpublished
allocation work finish at bounded safepoints. Snapshot records contain:

- statics and other logical roots;
- object runtime type, logical fields, arrays, and UTF-16 string content;
- canonical graph IDs preserving cycles and sharing;
- literal and emergency identities as dedicated symbolic references;
- per-object identity hashes and the next allocation sequence;
- quota-visible pending state owned by later scheduler/exception issues.

They never contain heap offsets, block headers, free-list order, mark epochs,
gray links, handle slots, generations, Rust layout, or native pointers.

Omitting physical fragmentation would otherwise let restored execution allocate
successfully where uninterrupted execution produced OOM. Therefore snapshot is
defined as a canonicalizing operation: the continuing instance, when there is
one, rebuilds its heap from the same canonical logical snapshot representation
used by restore. Both sides receive the same densely reconstructed block order,
allocator state, handles, and free tail. The rare rebuild is bounded and quota-
charged snapshot maintenance; ordinary GC remains non-moving.

The snapshot implementation may reuse its already bounded serialized snapshot
buffer as the reconstruction source. Failure before atomic snapshot publication
leaves the original instance valid at the barrier. Exact canonical schema and
I/O publication remain owned by the snapshot issue.

## Future parallelism and collector replacement

Artifact v1 has one execution lane. An active collector excludes all guest
coroutines in the instance, although other VM instances may run on other host
workers. No accidental host-thread memory model is exposed.

Future intra-VM parallel execution must introduce an explicit versioned memory
model, synchronization operations, root handshake, and collector barriers. A
future moving, compacting, or generational collector may reuse stable handles
without changing artifact reference semantics, logical heap charges, string
semantics, snapshots, or quota totals.

## Deterministic cost summary

Portable dynamic work uses these units in addition to artifact-fixed block
costs:

| Work | Charge |
|---|---:|
| allocation initialization | `ceil(logical_bytes / 16)` guest units |
| string processing | `ceil(inspected_code_units / 8)` guest units |
| root inspection | 1 maintenance unit |
| reference edge inspection | 1 maintenance unit |
| reference-free object classification | 1 maintenance unit |
| physical block sweep/coalesce | 1 maintenance unit |
| fixed collector transition/free-list action | 1 maintenance unit |

Every multiplication, addition, alignment, index, cursor, and cost accumulation
is checked before mutation. Cost depends on verified logical data, never wall
clock, native allocation behavior, string packing, pointer width, compiler
optimization, or build mode.

## Conformance requirements

The implementation issue must add golden vectors covering:

- stable handle identity, generation reuse, stale/foreign rejection, and slot
  retirement behavior;
- exact object, inherited-field, primitive/reference-array, compact-string, and
  block-size charges at alignment boundaries;
- zero initialization, null/non-null reads, negative lengths, bounds failures,
  casts, and atomic destination/store behavior;
- cycles, diamonds, duplicate roots, shared children, null edges, and unreachable
  islands under incremental root/mark/sweep slicing;
- one-unit maintenance slices, phase/cursor persistence, zero idle work, exact
  guest/maintenance spend, and debug/release-identical traces;
- full-heap success after collection, fragmentation OOM with sufficient total
  free space, oversized immediate OOM, catch/release/recollect recovery, and
  repeated singleton OOM delivery;
- unpublished multi-slice array/string initialization, cancellation cleanup, and
  absence of native allocation after admission;
- artifact-backed empty, Latin-1, BMP, surrogate-pair, and isolated-surrogate
  literals; cross-module deduplication; dynamic compact strings; equality,
  comparison, hash, concat, substring, and strict/replacement UTF-8 conversion;
- canonical snapshot graph IDs and a model-level comparison showing that
  canonical rebuild removes pre-snapshot fragmentation divergence.

Release workloads report, without hardware-specific CI thresholds:

- object and array allocation throughput by size class;
- allocation initialization units per second;
- root, edge, leaf, and sweep units per second;
- collection-cycle and per-slice work distributions;
- retained logical bytes, physical arena bytes, allocator slack, fragmentation,
  metadata reservation, and total resident bytes;
- literal-load, Latin-1/UTF-16 string, field, array, and type-check throughput;
- exact zero collector work for large populations of idle instances.

Semantic results, accounting totals, trace digests, allocation bounds, and zero
steady-state native allocation are hard tests. Performance documents record the
machine, profile, workload, and release results so later Tier 1/JIT work has a
reproducible baseline.

## Follow-up boundary

The immediate implementation issue consumes this specification to add the
crate-private admitted arena, handles, statics, allocator, collector state
machine, heap-backed opcodes, artifact-backed literals, dynamic strings, OOM
state, conformance vectors, and release baselines. It must preserve the current
private execution boundary and create no public VM/JNI API.

Exception-table unwinding may initially observe structured heap-operation traps
until its own issue maps them to fully managed target exceptions. Coroutine
roots, capability-held references, canonical snapshot bytes, compiler lowering,
and Minecraft integration remain separate follow-ups, but must use the ownership
and root-enumeration slots reserved here.
