# Host-neutral execution sessions

`Session` is the public execution boundary of Compukter VM. It owns one
admitted program instance and contains no Minecraft, JNI, terminal, filesystem,
thread, or process policy. A host adapter verifies bytes, admits the resulting
`VerifiedArtifact`, starts the entry point, and repeatedly drives bounded
slices.

## Admission

`Session::admit` accepts only a `VerifiedArtifact`, an `ExecutionProfile`, and
host-owned `CapabilityBinding` schemas. Admission resolves capability identity
by namespace, name, ABI major, and a host ABI minor at least as new as the
artifact minimum. Required capabilities must resolve exactly once. Invoked
optional capabilities must also resolve; synchronous capability calls are not
part of the v1 host boundary.

Admission reserves the mutable machine arenas, entry slots, host argument
slots, and inbound/outbound UTF-16 buffers. Profile limits bound heap and frame
storage, slice size, request storage, argument count, and both directions of
string exchange. Allocation failure during admission is distinct from managed
guest-heap exhaustion during execution.

## Lifecycle

The host calls `start` once and then calls `advance(guest_budget,
maintenance_budget)`. An advance returns one of:

- `SliceExhausted`, allowing cooperative scheduling;
- one borrowed `HostRequest`, which suspends guest execution;
- stable `Halted`, `Crashed`, `Faulted`, `HostFailed`, or `QuotaExhausted`;
- stable managed `AllocationExhausted` with bounded diagnostics.
- stable `UncaughtException` for an unhandled root-task Guest exception.

Only one request can be outstanding. Calling `advance` while it is outstanding
returns the same request ID and values without consuming a budget, changing
accounting, or changing the trace. The host performs any asynchronous work
outside the VM and later calls `resume(id, response)`. Thus the Rust API calls
are synchronous state transitions even when the capability operation itself is
asynchronous.

`resume` validates the pending ID, success type, string bound, and host-failure
detail before any response is accepted. Wrong or stale IDs, wrong types,
oversized strings, and empty or oversized failure details are correctable
`ResumeError` values: the original request and accounting stay unchanged. A
valid response is accepted exactly once. Explicit host failures carry one of
the shared failure kinds plus a non-empty human-readable UTF-8 detail of at
most 256 bytes. EOF/I/O failures raise managed IOException; unavailable/other
failures raise IllegalStateException. Cancellation remains a stable `HostFailed`
outcome. Ordinary failure details are copied into admission-reserved per-task
storage, so accepting a failure allocates nothing. Materialization waits until
the failed task's frames are restored and raises at the original host-call PC;
multiple responses before the next advance cannot overwrite one another.
The call was already retired at publication and is not retired again by the factory.

## Terminal failure stacks

The machine captures at most 32 active frames innermost first before terminal
failure teardown. Snapshot storage is reserved in fixed machine state and reads
only host-owned frame and execution-image metadata, not Guest heap contents.
Each frame retains its module-local function and execution-image instruction
position; active callers remain at their call instruction. Excess depth is
recorded as an explicit omitted count. Other tasks are not fabricated as callers.

`ComputerMachine::failure_stacktrace` formats this snapshot using immutable
verified artifact metadata. The formatted trace is byte-bounded, sanitizes
control characters and reports omitted frames. Optional nonsemantic module
section `0x8001` supplements existing DEBUG records with one-based line/column:
indexed records are three little-endian u32 fields (DEBUG index, line, column),
with strictly increasing valid indices and positive coordinates. Older artifacts
retain UTF-16-offset or bytecode fallback. Neither section changes semantic hashes.

C ABI 17 appends a length-prefixed UTF-8 trace after the scalar payload of
outcome tags 2, 5 and 6. Both native transports use that same wire contract.
VM fault traces describe the detection site, not necessarily the corruption cause.

