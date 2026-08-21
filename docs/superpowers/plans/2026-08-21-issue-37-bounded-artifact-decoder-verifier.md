# Bounded Artifact Decoder and Verifier Implementation Plan

> Issue: [#37](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/37)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Load untrusted Compukter artifact v1 bytes into immutable verified modules with bounded allocation, deterministic diagnostics, and no executable state published before complete validation.

**Architecture:** A small byte cursor and structural decoder produce crate-private `DecodedArtifact` tables backed by one immutable `Arc<[u8]>`. Independent verifier passes resolve identities, validate nominal types and module graphs, then prove instruction/CFG/register/exception/cost invariants before constructing the public `VerifiedArtifact`. The only runtime dependency is a no-default-features SHA-256 implementation; fixture construction remains test-only and explicit.

**Tech Stack:** Rust 2021, `sha2` without default features, standard-library `Arc`, integration tests, `cargo fmt`, Clippy, offline Cargo verification.

---

## File map

- `src/lib.rs` — narrow public entrypoint and exports; no unverified constructors.
- `src/limits.rs` — caller-supplied allocation/traversal/diagnostic limits.
- `src/diagnostic.rs` — stable diagnostic families, codes, bounded locations, and collection.
- `src/bytes.rs` — checked fixed-width, UTF-8, and canonical ULEB128 reads.
- `src/artifact/mod.rs` — crate-private decoded records and public immutable verified view.
- `src/artifact/format.rs` — v1 constants, flags, tags, and opcode metadata.
- `src/decode/mod.rs` — staged structural decoder orchestration.
- `src/decode/container.rs` — header, SHA-256, directory, range, alignment, and gap checks.
- `src/decode/indexed.rs` — indexed-section envelope validation.
- `src/decode/records.rs` — global and module record decoding.
- `src/decode/code.rs` — instruction framing and operand decoding.
- `src/verify/mod.rs` — verifier orchestration and publication boundary.
- `src/verify/modules.rs` — module digest, dependency, import/export, and nominal type graph checks.
- `src/verify/functions.rs` — signatures, fields, calls, capabilities, and instruction typing.
- `src/verify/cfg.rs` — block ownership, reachability, definite initialization, joins, and backedges.
- `src/verify/exceptions.rs` — protected-range nesting and handler-entry state.
- `tests/support/mod.rs` — test-only canonical artifact builder and mutation helpers.
- `src/decode/tests.rs` — crate-private structural, record, and instruction decoder tests.
- `src/verify/tests.rs` — crate-private symbol, type, CFG, register, handler, capability, and cost tests.
- `tests/minimal_artifact.rs` — exact #36 vector and final public API contract.
- `tests/container_rejection.rs` — final public-API structural rejection regressions.
- `tests/semantic_verification.rs` — final public-API semantic rejection regressions.
- `tests/bounded_failures.rs` — strict-limit and deterministic mutation corpus.

### Task 1: Bounded byte and diagnostic foundation

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Create: `src/limits.rs`
- Create: `src/diagnostic.rs`
- Create: `src/bytes.rs`

- [ ] **Step 1: Add failing unit tests for checked reads and canonical ULEB128**

Place tests at the bottom of `src/bytes.rs` that require exact offsets and reject truncated, overflowing, and redundant ULEB128 values:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_values_and_tracks_offset() {
        let mut cursor = Cursor::new(&[0x34, 0x12, 0x78, 0x56]);
        assert_eq!(cursor.read_u16().unwrap(), 0x1234);
        assert_eq!(cursor.read_u16().unwrap(), 0x5678);
        assert_eq!(cursor.position(), 4);
    }

    #[test]
    fn rejects_non_canonical_uleb128() {
        let error = Cursor::new(&[0x80, 0x00]).read_uleb32().unwrap_err();
        assert_eq!(error.code, Code::NonCanonicalUleb128);
        assert_eq!(error.location.offset, Some(0));
    }

    #[test]
    fn rejects_truncated_fixed_read() {
        let error = Cursor::new(&[1, 2, 3]).read_u32().unwrap_err();
        assert_eq!(error.code, Code::UnexpectedEnd);
    }
}
```

- [ ] **Step 2: Run the focused test and confirm the missing implementation failure**

Run: `cargo test bytes::tests --locked --offline`

Expected: FAIL because `Cursor`, `Code`, and read methods do not exist.

- [ ] **Step 3: Add the dependency and minimal bounded primitives**

Add to `Cargo.toml`:

```toml
[dependencies]
sha2 = { version = "0.10", default-features = false }
```

Define `ArtifactLimits` with `#[derive(Clone, Debug)]` and public fields `artifact_bytes`, `sections`, `modules`, `records_per_section`, `strings_bytes`, `code_bytes`, `functions`, `blocks`, `registers_per_function`, `imports`, `exceptions`, `capabilities`, `debug_bytes`, and `diagnostics`. Its `Default` implementation uses conservative host-independent values and is never an admission profile.

