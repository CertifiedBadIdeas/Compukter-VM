# Deterministic Tier 0 Execution Semantics

> Issue: [#38](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/38)

## Status

Accepted design for the portable Compukter artifact v1 execution semantics and
the crate-private Tier 0 reference interpreter boundary.

## Purpose

Tier 0 is the semantic oracle for Compukter bytecode. It defines observable
results, failures, suspension, and quota totals independently of Rust build
mode, host CPU, native pointer layout, allocator behavior, wall-clock time, and
future optimization tiers.

The first implementation slice executes only scalar values, direct calls, and
control flow. That partial interpreter remains crate-private. A public VM API
must not accept a fully verified artifact while any mandatory artifact v1
instruction family can still fail merely because it is unimplemented.

## Goals

- Define admission, instance ownership, entry, frame/register state, slicing,
  terminal outcomes, and failure boundaries.
- Give every scalar and control-flow v1 opcode exact Kotlin-target semantics.
- Make fixed and dynamic quota accounting deterministic at basic-block
  boundaries.
- Bound admission and steady-state execution for both small controllers and
  full computers.
- Provide conformance fixtures and performance workloads shared by Tier 0 and
  every future optimized interpreter, JIT, or AOT tier.

## Non-goals

- Implement heap objects, arrays, GC, exception unwinding, coroutines,
  capabilities, snapshots, JNI, Kotlin IR lowering, JIT, or AOT in #38.
- Change artifact v1 bytes, opcode identities, or fixed costs. The scalar
  implementation may close the verifier omission for the already specified
  `i32`/`char` `convert` pairs without changing the instruction encoding.
- Expose the native representation or width of runtime values and references
  as a portable ABI.
- Establish a hardware-dependent absolute throughput threshold.

## Architecture and ownership

`VerifiedArtifact` remains immutable and shareable. Crate-private admission
derives an immutable `ExecutionImage` containing resolved module, function,
block, signature, and frame-layout metadata needed by execution. It does not
repeat semantic verification during dispatch.

Each `VmInstance` owns all mutable state:

- lifecycle and last terminal outcome;
- the root coroutine and, in later slices, structured child coroutines;
- frame/register arenas and current function/block positions;
- per-instance static fields and managed heap;
- deterministic quota counters and suspension state;
- admitted capability bindings and host-request state.

An `ExecutionImage` may be shared by many instances. Mutable values, static
fields, references, frames, quotas, and suspension state may never be shared
implicitly between instances.

## Admission

An `ExecutionProfile` supplies host limits and identities for:

- managed heap capacity;
- frame/register storage capacity;
- maximum call depth and coroutine count;
- host-request and event capacities;
- maximum accepted slice budget;
- compiler, standard-library, and capability ABI identities.

Admission performs checked comparisons against the verified manifest, resolves
the entry signature, creates deterministic execution metadata, computes the
maximum scalar/control storage plan, and reserves all mandatory arenas. It
publishes no mutable instance until every check and allocation succeeds.

Frame storage planning uses portable logical charges rather than `size_of` of
a Rust enum:

- one register slot charges 16 logical bytes;
- one frame charges 32 logical bytes plus its register slots, rounded up to 16;
- the conservative root-coroutine reservation is `maximum_call_depth` times
  the largest executable frame charge;
- all multiplication, addition, and alignment use checked arithmetic.

The declared `required_stack_bytes` must cover that conservative reservation,
and both values must fit the profile. Implementations may use a more compact
native layout, but admission and quota-visible requirements use the portable
calculation above. Heap and coroutine slices may later add their own portable
storage formulas without changing scalar/control behavior.

Admission failures are bounded structured `AdmissionError` values. They are
host-facing failures, not catchable guest exceptions and not `VmFault` values.
They include incompatible ABI identities, unavailable required capabilities,
inconsistent manifest requirements, profile limit failures, arithmetic
overflow, and host allocation failure.

Admission and execution depend on no Minecraft, JNI, Kotlin compiler internal,
or file outside the VM repository.

## Runtime values

The private runtime value model contains:

- `i32` and `i64` two's-complement integers;
- `f32` and `f64` IEEE 754 bit patterns;
- canonical `bool`;
- Unicode-scalar `char`;
- `null`;
- opaque non-null managed or admitted-host references.

The verifier-provided register type determines which variant is legal in each
slot. Rust enum layout, pointer width, NaN payload propagation, and internal
reference handles are not portable ABI. Raw integers can never create or
inspect a reference identity.

For conformance traces, primitive values use canonical little-endian bits and
references use harness-assigned symbolic identities. The symbolic identity is
test metadata, not an object-address or serialized VM promise.

## Entry

Starting an instance receives a typed argument list. Before creating the root
frame, the VM validates the complete list against the verified entry signature:

- exact arity;
- exact primitive kind;
- reference assignability and nullability;
- ownership and liveness of every opaque reference handle.

Failure changes no mutable VM state. Kotlin script compilation normally emits
a zero-argument entry wrapper, but artifact v1 does not require the entry
function itself to have zero parameters.

Successful start creates the root frame, copies parameters into registers zero
through `parameter_count - 1`, leaves all other registers uninitialized, and
selects the function's first block. The instance becomes runnable.

An instance is one-shot. A terminal instance cannot be restarted; another run
creates a new `VmInstance` over the same `ExecutionImage`.

## Frames and calls

A frame contains the current module, function, block, caller continuation,
optional caller destination, and the function's fixed register slots. It does
not own a growable register collection.

`call_direct` performs these steps atomically after its containing block has
already been charged:

1. Resolve the verified target and read all argument values.
2. If the declared maximum call depth is reached, produce
   `GuestTrap::StackOverflow` before changing the frame stack.
3. Reserve the already preallocated callee frame slot.
4. Copy arguments, leave non-parameter registers uninitialized, and transfer to
   the callee entry block.

If the admitted frame arena is exhausted earlier than the declared depth, the
runtime produces `VmFault::InvalidStoragePlan`; this indicates an internal
admission/runtime invariant failure.

`return` reads its optional value before destroying the callee. A normal return
then removes the callee and initializes the caller destination, if present. A
return from the root entry function produces `Halted(value?)`.

## Slice execution

`run_slice(budget)` is valid only for a runnable instance. The supplied budget
must be positive, at least the admitted artifact `minimum_slice_cost`, and no
larger than the profile maximum. An invalid host request returns a bounded
host-facing `RunError` without changing VM state.

For every basic block, Tier 0 performs:

1. Read the block's verifier-recomputed fixed cost.
2. If the cost exceeds the remaining slice budget, do not enter the block.
   Discard the unused remainder and return `SliceExhausted` with the instance
   still runnable at that block.
3. Otherwise subtract the complete fixed cost atomically.
4. Execute instructions in order until the block terminator, a `GuestTrap`, or
   a `VmFault`.

There are no fixed-cost quota checks between instructions. A trap or fault does
not refund the unexecuted part of the already charged block. Destination
registers and externally visible effects are published only after a trapping
instruction succeeds.

Unused slice credit never carries to a later call, and debt is never created.
Every `run_slice` receives a fresh explicit budget.

### Infinite loops

Verifier-approved backedges target loop-header safepoints. For a one-block
`while (true)` loop with block cost `K` and slice budget `B`, a slice executes
exactly `floor(B / K)` iterations. It then returns `SliceExhausted` at the loop
header and discards `B mod K`. The guest observes neither the slice boundary nor
the host scheduling delay.

## Outcomes and lifecycle

Execution has distinct outcomes:

- `SliceExhausted`: non-terminal, runnable, stopped before entering the next
  block because its fixed cost did not fit;
- `Suspended(reason)`: non-terminal but not runnable until its explicit resume
  condition is satisfied;
- `Halted(value?)`: terminal normal return from the root entry function;
- `Crashed(trap)`: terminal uncaught guest-language failure;
- `Faulted(fault)`: terminal non-catchable VM/runtime invariant failure.

Suspension reasons are semantically distinct: cooperative yield, virtual-time
sleep, asynchronous capability completion, and structured coroutine join.
They are specified for the complete v1 runtime surface but are not implemented
by the initial scalar/control slice.

Calling execution again on a terminal instance returns its same immutable
terminal outcome and performs no dispatch. Resuming an instance with the wrong
reason, token, or lifecycle state returns a host-facing `RunError` without guest
state mutation.

## Guest traps and VM faults

Expected language/runtime failures use an internal stable `GuestTrap` kind with
bounded scalar payload. Required kinds include:

- integer division by zero;
- stack overflow;
- null dereference;
- array bounds or negative array length;
- failed checked cast;
- allocation limit;
- invalid sleep duration;
- capability-declared guest failure;
- explicit throw.

A later exception-unwinding layer maps each semantic kind to the target Kotlin
standard-library exception identity and materializes the managed exception
object. Until that layer exists, private scalar/control tests inspect the trap
directly. An uncaught trap produces `Crashed`.

`VmFault` is reserved for impossible verified states or broken host/runtime
contracts, including invalid resolved IDs, initialized-value/type mismatch,
accounting overflow after successful admission, premature arena exhaustion,
corrupt lifecycle state, and a host capability violating its admitted ABI.
Guest code cannot catch a `VmFault`.

## Scalar instruction semantics

`nop`, `move`, `const`, and `null` behave exactly as their verified schemas
state. An instruction reads all sources before writing its destination, so
source/destination aliasing is well-defined.

### Integer operations

For `i32` and `i64`:

- add, subtract, multiply, and negate wrap modulo 2^32 or 2^64;
- bitwise operations operate on those exact-width bit patterns;
- left, signed-right, and unsigned-right shifts mask the `i32` count with 31
  for `i32` values and with 63 for `i64` values;
- division and remainder truncate toward zero;
- a zero divisor produces `GuestTrap::DivisionByZero`;
- `MIN_VALUE / -1` returns `MIN_VALUE` and its remainder is zero.

No operation uses Rust debug overflow behavior.

### Floating-point operations

`f32` and `f64` use IEEE 754 round-to-nearest, ties-to-even. Separate artifact
operations may not be fused into an FMA unless the result is bit-identical.
Signed zero and infinities are preserved. Division by floating zero follows
IEEE 754 and does not produce an integer-division trap.

Floating remainder follows Kotlin/JVM remainder, using a quotient truncated
toward zero. NaN operands, an infinite dividend, or a zero divisor produce NaN;
a finite dividend modulo infinity returns the dividend.

Every NaN produced by arithmetic or conversion is normalized to one canonical
quiet NaN bit pattern per width. Artifact constants retain their verified raw
bits when loaded, but the first operation producing a NaN canonicalizes it.

### Conversions

- `i32` to `i64` sign-extends; narrowing integer conversion keeps the low bits.
- Integer-to-float and `f64`-to-`f32` use IEEE round-to-nearest, ties-to-even.
- Float-to-integer truncates toward zero, maps NaN to zero, and saturates values
  outside the destination range.
- Same-width conversions are ordinary value copies after verifier type checks.
- Character conversion accepts only verified Unicode scalar values; an integer
  outside that set produces a guest conversion trap rather than a Rust `char`
  construction failure.

### Comparisons

Integer and character comparisons use mathematical signed integer or Unicode
scalar order. Boolean values support equality only.

Primitive floating equality follows Kotlin/JVM primitive behavior: NaN is not
equal to itself and negative zero equals positive zero. Ordered comparison with
NaN is false. `not_equal` is the logical negation of primitive equality.

Reference equality compares opaque identity. Two null values are equal; null
and non-null differ. Reference ordering does not exist.

## Control-flow semantics

- `jump` selects its verified target.
- `branch` selects exactly one target from its canonical boolean condition.
- `switch_i32` performs mathematical equality against sorted unique cases and
  otherwise selects the default target. Search strategy is unobservable.
- `return` transfers as defined in the frame section.
- `unreachable`, if executed, produces `VmFault::ReachedUnreachable`; it is not
  a guest exception.

The initial implementation includes `call_direct` but not virtual/interface
dispatch, heap/static access, throwing/unwinding, coroutines, or capabilities.
Those later families must reuse this document's charging, publication, trap,
and outcome rules.

## Dynamic charging

Fixed block cost is always charged first. Allocation and capability work then
uses deterministic dynamic charges from the admitted ABI.

Dynamic work must either:

- compute and precharge its complete checked cost before mutation; or
- execute as explicit bounded resumable chunks whose progress is VM state.

Insufficient dynamic budget publishes no partial object, field write, host
request, or result. A resumable chunk suspends with a distinct continuation;
it does not masquerade as fixed-cost `SliceExhausted` at a block boundary.

## Conformance contract

Committed fixtures cover:

### Numeric vectors

- every wrapping boundary for add/subtract/multiply/negate;
- zero division and remainder and `MIN_VALUE / -1`;
- every shift-mask boundary for both integer widths;
- signed zero, infinities, canonical NaNs, and float remainder cases;
- conversion truncation, saturation, NaN-to-zero, and rounding boundaries;
- integer, float, boolean, character, and reference comparisons.

### Control and lifecycle vectors

- straight-line execution and source/destination register aliasing;
- both branch targets and sparse/dense switch tables;
- unit and value returns, nested calls, and stack overflow;
- entry argument arity, kind, nullability, and ownership rejection;
- exact-fit and insufficient-first-block budgets;
- discarded remainder and an empty infinite loop across many slices;
- a trap after block charging;
- halt, slice exhaustion, guest crash, and injected internal fault behavior.

Every vector asserts result values, consumed fixed and dynamic cost, current
block/frame/register state, outcome, and a deterministic block-boundary trace
digest. Future execution tiers must produce the same trace digest and observable
state for every vector.

## Performance contract

Admission may allocate bounded state. After start, the scalar/control
steady-state path performs no heap allocation, capacity growth, or lazy metadata
construction.

Release-mode workloads cover:

- a hot integer arithmetic loop;
- a mixed branch/switch loop;
- nested direct calls;
- an empty quota-limited infinite loop.

The baseline records blocks per second and bytecode instructions per second,
along with artifact hash, workload parameters, compiler version, target triple,
and CPU description. Initial CI treats semantic, accounting, and allocation
regressions as hard failures but does not enforce an absolute hardware-specific
throughput minimum. Later optimization issues compare against the committed
methodology and recorded baseline.

## Implementation decomposition

The immediate follow-up issue implements only the crate-private scalar/control
oracle:

- admission and preallocated root frame/register arena;
- private runtime values and typed entry arguments;
- scalar constants, moves, conversions, arithmetic, comparisons, and reference
  equality for null/fixture handles;
- direct calls, returns, jumps, branches, switches, unreachable, block charging,
  outcomes, traps, trace fixtures, and performance workloads.

Heap/static operations, virtual/interface dispatch, exception unwinding,
coroutines, capabilities, and public VM publication remain separate follow-up
slices. No partial slice may weaken artifact verification or expose an
`Unimplemented` result for a publicly accepted verified artifact.