C ABI 18 adds outcome tag 12: a little-endian u32 UTF-8 byte length followed by the bounded uncaught-exception
diagnostic (class, message, at most four causes and the source stack). No managed reference leaves the VM.
Both transports require ABI 19. Explicit Throw/handler artifacts require Runtime ABI 1.8 and one verified
Throwable root; older exception artifacts are rejected with a rebuild requirement, never converted to traps.
Admission resolves handlers in innermost/source order. Every handler inspection and unwound frame costs one
dynamic Guest unit. Exception references remain rooted during suspended unwinding and after child-task failure.
An unjoined failed child does not crash the process; every join throws its original exception at the join site.
Root-task failure terminates the process. OOM, quotas, VM faults and forced shutdown remain noncatchable.

`Session::uncaught_exception_diagnostic` reads bounded class/message/cause text
without advancing execution or exporting a managed reference. It requires the
admitted artifact's content hash and returns `DiagnosticError::ArtifactMismatch`
for foreign metadata; it returns an empty string when no exception is pending.

Runtime ABI 1.9 additionally encodes factory type roles in class flags bits 3..7 (0 ordinary;
1 arithmetic, 2 bounds, 3 negative size, 4 null pointer, 5 cast, 6 argument, 7 state, 8 I/O).
Unknown tags are rejected. Roles are unique across the artifact and identify non-abstract, non-generic
zero-state Throwable subclasses; their intermediate ancestors cannot add fields, methods, interfaces
or initializers. Only the verified Throwable root supplies the message/cause payload. This metadata
extension does not change C ABI 18. Fallible integer arithmetic, arrays, string ranges, reference access/casts
channels and host capability calls require their factory roles and ABI 1.9, including without a handler; legacy artifacts containing
these operations require rebuilding. Their bounded message and ordinary managed
exception are built through budgeted allocation and collection; pending references are GC roots.
Factory OOM stays noncatchable. Floating division is nonthrowing. Stack overflow and channel-storage exhaustion
remain noncatchable resource failures.

Runtime ABI 1.10 adds reference-default `string_value_of` form 7. It accepts a reference
(including null) and produces a non-null standard String. Null becomes `null`; a live reference
becomes its qualified runtime type name, `@`, and the lowercase hexadecimal VM Ref32 identity.
This identity is stable while the object is live and contains no host pointer. The instruction
provides the concrete root `Any.toString` implementation; virtual overrides retain ordinary
method dispatch, including inherited methods on arrays. ABI 1.10 allows methods on the
stateless root parent of arrays and Throwable; older artifacts retain their no-method restriction. String and scalar-box overrides are
compiler-owned library methods rather than special cases of this default instruction.

Admission deduplicates verified type names by module/string identity into an immutable shared
UTF-16 pool, bounded by twice the source metadata bytes plus per-type indices and pool records.
Conversion snapshots the name and identity before any collection, then uses the existing
sliceable string scan/allocation/copy machinery. All Guest work and allocation remain charged;
no partial destination is published. Older forms remain compatible, while form 7 requires 1.10.
The artifact container format and C ABI 18 are unchanged.

## Strings and ownership

The boundary carries borrowed UTF-16 code units because Kotlin `Char` and
`String` use UTF-16 semantics. Outbound values are copied into session-owned
storage before publication; the request view borrows that immutable storage.
Inbound values are validated, then copied into another session-owned arena
before `resume` returns. Surrogate pairs and isolated surrogate code units are
preserved exactly.

Inbound strings are subsequently materialized into the managed heap using its
compact Latin-1/UTF-16 representation. Allocation, copying, and a possible GC
retry remain sliceable and deterministically charged. UTF-8 conversion belongs
to terminal/JNI adapters, which must choose and test their own malformed-input
policy.

## Quotas, accounting, and trace

Request and response accounting is observational, not a lifetime throughput
limit. `maximum_host_requests` remains a structural admission/storage bound.
An adapter that needs throughput control supplies scheduling backpressure at
its external boundary; accepting a valid response is never rejected because a
cumulative response count was reached. Outbound code-unit exhaustion remains
terminal before publication, while an oversized inbound value remains a
correctable `ResumeError`.

