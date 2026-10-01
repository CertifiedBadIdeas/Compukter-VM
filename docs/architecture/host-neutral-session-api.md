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
most 256 bytes and become stable `HostFailed` outcomes rather than guest traps
or VM faults. The detail uses a fixed session-resident buffer reserved during
admission, so accepting a failure allocates nothing.

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
Both transports require ABI 18. Explicit Throw/handler artifacts require Runtime ABI 1.8 and one verified
Throwable root; older exception artifacts are rejected with a rebuild requirement, never converted to traps.
Admission resolves handlers in innermost/source order. Every handler inspection and unwound frame costs one
dynamic Guest unit. Exception references remain rooted during suspended unwinding and after child-task failure.
An unjoined failed child does not crash the process; every join throws its original exception at the join site.
Root-task failure terminates the process. OOM, quotas, VM faults and forced shutdown remain noncatchable.

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