Define these exact diagnostic primitives:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Family { Container, Section, Limit, Module, Symbol, Type, Code, Cfg, Register, Exception, Capability, Cost }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Code {
    UnexpectedEnd, IntegerOverflow, NonCanonicalUleb128, InvalidUtf8,
    LimitExceeded, BadMagic, UnsupportedVersion, BadLength, BadDigest,
    BadDirectory, BadSection, BadRecord, BadModule, BadSymbol, BadType,
    BadInstruction, BadControlFlow, UninitializedRegister, BadException,
    BadCapability, BadCost,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Location {
    pub offset: Option<u64>, pub section: Option<u16>, pub module: Option<u32>,
    pub function: Option<u32>, pub block: Option<u32>, pub instruction: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic { pub family: Family, pub code: Code, pub location: Location, pub detail: String }
```

`DiagnosticSet::push` truncates `detail` to 256 UTF-8 bytes and stores at most `limits.diagnostics` entries. Implement `Cursor<'a>` with checked `take`, `read_u8/u16/u32/u64`, `read_i32`, `read_uleb32`, and `read_utf8` methods. Every error records the cursor offset at the start of the failed value.

- [ ] **Step 4: Run foundation tests and lint**

Run: `cargo test bytes::tests --locked --offline`

Expected: 3 tests PASS.

Run: `cargo generate-lockfile`

Expected: `Cargo.lock` pins `sha2` and its hashing primitives. If the local Cargo cache lacks them, allow this one dependency-resolution command network access, then use `--locked --offline` for every subsequent command.

Run: `cargo clippy --lib --tests --all-features -- -D warnings`

Expected: PASS with no warnings.

- [ ] **Step 5: Commit the foundation**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/limits.rs src/diagnostic.rs src/bytes.rs
git commit -m "feat(vm): add bounded artifact parsing primitives (#37)"
```

### Task 2: Header, digest, and directory validation

**Files:**
- Create: `src/artifact/mod.rs`
- Create: `src/artifact/format.rs`
- Create: `src/decode/mod.rs`
- Create: `src/decode/container.rs`
- Create: `tests/support/mod.rs`
- Create: `src/decode/tests.rs`

- [ ] **Step 1: Build one valid empty-shaped container and failing rejection tests**

Create a test-only `ArtifactBuilder` that writes fixed-width fields explicitly and recomputes the SHA-256 trailer. Do not serialize Rust structs with `serde` or memory casts. Add tests:

```rust
#[test]
fn rejects_bad_magic_before_directory_decode() {
    let mut bytes = support::minimal_vector();
    bytes[0] = b'X';
    support::rehash(&mut bytes);
    assert_code(bytes, Code::BadMagic);
}

#[test]
fn rejects_directory_overlap() {
    let bytes = support::minimal_vector_with_overlapping_sections();
    assert_code(bytes, Code::BadDirectory);
}

#[test]
fn rejects_non_zero_alignment_gap() {
    let bytes = support::minimal_vector_with_non_zero_gap();
    assert_code(bytes, Code::BadDirectory);
}
```

- [ ] **Step 2: Run the container tests and confirm they fail**

Run: `cargo test decode::tests::container --locked --offline`

Expected: FAIL because `decode::container` does not exist.

- [ ] **Step 3: Implement stage-one container validation**

Define `Header`, `DirectoryEntry`, `SectionKey`, and `Container<'a>` as crate-private types. Implement:

```rust
pub(crate) fn decode_container<'a>(
    bytes: &'a [u8],
    limits: &ArtifactLimits,
) -> Result<Container<'a>, DiagnosticSet>;
```

The function follows #36 order exactly: outer byte limit; 64-byte header; exact format/runtime fields; checked `payload_end + 32`; SHA-256; checked `section_count * 32`; directory ordering; known flags/scopes; exact packed aligned offsets; non-overlap; zero gaps; exact trailer position. It must not allocate from `section_count` until `section_count <= limits.sections` and multiplication succeeds.

In `format.rs`, define header offsets, section kinds, `CRITICAL`, `SEMANTIC`, and the four semantic feature bits as named constants. Avoid `#[repr(C)]` parsing.

- [ ] **Step 4: Run positive and rejection tests**

Run: `cargo test decode::tests::container --locked --offline`

Expected: all tests PASS, including bad digest, unsupported major, unknown feature bit, duplicate key, wrong scope, wrong first offset, and truncated trailer cases.

- [ ] **Step 5: Commit container validation**

```bash
git add src/artifact src/decode tests/support src/lib.rs
git commit -m "feat(vm): validate artifact v1 containers (#37)"
```

### Task 3: Indexed sections, UTF-8, and record-local bounds

**Files:**
- Create: `src/decode/indexed.rs`
- Modify: `src/decode/mod.rs`
- Modify: `src/decode/tests.rs`
- Modify: `tests/support/mod.rs`

- [ ] **Step 1: Add failing indexed-envelope tests**

Cover `offsets[0] != 0`, decreasing offsets, last offset mismatch, non-zero envelope padding, directory/record count disagreement, record count over limit, invalid UTF-8, duplicate/unsorted strings, and empty string outside index zero. The assertion helper must compare both code and section location:

```rust
let diagnostic = first_error(bytes);
assert_eq!(diagnostic.code, Code::BadRecord);
assert_eq!(diagnostic.location.section, Some(format::STRINGS));
```

- [ ] **Step 2: Confirm focused failures**

Run: `cargo test decode::tests::indexed --locked --offline`

Expected: FAIL on the first unimplemented envelope validation.

- [ ] **Step 3: Implement borrowed indexed views**

Implement:

```rust
pub(crate) struct IndexedSection<'a> {
    pub kind: u16,
    offsets: Vec<u32>,
    records: &'a [u8],
}

impl<'a> IndexedSection<'a> {
    pub fn decode(entry: &DirectoryEntry, payload: &'a [u8], limits: &ArtifactLimits)
        -> Result<Self, Diagnostic>;
    pub fn len(&self) -> usize;
    pub fn record(&self, id: u32) -> Result<&'a [u8], Diagnostic>;
}
```

Check limits and all offset arithmetic before allocating `offsets`. Add `decode_string_table` that borrows `&str` records, enforces canonical raw-byte order/deduplication, and applies `strings_bytes` to the sum before walking strings.

- [ ] **Step 4: Run indexed and complete container tests**

Run: `cargo test decode::tests --locked --offline`

Expected: all structural tests PASS.

- [ ] **Step 5: Commit indexed decoding**

```bash
git add src/decode/indexed.rs src/decode/mod.rs src/decode/tests.rs tests/support/mod.rs
git commit -m "feat(vm): decode bounded indexed artifact sections (#37)"
```

### Task 4: Decode all v1 records into crate-private tables

**Files:**
- Modify: `src/artifact/mod.rs`
- Create: `src/decode/records.rs`
- Modify: `src/decode/mod.rs`
- Modify: `src/decode/tests.rs`
- Modify: `tests/support/mod.rs`

- [ ] **Step 1: Add the exact #36 minimal-vector test**

The crate-private test asserts the committed vector constants and decoded observations:

```rust
#[test]
fn records_decode_spec_vector_a() {
    let bytes = support::minimal_vector();
    assert_eq!(bytes.len(), 1088);
    assert_eq!(support::module_hash(&bytes), hex("f73d8f8699e060aac0df1079d820a9fd778a649dd391980c23ee2a4e3c17c2cc"));
    assert_eq!(support::artifact_hash(&bytes), hex("88803a07260a3b0123ef230b482a682400e6cae03e90f3be0a117419406509d3"));
    let artifact = decode_artifact(bytes.into(), &ArtifactLimits::default()).unwrap();
    assert_eq!(artifact.modules.len(), 1);
    assert_eq!(artifact.header.entry_module, 0);
    assert_eq!(artifact.header.entry_function, 0);
}
```

- [ ] **Step 2: Confirm the record-decoding failure**

Run: `cargo test decode::tests::records_decode_spec_vector_a --locked --offline`

Expected: FAIL because module record decoding is not implemented.

- [ ] **Step 3: Define decoded records and decode every section shape**

Create crate-private owned metadata using small ID newtypes (`ModuleId`, `TypeId`, `FunctionId`, `BlockId`, `ImportId`) and borrowed ranges into `Arc<[u8]>`. Decode `Manifest`, `Module`, `Capability`, `ValueType`, `NominalType`, `Constant`, `Import`, `Export`, `Field`, `Function`, `Block`, `ExceptionEntry`, and `DebugEntry` exactly as #36 defines.

The orchestrator signature is:

```rust
pub(crate) fn decode_artifact(
    bytes: Arc<[u8]>,
    limits: &ArtifactLimits,
) -> Result<DecodedArtifact, DiagnosticSet>;
```

Before pushing each table, validate its directory count against the relevant limit and reserve with `try_reserve_exact`; map reserve failure to `LimitExceeded`. Require all core global/module sections exactly once, `DEBUG` at most once, dense scopes, matching declared counts, reserved zero fields, canonical record order, scalar tags, Unicode scalars, and absent sentinels.

- [ ] **Step 4: Run record and container tests**

Run: `cargo test decode::tests --locked --offline`

Expected: all structural record and container tests PASS without requiring a public unverified API.

- [ ] **Step 5: Commit record decoding**

```bash
git add src/artifact/mod.rs src/decode/records.rs src/decode/mod.rs src/decode/tests.rs tests/support/mod.rs
git commit -m "feat(vm): decode artifact v1 records (#37)"
```

### Task 5: Decode framed instructions and recompute fixed costs

**Files:**
- Create: `src/decode/code.rs`
- Modify: `src/artifact/format.rs`
- Modify: `src/artifact/mod.rs`
- Modify: `src/decode/mod.rs`
- Modify: `src/decode/tests.rs`

- [ ] **Step 1: Add failing instruction-schema tests**

Add cases for unknown opcode/form, length below four, operand overrun, trailing operand bytes, non-canonical ID, excessive argument/case count, missing terminator, instruction after terminator, and mismatched instruction count. Include a positive unit return and arithmetic block.

- [ ] **Step 2: Confirm the instruction tests fail**

Run: `cargo test decode::tests::instruction --locked --offline`

Expected: FAIL with the first unknown/missing code decoder path.

- [ ] **Step 3: Implement explicit instruction decoding**

Define an `Instruction` enum with one variant for every opcode in #36 and typed operand structs for calls, suspending calls, switches, and capability calls. Implement one exhaustive `match opcode`; do not use transmute or an unknown-instruction fallback. Each decoder checks `form`, exact `byte_length`, list limits, register sentinel rules, and canonical ULEB128.

Add `Instruction::fixed_cost() -> Result<u32, Diagnostic>` using checked arithmetic and the exact table formulas (`4 + argc`, `1 + cases`, and so on). Decode each `CODE` record into a boxed instruction slice only after `code_bytes` and instruction-count limits pass.

- [ ] **Step 4: Run instruction tests and Clippy**

Run: `cargo test decode::tests::instruction --locked --offline`

Expected: all instruction tests PASS.

Run: `cargo clippy --lib --tests --all-features -- -D warnings`

Expected: PASS.

- [ ] **Step 5: Commit instruction decoding**

```bash
git add src/decode/code.rs src/artifact/format.rs src/artifact/mod.rs src/decode/mod.rs src/decode/tests.rs
git commit -m "feat(vm): decode typed bytecode instructions (#37)"
```

### Task 6: Verify modules, hashes, symbols, and nominal types

**Files:**
- Create: `src/verify/mod.rs`
- Create: `src/verify/modules.rs`
- Create: `src/verify/tests.rs`
- Modify: `src/artifact/mod.rs`
- Modify: `tests/support/mod.rs`

- [ ] **Step 1: Add failing module-graph and identity tests**

Create a valid two-module fixture and mutations for wrong semantic hash, import cycle, missing target, missing export, duplicate export key, ambiguous resolution, signature mismatch, bad superclass/interface, inheritance cycle, invalid field owner, and illegal abstract/final flags.

- [ ] **Step 2: Confirm module verification fails**

Run: `cargo test verify::tests::module --locked --offline`

Expected: FAIL because `verify::modules` does not exist.

- [ ] **Step 3: Implement deterministic resolution passes**

Implement these passes in order:

```rust
verify_module_counts(&decoded)?;
verify_module_hashes(&decoded)?;
let order = topological_module_order(&decoded)?;
let symbols = resolve_imports_and_exports(&decoded, &order)?;
let types = verify_nominal_types(&decoded, &symbols)?;
```

Use color-state DFS with a bounded explicit stack for import and inheritance graphs; do not recurse on artifact-controlled depth. Hash semantic sections directly from original bytes with the specified domain separator. Resolve imports to dense internal handles, but keep these handles crate-private and distinct from integers. Verify array/function/class/interface record shapes, generic erasure metadata, sorted unique interfaces, field ranges, method ranges, and exact signature agreement.

- [ ] **Step 4: Run module/type tests**

Run: `cargo test verify::tests::module --locked --offline`

Expected: valid two-module fixture PASS; every named mutation returns its expected `Module`, `Symbol`, or `Type` diagnostic.

- [ ] **Step 5: Commit module and type verification**

```bash
git add src/verify src/artifact/mod.rs tests/support/mod.rs
git commit -m "feat(vm): verify artifact modules and type identities (#37)"
```

### Task 7: Verify functions, CFG, registers, calls, and fields

**Files:**
- Create: `src/verify/functions.rs`
- Create: `src/verify/cfg.rs`
- Modify: `src/verify/mod.rs`
- Modify: `src/verify/tests.rs`

- [ ] **Step 1: Add failing semantic bytecode tests**

Cover bad entry function, non-contiguous function blocks, target outside function, unreachable block, no terminator, backedge to non-safepoint, uninitialized read, wrong destination type, incompatible join, wrong argument count/type, invalid receiver, abstract direct call, immutable field write, nullable receiver use, forged import-kind reference, and incorrect block cost.

- [ ] **Step 2: Confirm CFG/register tests fail**

Run: `cargo test verify::tests::cfg --locked --offline`

Expected: FAIL before publication.

- [ ] **Step 3: Implement work-list verification without artifact-depth recursion**

For each non-abstract function:

1. Validate signature, parameter register types, block range, exception range, and entry ownership.
2. Decode successor lists from the final terminator and validate every target.
3. Mark ordinary reachability with a bounded `VecDeque`; reject unvisited non-handler blocks.
4. Initialize entry bits for parameters and compute predecessor-state intersections to a fixed point.
5. Walk instructions in order, checking every source before marking a destination initialized.
6. Validate direct/virtual/interface calls, fields, array element types, nullable receiver rules, casts, refs, coroutine handles, and capability operand kinds.
7. Sum fixed costs with checked arithmetic and compare exact declared/manifest limits.
8. Require every backward edge to target a loop-header safepoint.

Represent initialization as `Vec<u64>` bitsets sized only after `register_count <= limits.registers_per_function`. Reuse scratch bitsets per function instead of allocating per instruction.

- [ ] **Step 4: Run semantic tests**

Run: `cargo test verify::tests --locked --offline`

Expected: function/CFG/register/call/field positive cases PASS and named invalid cases return stable diagnostics.

- [ ] **Step 5: Commit function verification**

```bash
git add src/verify/functions.rs src/verify/cfg.rs src/verify/mod.rs src/verify/tests.rs
git commit -m "feat(vm): verify typed bytecode control flow (#37)"
```

### Task 8: Verify exceptions, suspension, capabilities, and publication

**Files:**
- Create: `src/verify/exceptions.rs`
- Modify: `src/verify/functions.rs`
- Modify: `src/verify/mod.rs`
- Modify: `src/verify/tests.rs`
- Modify: `src/artifact/mod.rs`
- Modify: `src/lib.rs`
- Create: `tests/minimal_artifact.rs`
- Create: `tests/container_rejection.rs`
- Create: `tests/semantic_verification.rs`

- [ ] **Step 1: Add failing boundary tests**

Cover crossing exception ranges, empty/out-of-function ranges, bad catch type, incompatible exception register, invalid handler initialization, suspend opcode in non-suspending function, non-suspending target for `call_suspend`, capability ID/operation/signature mismatch, required/optional manifest count mismatch, missing feature bits, and public construction attempts through compile-fail doctests.

- [ ] **Step 2: Confirm boundary tests fail**

Run: `cargo test verify::tests::exception --locked --offline`

Expected: FAIL because handler and publication checks are missing.

- [ ] **Step 3: Complete verifier and expose only verified state**

Implement nested-range validation with an explicit stack ordered by the canonical exception table. Compute handler initialized state by intersecting states immediately before every potentially throwing protected instruction, then add parameters and the exception register. Verify suspending/coroutine/capability rules and exact semantic feature-bit use.

Expose:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryPoint { pub module: u32, pub function: u32 }

pub struct VerifiedArtifact { inner: Arc<VerifiedArtifactInner> }

impl VerifiedArtifact {
    pub fn content_hash(&self) -> [u8; 32];
    pub fn entry(&self) -> EntryPoint;
    pub fn module_count(&self) -> usize;
}

pub fn verify_artifact(
    bytes: Arc<[u8]>,
    limits: ArtifactLimits,
) -> Result<VerifiedArtifact, DiagnosticSet>;
```

Keep `DecodedArtifact`, `VerifiedArtifactInner`, resolved handles, and every constructor `pub(crate)`. Construct `VerifiedArtifact` only after all verifier passes return success.

- [ ] **Step 4: Run the full semantic suite**

Run: `cargo test --test minimal_artifact --test container_rejection --test semantic_verification --locked --offline`

Expected: all tests PASS, including Vector A and two-module publication.

- [ ] **Step 5: Commit the verified publication boundary**

```bash
git add src/verify src/artifact/mod.rs src/lib.rs tests/minimal_artifact.rs tests/container_rejection.rs tests/semantic_verification.rs
git commit -m "feat(vm): publish only verified artifacts (#37)"
```

### Task 9: Strict-limit and deterministic mutation corpus

**Files:**
- Create: `tests/bounded_failures.rs`
- Modify: `tests/support/mod.rs`
- Modify: `src/diagnostic.rs`
- Modify: decoder/verifier files only where a corpus test exposes an actual gap

- [ ] **Step 1: Add limit-matrix and mutation tests**

Use a deterministic xorshift64 seed and mutate each byte position with `0x00`, `0xff`, and one bit flip for Vector A. Wrap calls in `std::panic::catch_unwind` and assert no panic. Add one test per `ArtifactLimits` field using a valid fixture just above the limit and assert `Code::LimitExceeded` before proportional allocation. Run each malformed artifact twice and assert equal diagnostics.

```rust
let first = std::panic::catch_unwind(|| verify_artifact(bytes.clone().into(), limits.clone()));
let second = std::panic::catch_unwind(|| verify_artifact(bytes.into(), limits));
assert!(first.is_ok() && second.is_ok());
assert_eq!(first.unwrap().unwrap_err(), second.unwrap().unwrap_err());
```

- [ ] **Step 2: Run the corpus and record the first failure**

Run: `cargo test --test bounded_failures --locked --offline`

Expected: initial FAIL identifies either a panic, nondeterministic diagnostic, or missing early limit check.

- [ ] **Step 3: Close every observed boundedness gap**

Replace unchecked arithmetic with `checked_*`, indexing with `get`, recursion with bounded work lists, eager collection with limit-before-reserve, and unbounded diagnostic formatting with fixed enum/location fields plus 256-byte detail truncation. Add each discovered input as a named regression test before changing production code.

- [ ] **Step 4: Run corpus, full tests, and release-mode corpus**

Run: `cargo test --test bounded_failures --locked --offline`

Expected: PASS.

Run: `cargo test --release --test bounded_failures --locked --offline`

Expected: PASS with identical diagnostic assertions.

- [ ] **Step 5: Commit bounded failure coverage**

```bash
git add tests/bounded_failures.rs tests/support src
git commit -m "test(vm): harden artifact verification limits (#37)"
```

### Task 10: Documentation and final verification

**Files:**
- Modify: `README.md`
- Modify: `src/lib.rs`
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add public API examples and CI coverage**

Document that callers pass immutable bytes and explicit limits, only `VerifiedArtifact` crosses the boundary, integrity is not authenticity, and device admission/execution are separate. Add a compiling crate-level example using `Arc<[u8]>`. Keep CI commands aligned with local verification and add no network-dependent test behavior.

- [ ] **Step 2: Run formatting and inspect the diff**

Run: `cargo fmt --check`

Expected: PASS.

Run: `git diff --check`

Expected: PASS.

- [ ] **Step 3: Run complete lint and tests**

Run: `cargo clippy --all-targets --all-features -- -D warnings`

Expected: PASS.

Run: `cargo test --locked --offline`

Expected: all unit, integration, doc, positive, negative, and mutation tests PASS.

- [ ] **Step 4: Audit dependencies and public surface**

Run: `cargo tree --locked --offline`

Expected: the runtime dependency graph contains only `sha2` and its transitive hashing primitives; no serialization, async runtime, JIT, or platform integration crates.

Run: `cargo doc --no-deps --document-private-items --locked --offline`

Expected: PASS without rustdoc warnings; decoded/unverified constructors are not public.

- [ ] **Step 5: Commit final documentation**

```bash
git add README.md src/lib.rs .github/workflows/ci.yml Cargo.lock
git commit -m "docs(vm): document verified artifact loading (#37)"
```

## Completion checkpoint

Before marking #37 complete, rerun every Task 10 command from a clean worktree, inspect `git status --short`, and compare implemented section/opcode coverage against every table in the #36 specification. Keep #37 open if any binary fixture, strict-limit case, or exact verifier invariant remains unimplemented; every known v1 record and opcode must have an explicit decoder branch.
