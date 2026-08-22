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
storage, slice size, request and response counts, argument count, and both
directions of string exchange. Allocation failure during admission is distinct
from managed guest-heap exhaustion during execution.

## Lifecycle

The host calls `start` once and then calls `advance(guest_budget,
maintenance_budget)`. An advance returns one of:

- `SliceExhausted`, allowing cooperative scheduling;
- one borrowed `HostRequest`, which suspends guest execution;
- stable `Halted`, `Crashed`, `Faulted`, `HostFailed`, or `QuotaExhausted`;
- stable managed `AllocationExhausted` with bounded diagnostics.

Only one request can be outstanding. Calling `advance` while it is outstanding
returns the same request ID and values without consuming a budget, changing
accounting, or changing the trace. The host performs any asynchronous work
outside the VM and later calls `resume(id, response)`. Thus the Rust API calls
are synchronous state transitions even when the capability operation itself is
asynchronous.

`resume` validates the pending ID, success type, and string bound before any
response is accepted. Wrong or stale IDs, wrong types, and oversized strings
are correctable `ResumeError` values: the original request and accounting stay
unchanged. A valid response is accepted exactly once. Explicit host failures
become stable `HostFailed` outcomes rather than guest traps or VM faults.

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

Request-count exhaustion is checked before argument construction, string
copying, or ID publication. Accepted-response exhaustion is checked after
structural validation and before consuming the response. Both establish a
stable bounded `QuotaExhausted` outcome. Outbound code-unit exhaustion is also
terminal before publication; an oversized inbound value remains a correctable
`ResumeError`.

`Session::accounting()` returns fixed guest units, dynamic guest units,
maintenance units, entered blocks, executed instructions, published requests,
accepted responses, and the current SHA-256 trace digest. The digest is one
chronological event stream shared by guest block entries and host exchanges.
Every trace field is framed by a little-endian `u32` byte length. Host request
events use tag 2 followed by request ID (`u64`), capability index (`u32`),
operation (`u32`), argument count (`u32`), and typed values. Host response
events use tag 3 followed by request ID, success/failure tag, and a typed value
or bounded failure kind/code. Scalar payloads are little-endian. A string uses
type tag 7, its `u32` code-unit count, then its `u16` units as framed fields.

Legal request/resume operation performs no native allocation after admission.
Managed string objects still consume the explicitly reserved guest heap and
may cause budgeted GC maintenance.

## Adapter responsibilities

A terminal, JNI, Minecraft, test, or future addon adapter owns capability
implementations, asynchronous dispatch, cancellation policy, UTF conversion,
and mapping host errors to bounded `HostFailure` values. It must not retain a
borrowed request view across a mutable session call. It should copy or consume
the request immediately, perform external work without holding the session
borrow, and resume later with the exact request ID.

The VM intentionally does not spawn threads, perform I/O, interpret wall-clock
time, or decide how multiple computers run in parallel. A host scheduler can
drive many individually single-task sessions concurrently while preserving the
same per-session semantics.
