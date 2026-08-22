# Kotlin Char and UTF-16 Literal ABI Implementation Plan

> Issue: [#41](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/41)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Correct artifact 1.0 and the private Tier 0 runtime so Kotlin `Char` is exactly one arbitrary UTF-16 code unit and guest string constants address a lossless, bounded UTF-16 literal pool.

**Architecture:** Add one required module-scoped indexed section, `UTF16_LITERALS`, while retaining strict UTF-8 only for metadata in `STRINGS`. Decode literals as checked byte ranges into the immutable artifact, give literal IDs their own type, serialize `CHAR` in three bytes, and carry `Char` as `u16` through the runtime and trace. This is an atomic pre-release correction of artifact 1.0; there is no compatibility decoder.

**Tech Stack:** Rust 2021, `sha2`, Cargo unit/integration tests, committed binary golden fixtures, Markdown specifications.

---

## Execution rules

- Work directly on `main`; do not create a feature branch or worktree.
- Keep `.github/copilot-instructions.md` and both issue #41 documents in the documentation commit that precedes implementation.
- Use test-driven development: add one focused failing test, observe the intended failure, implement the smallest production change, and rerun the focused test.
- Never hand-edit fixture hashes or trace digests. Regenerate bytes using the committed generator, then review the resulting manifests and exact digest changes.
- Keep the artifact version at `1.0`. Do not add a legacy `u32` character decoder or make `UTF16_LITERALS` optional.
- Run every `gh` command outside the sandbox, as required by the workspace `AGENTS.md`.

## Task 1: Commit the accepted design and implementation plan

**Files:**

- Add: `.github/copilot-instructions.md`
- Add: `docs/superpowers/specs/2026-08-22-issue-41-kotlin-char-utf16-literals-design.md`
- Add: `docs/superpowers/plans/2026-08-22-issue-41-kotlin-char-utf16-literals.md`

- [x] **Step 1: Check the documents for unresolved language**

Run:

```sh
rg -n "TODO|TBD|Unicode scalar|optional UTF16|legacy CHAR" \
  .github/copilot-instructions.md \
  docs/superpowers/specs/2026-08-22-issue-41-kotlin-char-utf16-literals-design.md \
  docs/superpowers/plans/2026-08-22-issue-41-kotlin-char-utf16-literals.md
```

Expected: only explicit descriptions of the superseded Unicode-scalar behavior, if any; no `TODO`, `TBD`, optional-section, or compatibility-decoder requirement.

- [x] **Step 2: Check formatting and scope**

Run:

```sh
git diff --check
git status --short
```

Expected: no whitespace errors; only the three files listed above are uncommitted.

- [x] **Step 3: Commit and push the planning baseline**

Run:

```sh
git add .github/copilot-instructions.md \
  docs/superpowers/specs/2026-08-22-issue-41-kotlin-char-utf16-literals-design.md \
  docs/superpowers/plans/2026-08-22-issue-41-kotlin-char-utf16-literals.md
git commit -m "docs(vm): plan Kotlin text ABI correction (#41)"
git push origin main
```

Expected: the commit is on `main` and the push succeeds.

## Task 2: Register the required module section and independent limit

**Files:**

- Modify: `src/artifact/format.rs`
- Modify: `src/limits.rs`
- Modify: `src/verify/modules.rs`
- Modify: `src/decode/tests.rs`
- Modify: `tests/bounded_failures.rs`
- Modify: `tests/support/mod.rs`

- [x] **Step 1: Add failing section-contract tests**

Extend decoder/container tests so a minimal artifact fails when module section `0x010a` is missing, duplicated, global-scoped, non-semantic, or non-critical. Add a bounded-failure test that sets `utf16_literal_code_units` below the fixture requirement. The test support builder must accept literal records but must not yet make the new section valid by default.

Run:

```sh
cargo test --locked --offline decode::tests::utf16_literal_section -- --nocapture
```

Expected: FAIL because `0x010a` is not a known required module section.

- [x] **Step 2: Add the format constant and required-section membership**

Add after `EXCEPTIONS` in `src/artifact/format.rs`:

```rust
pub(crate) const UTF16_LITERALS: u16 = 0x010a;
```

Include it in `is_module`, and include it in the required semantic module-kind list in `src/verify/modules.rs`. Required flags are the existing `CRITICAL | SEMANTIC` contract used by semantic module sections.

- [x] **Step 3: Add the host policy limit**

Add to `ArtifactLimits` and its default:

```rust
pub utf16_literal_code_units: usize,
// ...
utf16_literal_code_units: 4 * 1024 * 1024,
```

Update every explicit `ArtifactLimits { ... }` literal found by:

```sh
rg -n "ArtifactLimits \{" src tests
```

Use struct update syntax where appropriate; do not silently reuse `strings_bytes` for guest literal content.

- [x] **Step 4: Emit an empty required section in all test artifact builders**

In every `semantic_sections` list in `tests/support/mod.rs`, insert an indexed empty section at kind `0x010a`, ordered after `0x0109`. Add a helper that can replace it with caller-supplied raw records for later tests. Ensure module semantic hashing sees the section.

Run:

```sh
cargo test --locked --offline decode::tests::utf16_literal_section -- --nocapture
cargo test --locked --offline bounded_failures -- --nocapture
```

Expected: section-contract tests pass; the independent limit test may remain red until Task 3 parses literal contents.

- [x] **Step 5: Commit the section skeleton**

Run:

```sh
git add src/artifact/format.rs src/limits.rs src/verify/modules.rs \
  src/decode/tests.rs tests/bounded_failures.rs tests/support/mod.rs
git commit -m "feat(format): reserve UTF-16 literal pool (#41)"
```

## Task 3: Decode and verify bounded UTF-16 literal pools

**Files:**

- Modify: `src/artifact/mod.rs`
- Modify: `src/decode/records.rs`
- Modify: `src/decode/tests.rs`
- Modify: `tests/bounded_failures.rs`
- Modify: `tests/support/mod.rs`

- [x] **Step 1: Add failing literal-pool vectors**

Add public-path verification tests for records containing:

```rust
&[]                                      // empty literal
&[0x00, 0x00]                            // embedded NUL
&[0x3d, 0xd8, 0x80, 0xde]                // U+1F680 surrogate pair as UTF-16LE
&[0x00, 0xd8]                            // isolated high surrogate
&[0x00, 0xdc]                            // isolated low surrogate
```

Also test odd byte length, duplicate records, non-increasing raw byte order, and a cumulative code-unit count one above the configured limit. Sort valid multi-record vectors by exact encoded bytes, not decoded numeric values.

Run:

```sh
cargo test --locked --offline decode::tests::utf16_literal -- --nocapture
cargo test --test bounded_failures --locked --offline utf16_literal_code_unit_limit_is_enforced -- --nocapture
```

Expected: FAIL because decoded modules do not publish or validate the pool.

- [x] **Step 2: Add distinct literal identity and storage**

In `src/artifact/mod.rs`, add:

```rust
id_type!(Utf16LiteralId);
```

Add `utf16_literals: Vec<ByteRange>` to `DecodedModule`, and change only the string constant payload type:

```rust
String(Utf16LiteralId),
```

Do not decode records into owned `Vec<u16>` values. Their `ByteRange`s must continue to point into `DecodedArtifact.bytes`.

- [x] **Step 3: Validate and collect literal records before publishing a module**

In `src/decode/records.rs`, find and decode `format::UTF16_LITERALS` for each module. Validate each record before `collect_ranges`:

```rust
if record.len() % 2 != 0 {
    return Err(/* BadRecord: UTF-16 literal has odd byte length */);
}
let code_units = record.len() / 2;
add_to_limit(
    &mut total_utf16_literal_code_units,
    code_units,
    limits.utf16_literal_code_units,
    limits,
    "total UTF-16 literal code-unit limit exceeded",
)?;
```

Use the existing raw-record ordering helper to require strict byte ordering and uniqueness. Empty records are valid, including in a non-empty pool. Keep `decode_string_table` unchanged and strict UTF-8.

- [x] **Step 4: Range-check string constant IDs against the new pool**

Parse tag `6` as:

```rust
Constant::String(Utf16LiteralId(ru32(cursor)?))
```

Replace metadata-string validation for these constants with a direct bounds check against `module.utf16_literals`. Do not pass a `Utf16LiteralId` to the UTF-8 `string(...)` helper.

Run:

```sh
cargo test --locked --offline decode::tests::utf16_literal -- --nocapture
cargo test --test bounded_failures --locked --offline utf16_literal_code_unit_limit_is_enforced -- --nocapture
```

Expected: PASS for all valid code-unit sequences and deterministic `BadRecord`/`LimitExceeded` for invalid structure or bounds.

- [x] **Step 5: Commit bounded literal decoding**

Run:

```sh
git add src/artifact/mod.rs src/decode/records.rs src/decode/tests.rs \
  tests/bounded_failures.rs tests/support/mod.rs
git commit -m "feat(format): verify UTF-16 literal pools (#41)"
```

## Task 4: Correct the CHAR constant wire representation

**Files:**

- Modify: `src/artifact/mod.rs`
- Modify: `src/decode/records.rs`
- Modify: `src/test_encode.rs`

- [x] **Step 1: Add exact failing record tests**

In the record decoder and test encoder tests, cover all boundary values:

```rust
[0x0000_u16, 0xd7ff, 0xd800, 0xdfff, 0xe000, 0xffff]
```

Assert canonical encoded bytes are exactly `[5, low, high]`. Add malformed records `[5, low]` and `[5, low, high, 0]`; both must fail `BadRecord` through the decoder.

Run:

```sh
cargo test --locked --offline decode::records::tests::char -- --nocapture
cargo test --locked --offline test_encode::tests::constant -- --nocapture
```

Expected: FAIL because tag `5` still reads and writes a `u32` scalar.

- [x] **Step 2: Replace Rust `char` in the artifact model**

Change:

```rust
Char(u16),
```

Decode with `ru16(cursor)?`; remove `char::from_u32` and the Unicode-scalar diagnostic. Encode with `u16le`. Keep `finish(cursor, record, true)` so both short and trailing-byte records are rejected.

Update string encoding to write `Utf16LiteralId.0`:

```rust
Constant::String(value) => {
    bytes.push(6);
    u32le(&mut bytes, value.0);
}
```

- [x] **Step 3: Make the test encoder preserve the literal pool**

In `encode_module`, copy each `module.utf16_literals` range from the immutable artifact and emit `format::UTF16_LITERALS` after `EXCEPTIONS`. Its record count and bytes participate in `semantic_hash` exactly like every other semantic indexed section.

Run:

```sh
cargo test --locked --offline decode::records::tests::char -- --nocapture
cargo test --locked --offline test_encode::tests -- --nocapture
```

Expected: PASS, including exact three-byte records, isolated surrogates, and byte-for-byte re-encoding of literal records.

- [x] **Step 4: Commit the corrected records**

Run:

```sh
git add src/artifact/mod.rs src/decode/records.rs src/test_encode.rs
git commit -m "fix(format): encode Kotlin Char as u16 (#41)"
```

## Task 5: Regenerate and review artifact golden fixtures

**Files:**

- Modify: `tests/fixtures/*.cpkt`
- Modify: `tests/fixtures/*.manifest.md`
- Modify as needed: `tests/golden_fixtures.rs`
- Modify as needed: `tests/support/mod.rs`

- [x] **Step 1: Observe the stale-fixture failure**

Run:

```sh
cargo test --test golden_fixtures --locked --offline
```

Expected: FAIL because committed pre-correction artifacts omit required section `0x010a` and their hashes no longer match.

- [x] **Step 2: Regenerate only through the committed generator**

Run:

```sh
cargo test --test golden_fixtures regenerate_committed_fixtures --locked --offline -- --ignored --exact
```

Expected: PASS and rewrite the `.cpkt`/manifest pairs.

- [x] **Step 3: Review deterministic changes**

Run:

```sh
git diff --stat
git diff -- tests/fixtures/*.manifest.md
cargo test --test golden_fixtures --locked --offline
```

Expected: every module manifest contains required semantic `0x010a` after `0x0109`; offsets, module hashes, and content hashes change consistently; all fixture tests pass.

- [x] **Step 4: Commit regenerated artifacts**

Run:

```sh
git add tests/fixtures tests/golden_fixtures.rs tests/support/mod.rs
git commit -m "test(format): regenerate corrected artifact v1 fixtures (#41)"
```

## Task 6: Make runtime Char a total `u16` value

**Files:**

- Modify: `src/execution/value.rs`
- Modify: `src/execution/numeric.rs`
- Modify: `src/execution/error.rs`
- Modify: `src/execution/machine.rs`
- Modify: `src/execution/image.rs`

- [x] **Step 1: Add failing conversion tests**

Replace scalar-validation tests with exact Kotlin truncation vectors:

```rust
assert_eq!(0xffff, i32_to_char(-1));
assert_eq!(0xffff, i32_to_char(65_535));
assert_eq!(0x0000, i32_to_char(65_536));
assert_eq!(0x0000, i32_to_char(i32::MIN));
assert_eq!(0xffff, i32_to_char(i32::MAX));
assert_eq!(0xd800, i32_to_char(0xd800));
assert_eq!(0xd800, char_to_i32(0xd800));
```

Run:

```sh
cargo test --locked --offline execution::numeric::tests -- --nocapture
```

Expected: FAIL because conversions currently return `Result<char, InvalidCharacter>`.

- [x] **Step 2: Implement total conversions and remove the obsolete trap**

Use:

```rust
pub(super) fn i32_to_char(value: i32) -> u16 {
    value as u16
}

pub(super) fn char_to_i32(value: u16) -> i32 {
    i32::from(value)
}
```

Change `RuntimeValue::Char(char)` to `RuntimeValue::Char(u16)`. Remove `GuestTrap::InvalidCharacter`. In `machine.rs`, remove error mapping for `i32 -> char`; the conversion always publishes a value. Preserve the existing read-before-write behavior for aliased registers.

- [x] **Step 3: Make trace representation exactly two payload bytes**

Keep the existing `Char` discriminant, return payload length `2`, and serialize `value.to_le_bytes()` without a `u32` widening. `trace_bits_u64` may zero-extend the `u16` for internal comparisons, but serialized trace bytes must not.

Run:

```sh
cargo test --locked --offline execution::numeric::tests -- --nocapture
cargo test --locked --offline execution::value::tests -- --nocapture
cargo test --locked --offline execution::image::tests -- --nocapture
```

Expected: PASS with all 65,536 code units representable and no invalid-character guest path.

- [x] **Step 4: Commit runtime semantics**

Run:

```sh
git add src/execution/value.rs src/execution/numeric.rs src/execution/error.rs \
  src/execution/machine.rs src/execution/image.rs
git commit -m "fix(vm): match Kotlin Char conversion semantics (#41)"
```

## Task 7: Lock execution conformance and trace digests

**Files:**

- Modify: `src/execution/fixtures.rs`
- Modify: `src/execution/tests.rs`
- Modify as needed: `src/verify/tests.rs`

- [x] **Step 1: Replace the invalid-character vector**

Remove the vector expecting `GuestTrap::InvalidCharacter`. Add executable vectors for `-1`, `65535`, `65536`, `0xd800`, and a round trip `i32 -> char -> i32`. Use `u16` expectations and ensure the verifier still rejects every conversion pair except `i32 <-> char`.

Run:

```sh
cargo test --locked --offline execution::tests::scalar_vectors_match_kotlin_jvm_semantics -- --nocapture
cargo test --locked --offline verify::tests::cfg_accepts_i32_char_conversions -- --nocapture
```

Expected: the semantic vector passes after updating expected values; old trace digest assertions fail because Char payloads shrink from four bytes to two.

- [x] **Step 2: Update trace assertions from observed canonical output**

Run the focused trace test with `--nocapture`, capture the digest produced by the corrected two-byte encoder, update only the corresponding expected digests, then prove debug/release agreement:

```sh
cargo test --locked --offline execution::tests::block_boundary_trace_digests_are_stable -- --nocapture
cargo test --release --locked --offline execution::tests::block_boundary_trace_digests_are_stable -- --nocapture
```

Expected: both modes pass with identical canonical digests.

- [x] **Step 3: Recheck allocation and execution determinism**

Run:

```sh
cargo test --locked --offline execution::tests -- --nocapture --test-threads=1
cargo test --release --locked --offline execution::tests::scalar_control_steady_state_allocates_nothing -- --nocapture --test-threads=1
```

Expected: PASS; no steady-state allocations are introduced.

- [x] **Step 4: Commit conformance changes**

Run:

```sh
git add src/execution/fixtures.rs src/execution/tests.rs src/verify/tests.rs
git commit -m "test(vm): lock UTF-16 Char conformance (#41)"
```

## Task 8: Reconcile normative and historical documentation

**Files:**

- Modify: `docs/superpowers/specs/2026-08-21-issue-36-compukter-artifact-bytecode-v1-design.md`
- Modify: `docs/superpowers/specs/2026-08-22-issue-38-deterministic-tier0-execution-semantics-design.md`
- Modify: `docs/superpowers/plans/2026-08-22-issue-39-tier0-scalar-control-interpreter.md`
- Modify as needed: `README.md`

- [x] **Step 1: Find all superseded claims**

Run:

```sh
rg -n "Unicode scalar|InvalidCharacter|Char\(char\)|CHAR|STRING|STRINGS|0x0109|four-byte|4-byte" README.md docs src tests
```

Expected: identify every statement that still treats Kotlin `Char` as a scalar, uses a four-byte CHAR payload, or sends guest string constants to metadata `STRINGS`.

- [x] **Step 2: Update the artifact and execution contracts**

In the #36 design, add `0x010a UTF16_LITERALS`, its indexed-record rules, the independent code-unit limit, the `Utf16LiteralId` payload, and three-byte CHAR record. In the #38 design, state low-16-bit truncation, zero extension, total conversion, and two-byte trace payload. In the completed #39 plan, annotate obsolete implementation snippets as superseded by issue #41 rather than leaving them as current guidance.

Do not rewrite history to claim #39 initially implemented the corrected contract. Link the accepted #41 design wherever a prior statement is retained for historical context.

- [x] **Step 3: Prove no active documentation teaches the old ABI**

Run:

```sh
rg -n "invalid Unicode scalar|InvalidCharacter|CHAR.*u32|StringId.*constant|Char\(char\)" README.md docs
git diff --check
```

Expected: no unqualified active contract remains; any match is explicitly marked superseded by #41.

- [x] **Step 4: Commit documentation reconciliation**

Run:

```sh
git add README.md docs/superpowers/specs docs/superpowers/plans
git commit -m "docs(vm): align v1 text semantics with Kotlin (#41)"
```

## Task 9: Run the complete acceptance gate and finish the roadmap item

**Files:**

- Modify only if a gate exposes a defect: files already owned by Tasks 2-8
- Update: `docs/superpowers/plans/2026-08-22-issue-41-kotlin-char-utf16-literals.md` checkbox state

- [x] **Step 1: Run formatting, lint, and all debug tests**

Run:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked --offline -- -D warnings
cargo test --locked --offline
cargo test --test golden_fixtures --locked --offline
```

Expected: all commands pass with no warnings.

- [x] **Step 2: Run release-mode safety and semantic gates**

Run:

```sh
cargo test --release --test bounded_failures --locked --offline
cargo test --release --locked --offline execution::tests -- --nocapture --test-threads=1
```

Expected: PASS with the same Kotlin Char results and trace digests as debug mode.

- [x] **Step 3: Inspect the public API boundary**

Run:

```sh
cargo doc --no-deps --locked --offline
rg -n "Utf16LiteralId|RuntimeValue|DecodedModule|UTF16_LITERALS" target/doc/compukter_vm
```

Expected: documentation succeeds and `rg` finds no newly public internal representation. `ArtifactLimits::utf16_literal_code_units` is the only intended new public item.

- [x] **Step 4: Verify the diff and commit plan completion**

Mark completed checkboxes only after their commands passed, then run:

```sh
git diff --check
git status --short
git diff --stat HEAD~7..HEAD
git add docs/superpowers/plans/2026-08-22-issue-41-kotlin-char-utf16-literals.md
git commit -m "docs(vm): record issue 41 verification (#41)"
git push origin main
```

Expected: clean worktree after push and all issue #41 commits present on `origin/main`.

- [x] **Step 5: Close #41 and unblock #40**

Using `gh` outside the sandbox, comment on #41 with the commit range and exact verification commands, close it as completed, and set its project item to `Done`. Move #40 from `Next` to `Now`, since the text ABI prerequisite is now fixed. Confirm both issue and project state with read-only `gh` queries.

Expected: #41 is closed/completed and `Done`; #40 remains open and is `Now`.