`Session::accounting()` returns fixed guest units, dynamic guest units,
maintenance units, entered blocks, executed instructions, published requests,
accepted responses, and the current SHA-256 trace digest. The digest is one
chronological event stream shared by guest block entries and host exchanges.
Every trace field is framed by a little-endian `u32` byte length. Host request
events use tag 2 followed by request ID (`u64`), capability index (`u32`),
operation (`u32`), argument count (`u32`), and typed values. Host response
events use tag 3 followed by request ID, success/failure tag, and a typed value
or bounded failure kind and UTF-8 detail. Scalar payloads are little-endian. A
string uses type tag 7, its `u32` code-unit count, then its `u16` units as framed
fields.

`ComputerMachine` uses an internal session admission mode that retains the
same execution and cost counters but does not compute this digest. The computer
and FFM APIs do not expose session accounting, and hashing every block plus all
active registers would otherwise charge production execution for an
unobservable diagnostic. Direct `Session::admit` callers continue to receive
the complete trace described above.

Legal request/resume operation performs no native allocation after admission.
Managed string objects still consume the explicitly reserved guest heap and
may cause budgeted GC maintenance.

## Adapter responsibilities

A terminal, JNI, Minecraft, test, or future addon adapter owns capability
implementations, asynchronous dispatch, cancellation policy, UTF conversion,
and mapping host errors to bounded human-readable `HostFailure` values. It must
not retain a borrowed request view across a mutable session call. It should copy
or consume the request immediately, perform external work without holding the
session borrow, and resume later with the exact request ID.

The VM intentionally does not spawn threads, perform I/O, interpret wall-clock
time, or decide how multiple computers run in parallel. A host scheduler can
drive many individually single-task sessions concurrently while preserving the
same per-session semantics.

## Value hashing

Runtime ABI 1.11 adds `value_hash` (0x69), a fixed-cost, allocation-free operation producing I32. Forms 1/2/3/5/6 accept I32/I64/F32/Bool/Char; form 7 accepts nullable references. Integers use Int identity or Long folded high/low bits; Float canonicalizes every NaN to 0x7fc00000 and preserves signed zero, Boolean uses 1231/1237, and Char uses its UTF-16 code unit. Reference null hashes to zero and live references use opaque VM identity bits, never host pointers. Hash collisions are permitted. The non-moving collector preserves live identity; future relocation must preserve observable hashes rather than recomputing them from a moved address. String content hashing retains its existing sliced, rooted `string_hash` path. Artifact format and C ABI 18 are unchanged.

Runtime ABI 1.12 adds F64 form 4 to `string_value_of` (0x68) and `value_hash` (0x69).
Admission requires ABI 1.12 and F64 input. Text uses bounded shortest-round-trip decimal
conversion, Kotlin notation, and a fixed 24-unit inline buffer; string allocation remains
sliced and quota-accounted. Hashing canonicalizes NaN to 0x7ff8000000000000 and folds high
xor low into I32, preserving the sign of zero. Hash collisions are permitted and must not
be used alone as value equality. Existing numeric F64 instructions remain ABI 1.0.
Artifact format and C ABI 18 are unchanged.

## Encoded host responses (C ABI 19)

`compukter_resume_value` accepts a caller-owned byte payload, validates it completely before request lookup, and retains
no pointer after return. Version byte 1 precedes a scalar HostValueType tag (0..7). Numeric values use little-endian
widths matching their register kind, including exact F32/F64 bits. Bool is 0 or 1; Char is a u16 UTF-16 code unit;
String has a u16 count followed by that many u16 code units (including isolated surrogates). Unit has no payload.
The payload limit is 65536 bytes, and strings are limited to 4096 code units. Wrong versions, tags, widths, Boolean
values, lengths and trailing data return InvalidArgument without consuming a pending response. HostValueInput still
checks the admitted operation result type. Existing resume exports are retained; this addition enables all existing
scalar result kinds through the JVM adapters without changing Runtime ABI 1.12 or artifact encoding.
