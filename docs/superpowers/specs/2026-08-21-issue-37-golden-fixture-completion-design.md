# Artifact v1 Golden Fixture Completion Design

> Issue: [#37](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/37)

## Status and purpose

This design completes the binary-fixture obligations inherited from the
accepted artifact and bytecode v1 specification. It does not change the v1
format, verifier semantics, or public VM API. It adds committed external test
vectors that another implementation can inspect and consume without executing
the Rust fixture builders.

## Committed fixture set

`tests/fixtures/` contains these immutable inputs:

- `vector-a.cpkt`: the exact canonical minimal application from the v1
  specification.
- `two-module.cpkt`: an application importing one typed function from one
  library, with both semantic module hashes fixed.
- `language-runtime.cpkt`: one verified application exercising a concrete
  class allocation, a nullable-reference branch, array allocation and access,
  a loop backedge targeting a loop-header safepoint, and a caught exception.
- `debug.cpkt`: a verified application with a non-semantic debug section,
  Kotlin UTF-16 source ranges, and at least two records whose second record
  names the first as its inline parent.
- `host-runtime.code`: canonical framed instruction records covering coroutine
  spawn, sleep, and join plus synchronous and asynchronous capability calls.
  This is a raw CODE payload rather than an executable artifact because the v1
  specification requires instruction records for this case.

Every binary has a neighboring `<name>.manifest.md`. Artifact manifests list
the exact file length, payload end, artifact SHA-256, semantic feature bits,
each directory entry, every indexed-record absolute offset and length, and each
module semantic SHA-256. The code manifest lists every instruction offset,
length, opcode, form, and fixed cost. Hexadecimal identities use lowercase
ASCII and fixed width.

## Canonical construction and regeneration

The existing explicit little-endian fixture builder remains test-only and is
split into focused helpers where needed. It constructs each committed fixture
from a declarative fixture description; it never serializes Rust memory layouts
or uses the production decoder as its source of bytes.

An ignored integration test named `regenerate_committed_fixtures` is the only
repository-writing path. It rewrites all fixture binaries and manifests when
invoked explicitly with:

```text
cargo test --test golden_fixtures regenerate_committed_fixtures \
  --locked --offline -- --ignored --exact
```

Normal tests and CI are read-only. They independently rebuild each vector in
memory and require equality with the committed file and manifest. A changed
format byte therefore fails until the developer intentionally regenerates and
reviews both representations.

## Decoded round-trip proof

A crate-private module compiled only for tests serializes `DecodedArtifact`
back into canonical v1 bytes. It encodes every known global and module record,
all value and nominal type forms, all constants, and all 52 v1 opcodes. It
derives module hashes, directory offsets, padding, payload length, and the final
artifact digest rather than copying the original container or section payloads.

For each committed `.cpkt`, a crate unit test performs:

```text
committed bytes -> production decoder -> test-only canonical encoder -> bytes
```

and requires exact byte equality. The encoder is deliberately unavailable in
non-test builds and is not exported from the crate. Round-trip scope is the
known v1 core sections represented by `DecodedArtifact`; skipped unknown
optional extensions are outside this proof because the decoded model does not
retain them.

## Verification boundaries

Integration tests load every committed `.cpkt` through public
`verify_artifact`, using default parser limits, and assert its fixed content
hash, entry point, and module count. The language/runtime and debug vectors also
receive crate-level structural assertions so the tests prove that their named
features are present rather than merely accepting arbitrary valid artifacts.

`host-runtime.code` is decoded through the instruction-record boundary. Its
test asserts the exact instruction variants, operands, terminator placement,
semantic feature implications, and recalculated fixed costs.

The fixture suite does not perform device admission, capability binding,
execution, scheduling, allocation, or Kotlin semantic conformance. Those remain
separate runtime slices.

## Failure and review policy

Fixture generation fails on checked arithmetic, an unrepresentable record
count, or an invalid declarative fixture. Normal tests report byte or manifest
mismatches without rewriting files. Production decoding and verification
remain responsible for diagnostics on untrusted bytes; test encoder failures
are ordinary test failures and never become public diagnostic codes.

Completion requires all committed bytes to verify or decode through their
specified boundary, every generated manifest to match its committed text, all
artifact round trips to be byte-identical, debug and language/runtime feature
assertions to pass, debug and release mutation corpora to remain green, and the
full Clippy, test, and rustdoc commands to pass offline.
