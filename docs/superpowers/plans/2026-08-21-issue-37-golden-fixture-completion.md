# Artifact v1 Golden Fixture Completion Implementation Plan

> Issue: [#37](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/37)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Commit the complete artifact v1 binary golden set, prove every golden is deterministically reproducible, and prove decoded known-v1 artifacts canonically re-encode byte-for-byte.

**Architecture:** Explicit test-only builders produce independent canonical bytes and Markdown manifests under `tests/fixtures/`. A separate crate-private test encoder serializes the production decoder's semantic model without copying original section payloads, providing the opposite side of the round-trip proof. Normal tests are read-only; one ignored test is the only intentional golden regeneration path.

**Tech Stack:** Rust 2021, `sha2` without default features, standard-library filesystem APIs in one ignored test, existing decoder/verifier and integration-test support.

---

## File map

- `tests/fixtures/*.cpkt` — committed executable artifact v1 goldens.
- `tests/fixtures/*.manifest.md` — exact directory, record, and hash manifests.
- `tests/fixtures/host-runtime.code` — indexed CODE envelope for coroutine and capability instruction records.
- `tests/golden_fixtures.rs` — read-only committed/generated equality, public verification, manifest, and ignored regeneration tests.
- `tests/support/mod.rs` — independent explicit fixture builders and raw-byte manifest renderer.
- `src/test_encode.rs` — crate-private, test-only canonical encoder from `DecodedArtifact`.
- `src/lib.rs` — enables `test_encode` only under `cfg(test)`.
- `src/decode/tests.rs` — private decoded feature-presence and round-trip assertions.
- `README.md` — documents golden consumption and explicit regeneration.
- `.github/workflows/ci.yml` — runs the read-only golden suite; never regenerates files.

### Task 1: Commit reproducible base artifact goldens

**Files:**
- Create: `tests/golden_fixtures.rs`
- Create: `tests/fixtures/vector-a.cpkt`
- Create: `tests/fixtures/vector-a.manifest.md`
- Create: `tests/fixtures/two-module.cpkt`
- Create: `tests/fixtures/two-module.manifest.md`
- Modify: `tests/support/mod.rs`

- [ ] **Step 1: Add a failing read-only committed-byte test**

Create `tests/golden_fixtures.rs` with a repository-relative reader and require the two existing independent builders to equal committed files:

```rust
#[allow(dead_code)]
mod support;

use std::{fs, path::PathBuf, sync::Arc};

use compukter_vm::{verify_artifact, ArtifactLimits};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn base_artifact_goldens_are_committed_and_reproducible() {
    for (name, generated, modules) in [
        ("vector-a", support::minimal_vector(), 1),
        ("two-module", support::two_module_vector(), 2),
    ] {
        let committed = fs::read(fixture(&format!("{name}.cpkt"))).unwrap();
        assert_eq!(committed, generated, "{name} bytes changed");
        let verified = verify_artifact(Arc::from(committed), ArtifactLimits::default()).unwrap();
        assert_eq!(verified.module_count(), modules);
    }
}
```

- [ ] **Step 2: Run the test and verify the missing-file failure**

Run: `cargo test --test golden_fixtures base_artifact_goldens_are_committed_and_reproducible --locked --offline -- --exact`

Expected: FAIL because `tests/fixtures/vector-a.cpkt` is absent.

- [ ] **Step 3: Add deterministic artifact manifest rendering**

In `tests/support/mod.rs`, add `artifact_manifest(name, bytes) -> String`. It reads fixed header and directory fields with checked slice conversions, renders lowercase 64-character SHA-256 values, and walks every non-MANIFEST indexed envelope to produce this stable schema:

```text
# <name>

- file length: <decimal>
- payload end: <decimal>
- semantic features: 0x<8 hex digits>
- artifact sha256: `<64 lowercase hex digits>`

| Kind | Scope | Offset | Length | Count | Record offsets |
|---:|---:|---:|---:|---:|---|
| `0x0001` | 0 | 480 | 112 | 1 | fixed |
| `0x0002` | 0 | 592 | 84 | 1 | `616:60` |

## Module semantic hashes

- module 0: `<64 lowercase hex digits>`
```

The record offset list uses absolute byte offsets and exact record lengths. Read module hashes from decoded MODULE record bytes, but calculate the artifact hash independently with `Sha256` and assert it equals the trailer before rendering.

- [ ] **Step 4: Add the explicit ignored regeneration path**

Add this ignored test and keep all ordinary tests read-only:

```rust
#[test]
#[ignore = "explicitly rewrites committed golden fixtures"]
fn regenerate_committed_fixtures() {
    fs::create_dir_all(fixture("")).unwrap();
    for (name, bytes) in [
        ("vector-a", support::minimal_vector()),
        ("two-module", support::two_module_vector()),
    ] {
        fs::write(fixture(&format!("{name}.cpkt")), &bytes).unwrap();
        fs::write(
            fixture(&format!("{name}.manifest.md")),
            support::artifact_manifest(name, &bytes),
        )
        .unwrap();
    }
}
```

Extend the ordinary base test to compare the committed manifest with `artifact_manifest`.

- [ ] **Step 5: Generate, inspect, and verify the base goldens**

Run: `cargo test --test golden_fixtures regenerate_committed_fixtures --locked --offline -- --ignored --exact`

Expected: PASS and create four files under `tests/fixtures/`.

Run: `cargo test --test golden_fixtures --locked --offline`

Expected: PASS; the ignored regeneration test is not executed.

Inspect both manifests and confirm Vector A retains length `1088`, payload end `1056`, module hash `f73d8f8699e060aac0df1079d820a9fd778a649dd391980c23ee2a4e3c17c2cc`, and artifact hash `88803a07260a3b0123ef230b482a682400e6cae03e90f3be0a117419406509d3`.

- [ ] **Step 6: Commit the base goldens**

```bash
git add tests/golden_fixtures.rs tests/support/mod.rs tests/fixtures
git commit -m "test(vm): commit base artifact v1 goldens (#37)"
```

### Task 2: Commit the language/runtime and debug artifacts

**Files:**
- Modify: `tests/support/mod.rs`
- Modify: `tests/golden_fixtures.rs`
- Modify: `src/decode/tests.rs`
- Create: `tests/fixtures/language-runtime.cpkt`
- Create: `tests/fixtures/language-runtime.manifest.md`
- Create: `tests/fixtures/debug.cpkt`
- Create: `tests/fixtures/debug.manifest.md`

- [ ] **Step 1: Add failing public and private feature tests**

Extend the integration case table with `support::language_runtime_vector()` and `support::debug_vector()`. In `src/decode/tests.rs`, decode the committed files and require these exact semantic shapes:

```rust
#[test]
fn records_decode_language_runtime_golden_features() {
    let artifact = decoded_fixture("language-runtime.cpkt");
    let module = &artifact.modules[0];
    assert!(module.types.iter().any(|value| matches!(value, NominalType::Class { .. })));
    assert!(module.types.iter().any(|value| matches!(value, NominalType::Array { .. })));
    assert!(module.functions[0].registers.iter().any(|value| value.kind == 7 && value.flags == 1));
    let instructions = module.code.iter().flat_map(|code| code.instructions.iter());
    assert!(instructions.clone().any(|value| matches!(value, Instruction::NewObject { .. })));
    assert!(instructions.clone().any(|value| matches!(value, Instruction::NewArray { .. })));
    assert!(instructions.clone().any(|value| matches!(value, Instruction::ArrayLoad { .. } | Instruction::ArrayStore { .. })));
    assert!(instructions.any(|value| matches!(value, Instruction::Branch { .. })));
    assert!(module.blocks.iter().any(|block| block.flags & 1 != 0));
    assert_eq!(module.exceptions.len(), 1);
}

#[test]
fn records_decode_debug_golden_inline_ancestry() {
    let artifact = decoded_fixture("debug.cpkt");
    let debug = &artifact.modules[0].debug;
    assert_eq!(debug.len(), 2);
    assert_eq!(debug[0].inline_parent, u32::MAX);
    assert_eq!(debug[1].inline_parent, 0);
    assert!(debug[0].end_utf16 > debug[0].start_utf16);
    assert!(debug[1].end_utf16 > debug[1].start_utf16);
}
```

Implement `decoded_fixture` as a small filename match over the two fixed `include_bytes!` values; do not accept arbitrary paths and do not add a production method solely for tests.

- [ ] **Step 2: Run and verify the missing builder/fixture failure**

Run: `cargo test records_decode_language_runtime_golden_features --locked --offline`

Expected: FAIL because the language/runtime builder and file do not exist.

- [ ] **Step 3: Build the valid language/runtime artifact explicitly**

Add `language_runtime_vector()` using the existing fixed-width/indexed helpers. Its one function has eight registers with static types:

```text
r0: non-null ref Class(0)
r1: nullable ref Class(0)
r2: bool
r3: i32 array length/value
r4: non-null ref Array(1)
r5: i32 loaded value
r6: non-null ref Class(0), exception value
r7: i32 array index
```

Use types `0 = concrete root class`, `1 = i32 array`, and `2 = () -> unit`. Use sorted strings `Box`, `app`, `array`, `entry` and sorted constants `I32(0)`, `I32(1)`. Encode five blocks:

```text
b0 cost 8: new_object r0,type0; null r1; is_type r2,r1,type0; branch r2,b1,b2
b1 cost 11: const r3,c1; const r7,c0; new_array r4,type1,r3;
            array_store r4,r7,r3; array_load r5,r4,r7; jump b3
b2 cost 6: new_object r6,type0; throw r6
b3 cost 1, loop-header flag: jump b3
b4 cost 1: return unit
```

Protect exactly `b2` with catch type 0, handler `b4`, and exception register `r6`. Set manifest maximum block cost and minimum slice cost to 11 and semantic feature bit 0. The module record declares three types, one function, five blocks through the function range, and one exception.

- [ ] **Step 4: Build the valid debug artifact explicitly**

Add `debug_vector()` as a one-module application with one block containing `nop; return unit`, declared cost 2, and an optional zero-flag DEBUG section. Encode two ordered debug records for `src/main.kts`:

```text
record 0: function 0, block 0, instruction 0, UTF-16 [0, 5), parent absent
record 1: function 0, block 0, instruction 1, UTF-16 [6, 12), parent 0
```

Do not include DEBUG in the module semantic hash and keep semantic feature bits zero.

- [ ] **Step 5: Extend regeneration and produce committed files**

Add both builders to the ordinary case table and ignored regeneration table. Run:

`cargo test --test golden_fixtures regenerate_committed_fixtures --locked --offline -- --ignored --exact`

Expected: PASS and create both `.cpkt` files and manifests.

- [ ] **Step 6: Run public and private golden verification**

Run: `cargo test golden --locked --offline`

Expected: all golden equality, public verification, and decoded feature tests PASS.

- [ ] **Step 7: Commit the language/runtime and debug goldens**

```bash
git add tests/support/mod.rs tests/golden_fixtures.rs src/decode/tests.rs tests/fixtures
git commit -m "test(vm): add language and debug artifact goldens (#37)"
```

### Task 3: Commit the host-runtime instruction record golden

**Files:**
- Modify: `tests/support/mod.rs`
- Modify: `tests/golden_fixtures.rs`
- Modify: `src/decode/tests.rs`
- Create: `tests/fixtures/host-runtime.code`
- Create: `tests/fixtures/host-runtime.manifest.md`

- [ ] **Step 1: Add a failing exact instruction-record test**

Add a crate-private test that reads the committed indexed envelope, obtains its five records, decodes them with the production instruction decoder, and asserts this sequence:

```text
record 0: coroutine_spawn r0,function0,[]; return unit
record 1: sleep r0,resume0
record 2: coroutine_join unit,r0,resume0
record 3: cap_call_sync unit,capability0,operation0,[]; return unit
record 4: cap_call_async unit,capability0,operation1,[],resume0
```

Assert recalculated record costs `[7, 3, 4, 6, 6]` and terminator placement. The spawn and synchronous-call records contain a trailing return because those operations are not terminators.

- [ ] **Step 2: Run and verify the missing-file failure**

Run: `cargo test instruction_decodes_host_runtime_golden --locked --offline`

Expected: FAIL because `host-runtime.code` does not exist.

- [ ] **Step 3: Add the independent code-envelope builder and manifest**

Add `host_runtime_code() -> Vec<u8>` that frames the operands above using canonical ULEB128 zero IDs/counts and wraps the five records in `indexed`. Add `code_manifest(name, bytes) -> String` that renders envelope length and, for each instruction, record ID, absolute offset, opcode, form, byte length, and fixed cost.

Extend the ignored regeneration test to write `host-runtime.code` and its manifest. Extend the ordinary test to compare both generated values to the committed values.

- [ ] **Step 4: Generate and verify the instruction golden**

Run: `cargo test --test golden_fixtures regenerate_committed_fixtures --locked --offline -- --ignored --exact`

Run: `cargo test instruction_decodes_host_runtime_golden --locked --offline`

Expected: both PASS and the manifest lists all five records and seven framed instructions.

- [ ] **Step 5: Commit the host-runtime golden**

```bash
git add tests/support/mod.rs tests/golden_fixtures.rs src/decode/tests.rs tests/fixtures
git commit -m "test(vm): add host runtime instruction golden (#37)"
```

### Task 4: Canonically re-encode decoded records and containers

**Files:**
- Create: `src/test_encode.rs`
- Modify: `src/lib.rs`
- Modify: `src/decode/tests.rs`

- [ ] **Step 1: Add a failing Vector A decoded round-trip test**

Enable `mod test_encode` only under `cfg(test)`. Add:

```rust
#[test]
fn records_reencode_vector_a_byte_for_byte() {
    let original = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/vector-a.cpkt"
    ));
    let decoded = super::records::decode_artifact(
        Arc::from(original.as_slice()),
        &ArtifactLimits::default(),
    )
    .unwrap();
    assert_eq!(crate::test_encode::encode_artifact(&decoded).unwrap(), original);
}
```

- [ ] **Step 2: Run and verify the missing encoder failure**

Run: `cargo test records_reencode_vector_a_byte_for_byte --locked --offline`

Expected: FAIL because `test_encode::encode_artifact` does not exist.

- [ ] **Step 3: Implement checked test-only container assembly**

Create `src/test_encode.rs` with a private `EncodeError(&'static str)`, checked `u32/u64/usize` conversions, fixed-width writers, canonical ULEB32, indexed-envelope construction, SHA-256 module hashing, aligned directory assembly, and final artifact digest. Return `Result<Vec<u8>, EncodeError>`; never panic on decoded counts.

Encode these global records from semantic fields: `Manifest`, `Capability`, and the module records. Recompute each module semantic hash from its ten semantic section payloads and require it to equal `module.semantic_hash` before writing MODULES.

Encode every module record shape from its decoded fields: strings by slicing their `ByteRange`, four nominal type variants, eight constant variants, imports, exports, fields, functions and register value types, blocks, exceptions, and debug records. Debug source paths may be read through their decoded `ByteRange`; no other section or record payload may be copied from `artifact.bytes`.

For this first green step, encode `Instruction::Return`, which is sufficient for Vector A. Any other instruction returns `EncodeError("instruction encoder is incomplete")` until Task 5.

- [ ] **Step 4: Verify Vector A and add record-shape round trips**

Run: `cargo test records_reencode_vector_a_byte_for_byte --locked --offline`

Expected: PASS.

Add a round trip for the committed two-module artifact; it proves import/export and multiple-scope assembly. Inside `test_encode.rs`, add exact unit tests for the record encoders using one value of every `NominalType` variant and every `Constant` variant. The nominal values are a root class, a parentless interface, an i32 array, and a zero-parameter unit function. The constants are `I32(-1)`, `I64(-2)`, raw `F32(0x3f80_0000)`, raw `F64(0x3ff0_0000_0000_0000)`, `Bool(true)`, `Char('x')`, `String(0)`, and `Null`. Assert their exact tag and little-endian payload bytes. Add exact record tests for one capability, import, export, field, function with one register, block, exception, and debug value using the same canonical field order specified by #36.

- [ ] **Step 5: Run focused encoder tests and lint**

Run: `cargo test reencode --locked --offline`

Run: `cargo clippy --lib --tests --all-features --locked --offline -- -D warnings`

Expected: PASS without permitting dead production code; the module is compiled only for tests.

- [ ] **Step 6: Commit record and container re-encoding**

```bash
git add src/lib.rs src/test_encode.rs src/decode/tests.rs
git commit -m "test(vm): reencode decoded artifact records (#37)"
```

### Task 5: Exhaustively re-encode all v1 instructions

**Files:**
- Modify: `src/test_encode.rs`
- Modify: `src/decode/tests.rs`

- [ ] **Step 1: Add a failing decoder/encoder identity test for all 52 opcodes**

Extract the existing `instruction_decodes_every_v1_opcode` case table into a crate-test helper returning the canonical framed records. For every case, decode it, encode each resulting `Instruction`, and require exact framed byte equality. Preserve the appended unit return for non-terminators.

Run: `cargo test instruction_reencodes_every_v1_opcode --locked --offline`

Expected: FAIL on opcode `0x00` because only return encoding exists.

- [ ] **Step 2: Implement exhaustive instruction encoding**

Implement one exhaustive `match Instruction` with the exact opcode/form mapping from the #36 table:

```text
Nop..Convert             00,01,02,03,04 / form 0
Add..Neg                 10..15 / stored form
BitAnd..ShiftUnsigned    16..1b / stored form
Equal..GreaterEqual      20..25 / stored form
RefEqual,RefNotEqual     26,27 / form 7
NewObject..CheckedCast   30..3a / form 0
CallDirect..CallInterface 40..42 / form 0
CoroutineSpawn,CapSync   50,51 / form 0
Jump..CapAsync           e0..e9 / form 0
Unreachable              ff / form 0
```

Registers are little-endian `u16`; absent optional registers retain `u16::MAX`. IDs, argument counts, switch counts, switch targets, and resume blocks use canonical ULEB32. Switch case values are little-endian `i32`. Frame `byte_length` is checked to fit `u16` before writing the four-byte header.

- [ ] **Step 3: Verify all opcode identities**

Run: `cargo test instruction_reencodes_every_v1_opcode --locked --offline`

Expected: all 52 opcode cases PASS byte-for-byte.

- [ ] **Step 4: Require every committed artifact to round-trip**

Add one table-driven crate test for `vector-a.cpkt`, `two-module.cpkt`, `language-runtime.cpkt`, and `debug.cpkt`. Decode with production code, encode with `test_encode`, and compare the full bytes. Assert the language/runtime artifact exercises instructions beyond Return so this test cannot pass through the earlier incomplete branch.

Run: `cargo test committed_artifacts_reencode_byte_for_byte --locked --offline`

Expected: all four artifacts PASS.

- [ ] **Step 5: Commit exhaustive instruction encoding**

```bash
git add src/test_encode.rs src/decode/tests.rs
git commit -m "test(vm): reencode every artifact v1 opcode (#37)"
```

### Task 6: Document and audit the completed golden contract

**Files:**
- Modify: `README.md`
- Modify: `.github/workflows/ci.yml`
- Modify: `docs/superpowers/specs/2026-08-21-issue-37-golden-fixture-completion-design.md` only if implementation names differ from the accepted design

- [ ] **Step 1: Document fixture use and regeneration**

Add a README section listing the five binary fixtures, stating that normal tests are read-only, and documenting the exact ignored regeneration command. State that regenerated binaries and manifests must be reviewed together.

Add an explicit CI step:

```yaml
- run: cargo test --test golden_fixtures --locked --offline
```

Do not add the ignored regeneration command to CI.

- [ ] **Step 2: Run formatting and static checks**

Run: `cargo fmt --all -- --check`

Run: `git diff --check`

Run: `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`

Expected: all PASS without warnings.

- [ ] **Step 3: Run complete debug and release verification**

Run: `cargo test --locked --offline`

Run: `cargo test --release --test bounded_failures --locked --offline`

Run: `cargo test --release --test golden_fixtures --locked --offline`

Expected: all unit, integration, doctest, mutation, manifest, feature-presence, and byte-round-trip tests PASS. The regeneration test remains ignored.

- [ ] **Step 4: Audit dependencies, documentation, and public surface**

Run: `cargo tree --locked --offline`

Expected: only `sha2` and its hashing primitives remain.

Run: `cargo doc --no-deps --document-private-items --locked --offline`

Expected: PASS; `test_encode` is absent from normal library documentation and no encoder becomes public.

Inspect `git status --short` and confirm no fixture changed during ordinary verification.

- [ ] **Step 5: Perform the final #36 coverage audit**

Compare the committed fixture manifests, record encoder matches, and the 52-opcode identity table against every section, record, value type, nominal type, constant, and opcode table in #36. Keep #37 open on any mismatch. Record these exact totals in the commit handoff:

```text
13 known section kinds
4 nominal type tags
8 value kinds
8 constant tags
52 opcodes
14 ArtifactLimits fields
5 committed binary fixture groups
```

- [ ] **Step 6: Commit documentation and CI coverage**

```bash
git add README.md .github/workflows/ci.yml docs/superpowers/specs/2026-08-21-issue-37-golden-fixture-completion-design.md
git commit -m "docs(vm): document artifact golden workflow (#37)"
```

## Completion checkpoint

From a clean `main`, rerun every Task 6 command and inspect `git status --short`. Do not close #37 or advance to execution/runtime work unless committed/generated byte equality, manifest equality, public verification, decoded feature presence, all known-record round trips, all 52 opcode identities, and all four `.cpkt` decoded round trips pass without rewriting repository files.
