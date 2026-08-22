# Kotlin Char and UTF-16 Literal ABI Design

> Issue: [#41](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/41)

## Purpose

This specification corrects the pre-release artifact 1.0 text ABI before the
managed heap and Kotlin `String` implementation begins in #40. The existing
artifact and Tier 0 runtime incorrectly model `Char` as a Unicode scalar value.
Kotlin common semantics instead expose `Char` as one unsigned 16-bit UTF-16
code unit and expose `String` as a sequence of those values.

The correction is intentionally made in artifact 1.0 rather than preserving an
unshipped incompatible format. All committed fixtures, semantic hashes, and
conformance digests move atomically to the corrected canonical bytes.

## Compatibility contract

The observable contract follows Kotlin common semantics:

- `Char` contains any value in `0x0000..=0xffff`.
- High and low surrogate code units are ordinary valid `Char` values.
- A supplementary Unicode code point is represented in a string by two
  `Char` values; the VM does not combine them for `length` or indexed access.
- An isolated surrogate remains losslessly representable in a string.
- Unicode normalization, grapheme segmentation, and code-point iteration are
  library operations and are not implicit VM behavior.

Runtime storage and artifact serialization are allowed to differ physically,
but neither may change this code-unit contract. The managed-string design in
#40 may later select a compact internal representation only if it preserves the
same observable sequence of `u16` values.

The Kotlin common library distinguishes numeric conversion from checked
construction. `Int.toChar()` retains the least-significant 16 bits, while
`Char(Int)` checks the inclusive `0..65535` range and throws on failure. The
bytecode `convert i32 -> char` implements the former. A compiler lowers the
checked constructor to an explicit range check followed by the same conversion
and an ordinary Kotlin exception path.

## Artifact 1.0 changes

### Character constants

Constant tag `5` remains `CHAR`, but its payload changes from a Unicode scalar
encoded as `u32` to one little-endian `u16`:

```text
u8  tag = 5
u16 code_unit
```

The record has exactly three bytes. There is no scalar-value validation because
all 65,536 bit patterns are valid. A shorter or longer record is non-canonical
and is rejected as `BadRecord`.

Constant sorting remains tag first and then exact payload bytes. The compact
payload therefore participates directly in canonical ordering and module
identity.

### UTF-16 literal pool

Every module has a required critical semantic section:

| Kind | Scope | Name |
|---:|---|---|
| `0x010a` | module | `UTF16_LITERALS` |

It uses the standard indexed-section envelope. Each record consists solely of
zero or more little-endian `u16` code units:

```text
u16 code_units[]
```

Rules:

- an empty record is canonical and represents the empty string;
- the record byte length must be even;
- every `u16` bit pattern is accepted, including null and isolated surrogates;
- records are strictly increasing and unique by their exact payload bytes;
- offsets and payload bytes obey the ordinary indexed-section bounds;
- the section is included in the module semantic digest in ascending section
  kind order;
- even a module with no string literals contains the empty indexed section, so
  superseded pre-correction artifacts are rejected rather than ambiguously
  reinterpreted.

Artifact limits gain `utf16_literal_code_units`, an independent upper bound on
the sum of all literal record lengths divided by two. The decoder checks record
count, even lengths, checked cumulative code-unit count, ordering, uniqueness,
and the limit before publishing decoded module state. It may retain byte ranges
into the immutable artifact rather than allocate decoded `u16` collections.

### String constants and UTF-8 metadata

Constant tag `6` remains `STRING` with a little-endian `u32` payload. The value
is redefined from `StringId` to `Utf16LiteralId` in the owning module:

```text
u8  tag = 6
u32 utf16_literal_id
```

Every reference is range-checked during verification. The `STRINGS` section
continues to contain only strict UTF-8 metadata such as module, type, function,
field, namespace, and source-path names. It never stores guest string content.
This separation keeps Rust/tooling metadata compatible with ordinary UTF-8
while preserving every possible Kotlin string literal.

Literal heap allocation, deduplication, interning, reference identity, and
startup ownership are deliberately deferred to #40. This issue only publishes
lossless verified literal metadata for that design to consume.

## Runtime value and instruction semantics

The private runtime representation becomes:

```rust
RuntimeValue::Char(u16)
```

Rust `char` is not used anywhere in the semantic value layer. Consequently the
type system itself prevents values wider than a Kotlin `Char`.

The only verifier-legal conversions involving `Char` remain:

```text
i32 -> char: low 16 bits, equivalent to value as u16
char -> i32: zero extension, equivalent to i32::from(value)
```

Both operations are total and cannot produce a guest trap. The private
`GuestTrap::InvalidCharacter` variant is removed. Other numeric/character
conversion pairs remain verifier errors.

Character equality and ordering compare unsigned `u16` values. Source and
destination aliasing retains the existing read-before-write rule. No operation
consults host Unicode tables, locale, native pointer width, or Rust build mode.

## Trace contract

An initialized `Char` register retains its runtime-value discriminant and
encodes exactly two little-endian payload bytes. Trace records do not retain the
former zero-extended four-byte scalar representation. All committed digests
whose active registers include `Char` are regenerated and reviewed in debug
and release builds.

The UTF-16 literal pool affects artifact and module hashes. It does not add a
heap reference to Tier 0 before #40 and therefore does not create a new runtime
trace event in this issue.

## Decoder, verifier, and admission boundaries

The container decoder recognizes `0x010a` as a required module section and
rejects duplicate, missing, wrongly scoped, or incorrectly flagged instances.
Directory element count must equal the indexed envelope count as for all other
indexed sections.

The records decoder validates the literal pool under `ArtifactLimits`, stores
bounded byte ranges, decodes `CHAR` with `read_u16`, and represents string
constants with a distinct `Utf16LiteralId` type. A string constant that refers
outside the owning module's literal pool is rejected before semantic
verification completes.

The verifier continues to prove `Char` register types and the exact legal
conversion pairs. It no longer diagnoses surrogate constants or conversion
results. Execution admission may resolve character constants immediately, but
must leave string literals as immutable verified metadata until #40 defines
their bounded heap publication.

## Canonical migration

This is an in-place correction to unshipped artifact format `1.0`. The format
major and minor remain `1.0`; no compatibility branch or dual decoder is kept.
All modules now require `UTF16_LITERALS`, making rejection of older bytes
deterministic even when they contain no character constants.

The test encoder, canonical vector generator, committed `.cpkt` files,
Markdown manifests, module semantic hashes, container digests, and execution
trace goldens are regenerated in one implementation series. Documentation must
not describe the superseded scalar-value model as an alternate v1 dialect.

## Conformance requirements

Character vectors cover:

- `0x0000`, `0xd7ff`, `0xd800`, `0xdfff`, `0xe000`, and `0xffff` constants;
- exact three-byte `CHAR` records and rejection of one- or three-byte payloads
  after the tag;
- `-1 -> 0xffff`, `65535 -> 0xffff`, `65536 -> 0x0000`, and signed boundary
  truncation;
- surrogate round trips through `i32 -> char -> i32`;
- unsigned equality and ordering at surrogate and range boundaries;
- rejection of every non-`i32`/`char` conversion pair.

Literal-pool vectors cover:

- empty literals and empty pools;
- embedded `0x0000`;
- a valid surrogate pair;
- isolated high and low surrogates;
- odd byte length, duplicates, non-increasing raw order, invalid IDs, excessive
  code units, truncated envelopes, and wrong section scope/flags;
- deterministic module hashes and byte-for-byte fixture regeneration.

Runtime tests execute in Rust debug and release modes and compare exact values,
outcomes, fixed/dynamic costs, and trace digests. The steady-state allocation
test remains zero. The public rustdoc boundary remains unchanged: execution
types and the new verified literal representation stay crate-private.

## Follow-up boundary

Issue #40 consumes this specification to define managed strings as observable
UTF-16 code-unit sequences. It owns heap layout, literal materialization,
interning and identity, string operations and costs, GC roots, OOM behavior, and
host UTF-8 conversion. It may not reinterpret `Utf16LiteralId` content as
Unicode scalar sequences or reject isolated surrogates.
