# Tier 0 Scalar/Control Interpreter Implementation Plan

> Issue: [#39](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/39)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Build the crate-private, block-atomic Tier 0 semantic oracle for admitted scalar/control artifacts, including deterministic accounting, conformance traces, allocation checks, and reproducible performance workloads.

**Architecture:** Add a private `execution` subsystem beside decoding and verification. Admission lowers immutable `VerifiedArtifact` data into resolved execution metadata and reserves a fixed frame/register arena; `Machine` owns all mutable lifecycle and accounting state. Numeric helpers implement Kotlin/JVM behavior independently of Rust overflow mode, while unit-test-only fixtures exercise the private interpreter without exposing an incomplete public runtime API.

**Tech Stack:** Rust 2021, existing artifact v1 decoder/verifier, `sha2` for canonical trace digests, Rust unit tests, a test-only counting allocator, and release-mode ignored tests for performance reporting.

---

## File map

- Modify `src/lib.rs`: declare the private execution subsystem; do not re-export it.
- Modify `src/artifact/mod.rs`: provide the narrow crate-private decoded-artifact accessor needed by admission.
- Create `src/execution/mod.rs`: private module boundary and shared resolved ID types.
- Create `src/execution/error.rs`: bounded admission, host-call, guest-trap, VM-fault, outcome, and lifecycle types.
- Create `src/execution/value.rs`: fixed-size runtime values, typed entry arguments, opaque fixture references, and canonical trace encoding.
- Create `src/execution/numeric.rs`: Kotlin/JVM integer, float, conversion, and comparison semantics.
- Create `src/execution/image.rs`: immutable resolved execution metadata and portable frame-storage planning.
- Create `src/execution/machine.rs`: preallocated frames/registers, typed start, block-atomic slicing, dispatch, calls, control flow, outcomes, and trace accumulation.
- Create `src/execution/fixtures.rs`: test-only verified scalar/control artifact builder and workload definitions.
- Create `src/execution/tests.rs`: conformance, lifecycle, allocation, and release performance tests.

### Task 1: Establish the private execution boundary and bounded outcome vocabulary

**Files:**
- Modify: `src/lib.rs`
- Create: `src/execution/mod.rs`
- Create: `src/execution/error.rs`
- Create: `src/execution/value.rs`

- [x] **Step 1: Write compile-time boundary and value-shape tests**

Add unit tests at the bottom of `src/execution/error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_outcomes_are_stable_and_distinct() {
        let halted = Outcome::Halted(None);
        assert!(halted.is_terminal());
        assert!(Outcome::Crashed(GuestTrap::DivisionByZero).is_terminal());
        assert!(Outcome::Faulted(VmFault::ReachedUnreachable).is_terminal());
        assert!(!Outcome::SliceExhausted.is_terminal());
    }

    #[test]
    fn failures_have_bounded_scalar_payloads() {
        assert!(core::mem::size_of::<GuestTrap>() <= 8);
        assert!(core::mem::size_of::<AdmissionError>() <= 48);
        assert!(core::mem::size_of::<RunError>() <= 32);
    }
}
```

- [x] **Step 2: Run the focused test and verify the missing module failure**

Run: `cargo test --locked --offline execution::error::tests -- --nocapture`

Expected: FAIL because `execution` and its error types do not exist.

- [x] **Step 3: Add the private module and exact error/lifecycle types**

Add only `mod execution;` to `src/lib.rs`; do not add a `pub use`. Create `src/execution/mod.rs`:

```rust
mod error;
mod value;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct FunctionKey {
    pub module: u32,
    pub function: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct TypeKey {
    pub module: u32,
    pub ty: u32,
}
```

Create `src/execution/error.rs` with these non-string payloads:

```rust
use super::value::RuntimeValue;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdmissionError {
    CompilerAbiMismatch,
    StandardLibraryAbiMismatch,
    MissingCapability { index: u8 },
    HeapLimit { required: u32, available: u32 },
    FrameStorageLimit { required: u64, available: u64 },
    CallDepthLimit { required: u32, available: u32 },
    CoroutineLimit { required: u32, available: u32 },
    HostRequestLimit { required: u32, available: u32 },
    EventLimit { required: u32, available: u32 },
    SliceLimit { required: u32, available: u32 },
    StoragePlanOverflow,
    AllocationFailed,
    InvalidEntry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RunError {
    AlreadyStarted,
    NotStarted,
    NotRunnable,
    InvalidSliceBudget { minimum: u32, maximum: u32, supplied: u32 },
    EntryArity { expected: u16, supplied: u16 },
    EntryType { parameter: u16 },
    ForeignReference { parameter: u16 },
    DeadReference { parameter: u16 },
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GuestTrap {
    DivisionByZero,
    StackOverflow,
    InvalidCharacter,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VmFault {
    InvalidResolvedId,
    InvalidValueType,
    AccountingOverflow,
    InvalidStoragePlan,
    CorruptLifecycle,
    ReachedUnreachable,
    UnsupportedInstruction,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Outcome {
    SliceExhausted,
    Halted(Option<RuntimeValue>),
    Crashed(GuestTrap),
    Faulted(VmFault),
}

impl Outcome {
    pub(super) fn is_terminal(self) -> bool {
        !matches!(self, Self::SliceExhausted)
    }
}
```

Create `src/execution/value.rs` with the initial value shape required by
`Outcome`; Task 2 fills in trace encoding and reference metadata:

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RuntimeValue {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Bool(bool),
    Char(char),
    Null,
}
```

`UnsupportedInstruction` is an internal invariant guard only: admission in Task 3 rejects an image containing any opcode outside this issue before a `Machine` can be constructed. It is never a public or guest-visible “unimplemented” result.

- [x] **Step 4: Run the focused test**

Run: `cargo test --locked --offline execution::error::tests -- --nocapture`

Expected: PASS, 2 tests.

- [x] **Step 5: Confirm no execution API escaped the crate**

Run: `cargo doc --no-deps --locked --offline`

Expected: PASS; generated public docs list `verify_artifact`, artifact limits, diagnostics, `EntryPoint`, and `VerifiedArtifact`, but no execution type.

- [x] **Step 6: Commit the boundary**

```bash
git add src/lib.rs src/execution/mod.rs src/execution/error.rs src/execution/value.rs
git commit -m "feat(vm): define private Tier 0 execution boundary (#39)"
```

### Task 2: Implement fixed-size values and Kotlin/JVM numeric semantics

**Files:**
- Modify: `src/execution/mod.rs`
- Modify: `src/execution/value.rs`
- Create: `src/execution/numeric.rs`
- Modify: `src/verify/functions.rs`
- Modify: `src/verify/tests.rs`

- [x] **Step 1: Write exhaustive numeric table tests**

In `src/execution/numeric.rs`, add table-driven tests covering both widths and all special cases:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_wrap_mask_shifts_and_handle_min_division() {
        assert_eq!(i32::MAX.wrapping_add(1), add_i32(i32::MAX, 1));
        assert_eq!(i64::MIN, neg_i64(i64::MIN));
        assert_eq!(1, shl_i32(1, 32));
        assert_eq!(-1, shr_i64(-1, 65));
        assert_eq!(Ok(i32::MIN), div_i32(i32::MIN, -1));
        assert_eq!(Ok(0), rem_i64(i64::MIN, -1));
        assert_eq!(Err(GuestTrap::DivisionByZero), div_i64(1, 0));
    }

    #[test]
    fn produced_nans_are_canonical_and_constants_are_not_rewritten() {
        assert_eq!(CANONICAL_F32_NAN, canonical_f32(f32::NAN).to_bits());
        assert_eq!(CANONICAL_F64_NAN, rem_f64(f64::INFINITY, 1.0).to_bits());
        assert_eq!(0x7fa0_0001, RuntimeValue::F32(0x7fa0_0001).trace_bits_u64() as u32);
    }

    #[test]
    fn float_to_integer_matches_jvm_truncation_and_saturation() {
        assert_eq!(0, f64_to_i32(f64::NAN));
        assert_eq!(i32::MAX, f64_to_i32(f64::INFINITY));
        assert_eq!(i32::MIN, f64_to_i32(f64::NEG_INFINITY));
        assert_eq!(3, f64_to_i32(3.99));
        assert_eq!(-3, f64_to_i64(-3.99));
    }

    #[test]
    fn primitive_float_comparison_uses_kotlin_rules() {
        assert!(!eq_f64(f64::NAN, f64::NAN));
        assert!(eq_f32(-0.0, 0.0));
        assert!(!lt_f64(f64::NAN, 1.0));
        assert!(ne_f64(f64::NAN, f64::NAN));
    }
}
```

- [x] **Step 2: Run the numeric tests and verify they fail**

Run: `cargo test --locked --offline execution::numeric::tests -- --nocapture`

Expected: FAIL with missing constants/functions and `RuntimeValue`.

- [x] **Step 2a: Write and run verifier tests for character conversions**

Add verifier cases that accept `Convert` from `i32` to `char` and from `char`
to `i32`, while still rejecting `char` conversions involving `i64`, `f32`, or
`f64`.

Run: `cargo test --locked --offline verify::tests::cfg_accepts_i32_char_conversions -- --nocapture`

Expected: FAIL with `Code::BadType` because the current verifier limits
`Convert` to kinds `1..=4`.

- [x] **Step 3: Implement the fixed-size runtime value model**

Add `mod numeric;` to `src/execution/mod.rs`. Replace the initial enum in
`src/execution/value.rs` with:

```rust
use super::TypeKey;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReferenceValue {
    pub image: [u8; 32],
    pub ty: TypeKey,
    pub handle: u32,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RuntimeValue {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Bool(bool),
    Char(char),
    Null,
    Reference(ReferenceValue),
}

impl RuntimeValue {
    pub(super) fn trace_bits_u64(self) -> u64 {
        match self {
            Self::I32(value) => value as u32 as u64,
            Self::I64(value) => value as u64,
            Self::F32(bits) => bits as u64,
            Self::F64(bits) => bits,
            Self::Bool(value) => u64::from(value),
            Self::Char(value) => value as u32 as u64,
            Self::Null => 0,
            Self::Reference(value) => value.handle as u64,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct EntryArgument(pub RuntimeValue);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RegisterValue {
    Uninitialized,
    Initialized(RuntimeValue),
}
```

Keep reference identity comparison on `(image, handle, generation)`, never on a Rust address or integer conversion.

- [x] **Step 4: Implement numeric helpers without unchecked Rust arithmetic**

In `src/execution/numeric.rs`, define `CANONICAL_F32_NAN = 0x7fc0_0000` and `CANONICAL_F64_NAN = 0x7ff8_0000_0000_0000`. Implement integer helpers exclusively with `wrapping_*`, shift counts `& 31`/`& 63`, explicit zero-divisor checks, and explicit `MIN / -1` branches. Implement float arithmetic by converting raw bits to `f32`/`f64`, applying one operation, and passing every produced result through:

```rust
pub(super) fn canonical_f32(value: f32) -> f32 {
    if value.is_nan() { f32::from_bits(CANONICAL_F32_NAN) } else { value }
}

pub(super) fn canonical_f64(value: f64) -> f64 {
    if value.is_nan() { f64::from_bits(CANONICAL_F64_NAN) } else { value }
}
```

Implement remainder with Rust's primitive `%` operation, whose quotient is
truncated toward zero, followed by canonical NaN normalization; cover NaN,
infinite dividend, zero divisor, and finite-modulo-infinity explicitly in the
tests. Implement all `i32/i64/f32/f64` conversions, including NaN-to-zero and
saturation, as named functions so dispatch never relies on ambiguous `as`
casts. Add checked `i32_to_char` and lossless `char_to_i32` helpers. Narrow the
verifier rule to accept the existing numeric pairs plus exactly `i32 -> char`
and `char -> i32`; do not alter encoding or fixed cost. Implement equality and
ordered comparison as primitive Rust float comparisons, then canonicalize only
values, not boolean results.

- [x] **Step 5: Run numeric tests in debug and release modes**

Run: `cargo test --locked --offline execution::numeric::tests -- --nocapture`

Expected: PASS, 4 tests.

Run: `cargo test --release --locked --offline execution::numeric::tests -- --nocapture`

Expected: PASS with identical assertions.

- [x] **Step 6: Commit numeric semantics**

```bash
git add src/execution/mod.rs src/execution/value.rs src/execution/numeric.rs src/verify/functions.rs src/verify/tests.rs docs/superpowers/specs/2026-08-22-issue-38-deterministic-tier0-execution-semantics-design.md docs/superpowers/plans/2026-08-22-issue-39-tier0-scalar-control-interpreter.md
git commit -m "feat(vm): implement Kotlin scalar semantics (#39)"
```

### Task 3: Admit artifacts into a resolved immutable execution image

**Files:**
- Modify: `src/artifact/mod.rs`
- Create: `src/execution/image.rs`
- Modify: `src/execution/mod.rs`

- [x] **Step 1: Write admission and portable-storage tests**

Add tests in `src/execution/image.rs` using `fixtures::scalar_artifact()`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::fixtures;

    #[test]
    fn portable_frame_charge_is_checked_and_aligned() {
        assert_eq!(48, frame_charge(1).unwrap());
        assert_eq!(64, frame_charge(2).unwrap());
        assert_eq!(160, frame_charge(8).unwrap());
        assert_eq!(Err(AdmissionError::StoragePlanOverflow), frame_charge(u64::MAX));
    }

    #[test]
    fn admission_resolves_calls_and_rejects_non_tier0_families() {
        let artifact = fixtures::scalar_artifact();
        let image = ExecutionImage::admit(artifact, fixtures::profile()).unwrap();
        assert_eq!(image.entry(), FunctionKey { module: 0, function: 0 });
        assert!(image.functions().iter().all(|function| function.register_count <= image.registers_per_frame()));

        let artifact = fixtures::artifact_with_new_object();
        assert_eq!(Err(AdmissionError::InvalidEntry), ExecutionImage::admit(artifact, fixtures::profile()).map(|_| ()));
    }

    #[test]
    fn admission_is_atomic_across_every_profile_limit() {
        for profile in fixtures::profiles_below_each_manifest_limit() {
            assert!(ExecutionImage::admit(fixtures::scalar_artifact(), profile).is_err());
        }
    }
}
```

- [x] **Step 2: Run the admission tests and verify they fail**

Run: `cargo test --locked --offline execution::image::tests -- --nocapture`

Expected: FAIL because `ExecutionImage`, fixtures, and profile types are absent.

- [x] **Step 3: Expose immutable verified data only inside the crate**

Add to `impl VerifiedArtifact` in `src/artifact/mod.rs`:

```rust
pub(crate) fn decoded(&self) -> &DecodedArtifact {
    &self.inner.decoded
}
```

Do not expose `DecodedArtifact` or this accessor publicly.

- [x] **Step 4: Define profile and resolved image metadata**

In `src/execution/image.rs`, define `ExecutionProfile`, `ExecutionImage`,
`ResolvedFunction`, `ResolvedBlock`, `ResolvedInstruction`, and
`ResolvedValueType`. `ExecutionImage` is a cheap cloneable wrapper around
`Arc<ExecutionImageInner>`; the inner value owns boxed metadata slices and
contains only integer indices, never pointers into decoder vectors.
`ExecutionProfile` contains every #38 host ceiling and exact
compiler/standard-library ABI identities:

```rust
#[derive(Clone, Debug)]
pub(super) struct ExecutionProfile {
    pub heap_bytes: u32,
    pub frame_storage_bytes: u64,
    pub maximum_call_depth: u32,
    pub maximum_coroutines: u32,
    pub maximum_host_requests: u32,
    pub maximum_events: u32,
    pub maximum_slice_budget: u32,
    pub compiler_abi: [u8; 32],
    pub standard_library_abi: [u8; 32],
    pub capability_mask: u32,
    pub host_references: Box<[AdmittedReference]>,
}
```

`AdmittedReference` contains `TypeKey`, symbolic handle, generation, and a live
flag. Admission rejects duplicate `(handle, generation)` identities and type IDs
that do not resolve in this image. `RuntimeValue::Reference` carries only the
image hash, type, handle, and generation; entry validation consults the admitted
table rather than trusting liveness supplied by a caller.

Resolve local/imported type and function references once during admission. Copy constants into `RuntimeValue`. Convert block targets to absolute `(function, local_block)` identities. Convert `call_direct` targets to absolute `FunctionKey`. Return `AdmissionError::InvalidEntry` for abstract/suspending entries, inconsistent resolved IDs, or any instruction outside scalar/control/direct-call families. This makes `UnsupportedInstruction` unreachable for every admitted image.

- [x] **Step 5: Implement portable storage planning and reservation**

Implement `frame_charge(registers) = align16(32 + registers * 16)` with checked `u64` operations. Compute the largest executable frame, multiply by admitted `maximum_call_depth`, and require both the manifest `required_stack_bytes` and profile storage ceiling to cover it. Convert the resulting register slot count and frame count to `usize`, reserve every image vector with `try_reserve_exact`, and map failures to `AllocationFailed`.

- [x] **Step 6: Create the fixture builder required by these tests**

Add `mod image;` plus `#[cfg(test)] mod fixtures;` to
`src/execution/mod.rs`. Create `src/execution/fixtures.rs` with helpers that
build `DecodedArtifact` values from explicit functions/blocks/instructions,
pass them through `test_encode::encode_artifact`, and call public
`verify_artifact` before admission. The default manifest uses ABI `[0x11; 32]`/
`[0x22; 32]`, zero required capabilities, 64 KiB heap, call depth 16, one
coroutine, maximum block cost 64, minimum slice cost 64, and sufficient stack
storage. Every helper must therefore exercise the real decoder and verifier
rather than constructing `VerifiedArtifact` directly.

- [x] **Step 7: Run admission and existing verification tests**

Run: `cargo test --locked --offline execution::image::tests -- --nocapture`

Expected: PASS, 3 tests.

Run: `cargo test --locked --offline verify::tests -- --nocapture`

Expected: all existing verifier tests PASS.

- [x] **Step 8: Commit admission**

```bash
git add src/artifact/mod.rs src/execution/mod.rs src/execution/image.rs src/execution/fixtures.rs
git commit -m "feat(vm): admit bounded Tier 0 execution images (#39)"
```

### Task 4: Start one-shot machines with typed arguments and fixed arenas

**Files:**
- Create: `src/execution/machine.rs`
- Modify: `src/execution/value.rs`
- Modify: `src/execution/error.rs`
- Modify: `src/execution/mod.rs`
- Create: `src/execution/tests.rs`

- [x] **Step 1: Write typed-entry and one-shot lifecycle tests**

Create `src/execution/tests.rs`:

```rust
use super::{fixtures, image::ExecutionImage, machine::Machine, value::*};

#[test]
fn start_validates_all_arguments_before_mutation() {
    let image = ExecutionImage::admit(fixtures::typed_entry_artifact(), fixtures::profile()).unwrap();
    let mut machine = Machine::new(image).unwrap();
    let before = machine.test_snapshot();
    assert!(machine.start(&[EntryArgument(RuntimeValue::I64(1))]).is_err());
    assert_eq!(before, machine.test_snapshot());
    machine.start(&[EntryArgument(RuntimeValue::I32(1))]).unwrap();
    assert_eq!(1, machine.frame_depth());
    assert_eq!(RuntimeValue::I32(1), machine.test_register(0).unwrap());
}

#[test]
fn references_require_matching_image_type_liveness_and_generation() {
    let (image, valid, foreign, dead, stale) = fixtures::reference_entry_case();
    assert!(Machine::new(image.clone()).unwrap().start(&[EntryArgument(valid)]).is_ok());
    assert!(Machine::new(image.clone()).unwrap().start(&[EntryArgument(foreign)]).is_err());
    assert!(Machine::new(image.clone()).unwrap().start(&[EntryArgument(dead)]).is_err());
    assert!(Machine::new(image).unwrap().start(&[EntryArgument(stale)]).is_err());
}

#[test]
fn failed_start_is_retryable_but_successful_start_is_one_shot() {
    let image = ExecutionImage::admit(fixtures::typed_entry_artifact(), fixtures::profile()).unwrap();
    let mut machine = Machine::new(image).unwrap();
    assert!(machine.start(&[]).is_err());
    machine.start(&[EntryArgument(RuntimeValue::I32(7))]).unwrap();
    assert_eq!(Err(RunError::AlreadyStarted), machine.start(&[EntryArgument(RuntimeValue::I32(8))]));
}
```

- [x] **Step 2: Run the entry tests and verify they fail**

Run: `cargo test --locked --offline execution::tests -- --nocapture`

Expected: FAIL with missing `Machine` and fixture helpers. If Cargo accepts only one filter, run the whole `execution::tests` module.

- [x] **Step 3: Implement preallocated mutable state**

Add `mod machine;` plus `#[cfg(test)] mod tests;` to `src/execution/mod.rs`.
Define `Machine` with an owned `ExecutionImage`, lifecycle enum,
`Box<[Frame]>` sized to maximum depth, `Box<[RegisterValue]>` sized to the
admitted arena, active depth, cumulative fixed/dynamic counters, and trace
state. `Machine::new` uses fallible reservation before publishing the machine.
`Frame` contains function, local block, caller continuation block/instruction,
optional caller destination, and the fixed register-window base. Neither start
nor later dispatch may push or resize a `Vec`.

- [x] **Step 4: Implement atomic typed start**

Validate arity and every argument into an index-only validation pass. Primitive kinds must match exactly; null requires a nullable reference; non-null references require the same image digest, a live handle/generation registered by the test host table, and nominal assignability through image supertype metadata. Only after all arguments pass, initialize frame zero and copy parameters to registers `0..parameter_count`; keep other slots `Uninitialized`. Set lifecycle to runnable. Preserve the pristine machine after any error.

- [x] **Step 5: Run entry/lifecycle tests**

Run: `cargo test --locked --offline execution::tests -- --nocapture`

Expected: entry/lifecycle tests PASS; execution tests added later are not present yet.

- [x] **Step 6: Commit machine construction**

```bash
git add src/execution/mod.rs src/execution/machine.rs src/execution/value.rs src/execution/error.rs src/execution/tests.rs src/execution/fixtures.rs
git commit -m "feat(vm): start preallocated Tier 0 machines (#39)"
```

### Task 5: Execute constants, moves, arithmetic, conversions, and comparisons

**Files:**
- Modify: `src/execution/machine.rs`
- Modify: `src/execution/fixtures.rs`
- Modify: `src/execution/tests.rs`

- [x] **Step 1: Add scalar conformance vectors**

Add fixture cases for every form mapping (`1=i32`, `2=i64`, `3=f32`, `4=f64`, `5=bool`, `6=char`, `7=reference`) and assert exact raw-bit results. Include wrapping boundaries, shift counts 31/32/63/64/negative, integer zero division/remainder, `MIN/-1`, signed zero, infinities, raw NaN constants versus produced canonical NaNs, float remainder special cases, conversion rounding/saturation/NaN-to-zero, every comparison family, source/destination aliasing, null equality, and two symbolic reference identities.

Use a shared table shape:

```rust
struct ScalarCase {
    name: &'static str,
    artifact: VerifiedArtifact,
    args: Box<[EntryArgument]>,
    expected: Result<RuntimeValue, GuestTrap>,
    expected_fixed_cost: u64,
}

#[test]
fn scalar_vectors_match_kotlin_jvm_semantics() {
    for case in fixtures::scalar_cases() {
        let mut machine = fixtures::started(case.artifact, &case.args);
        let outcome = machine.run_slice(fixtures::profile().maximum_slice_budget).unwrap();
        match case.expected {
            Ok(value) => assert_eq!(Outcome::Halted(Some(value)), outcome, "{}", case.name),
            Err(trap) => assert_eq!(Outcome::Crashed(trap), outcome, "{}", case.name),
        }
        assert_eq!(case.expected_fixed_cost, machine.consumed_fixed_cost(), "{}", case.name);
    }
}
```

- [x] **Step 2: Run the scalar vector test and verify it fails**

Run: `cargo test --locked --offline execution::tests::scalar_vectors_match_kotlin_jvm_semantics -- --nocapture`

Expected: FAIL because `run_slice` and scalar dispatch are absent.

- [x] **Step 3: Implement read-before-write scalar dispatch**

For each instruction, copy all source `RuntimeValue`s to locals before touching the destination. Match the verified form and value variants; delegate every numeric operation to `numeric.rs`. Load constants without rewriting their stored raw NaN bits. On a successful operation, write one destination. On division by zero or invalid character conversion, return `GuestTrap` before destination publication. Any form/value mismatch is `VmFault::InvalidValueType`.

- [x] **Step 4: Implement root return and terminal stability**

Read the optional return register before clearing the frame. Root return stores `Outcome::Halted(value)` in lifecycle. Guest traps store `Outcome::Crashed(trap)`. Repeated `run_slice` on a terminal machine returns the same stored outcome without dispatch or counter changes.

- [x] **Step 5: Run scalar vectors in debug and release**

Run: `cargo test --locked --offline execution::tests::scalar_vectors_match_kotlin_jvm_semantics -- --nocapture`

Expected: PASS.

Run: `cargo test --release --locked --offline execution::tests::scalar_vectors_match_kotlin_jvm_semantics -- --nocapture`

Expected: PASS with identical results and costs.

- [x] **Step 6: Commit scalar dispatch**

```bash
git add src/execution/machine.rs src/execution/fixtures.rs src/execution/tests.rs
git commit -m "feat(vm): execute Tier 0 scalar bytecode (#39)"
```

### Task 6: Implement block-atomic slicing and deterministic control flow

**Files:**
- Modify: `src/execution/machine.rs`
- Modify: `src/execution/fixtures.rs`
- Modify: `src/execution/tests.rs`

- [x] **Step 1: Write quota/control/lifecycle vectors**

Add exact-fit, insufficient-first-block, discarded-remainder, trap-after-charge, branch-both-ways, sparse/dense switch, multi-block loop, and one-block infinite-loop cases:

```rust
#[test]
fn block_cost_is_atomic_and_slice_remainder_is_discarded() {
    let mut exact = fixtures::started_zero_arg(fixtures::two_block_artifact(3, 5));
    assert_eq!(Outcome::Halted(None), exact.run_slice(8).unwrap());
    assert_eq!(8, exact.consumed_fixed_cost());

    let mut short = fixtures::started_zero_arg(fixtures::two_block_artifact(3, 5));
    assert_eq!(Outcome::SliceExhausted, short.run_slice(7).unwrap());
    assert_eq!(3, short.consumed_fixed_cost());
    assert_eq!(Outcome::Halted(None), short.run_slice(5).unwrap());
    assert_eq!(8, short.consumed_fixed_cost());
}

#[test]
fn while_true_executes_floor_budget_over_block_cost_iterations() {
    let mut machine = fixtures::started_zero_arg(fixtures::empty_loop_artifact(3));
    assert_eq!(Outcome::SliceExhausted, machine.run_slice(10).unwrap());
    assert_eq!(9, machine.consumed_fixed_cost());
    assert_eq!(3, machine.entered_blocks());
    assert_eq!(Outcome::SliceExhausted, machine.run_slice(4).unwrap());
    assert_eq!(12, machine.consumed_fixed_cost());
    assert_eq!(4, machine.entered_blocks());
}

#[test]
fn trap_keeps_the_full_containing_block_charge() {
    let mut machine = fixtures::started_zero_arg(fixtures::trap_after_write_artifact(7));
    assert_eq!(Outcome::Crashed(GuestTrap::DivisionByZero), machine.run_slice(7).unwrap());
    assert_eq!(7, machine.consumed_fixed_cost());
    assert_eq!(fixtures::pre_trap_registers(), machine.test_active_registers());
}
```

- [x] **Step 2: Run the quota/control tests and verify they fail**

Run: `cargo test --locked --offline execution::tests -- --nocapture`

Expected: at least one FAIL because slicing is not block-atomic yet; use the module filter if Cargo accepts only one substring.

- [x] **Step 3: Implement `run_slice` validation and charging**

Validate runnable lifecycle and require `budget > 0`, `budget >= image.minimum_slice_cost`, and `budget <= image.maximum_slice_budget` before mutation. At every block boundary, compare the complete verified block cost with remaining credit. If it does not fit, discard the local remainder, preserve the current block, and return `SliceExhausted`. Otherwise subtract and add the full cost with checked arithmetic before dispatching any instruction. Count entered blocks and bytecode instructions with checked `u64`; overflow is `VmFault::AccountingOverflow`.

- [x] **Step 4: Implement deterministic targets**

Implement `jump`, canonical-boolean `branch`, and binary search over the already sorted unique `switch_i32` cases. Store only the chosen block; search strategy must not enter the trace. Executed `unreachable` terminates with `VmFault::ReachedUnreachable`. A resolved-ID miss becomes `VmFault::InvalidResolvedId` and never indexes unchecked.

- [x] **Step 5: Run all quota/control vectors in debug and release**

Run: `cargo test --locked --offline execution::tests -- --nocapture`

Expected: all current execution tests PASS.

Run: `cargo test --release --locked --offline execution::tests -- --nocapture`

Expected: identical outcomes, register state, and costs.

- [x] **Step 6: Commit slicing and control flow**

```bash
git add src/execution/machine.rs src/execution/fixtures.rs src/execution/tests.rs
git commit -m "feat(vm): enforce block-atomic Tier 0 slices (#39)"
```

### Task 7: Implement direct calls, returns, and recursion limits

**Files:**
- Modify: `src/execution/machine.rs`
- Modify: `src/execution/fixtures.rs`
- Modify: `src/execution/tests.rs`

- [x] **Step 1: Write nested-call, aliasing, and stack-overflow tests**

```rust
#[test]
fn direct_calls_copy_arguments_and_publish_results_on_return() {
    let mut machine = fixtures::started_zero_arg(fixtures::nested_call_artifact());
    assert_eq!(Outcome::Halted(Some(RuntimeValue::I32(42))), machine.run_slice(128).unwrap());
    assert_eq!(3, machine.maximum_observed_frame_depth_for_test());
}

#[test]
fn stack_overflow_happens_before_a_new_frame_exists() {
    let mut profile = fixtures::profile();
    profile.maximum_call_depth = 3;
    let mut machine = fixtures::started_with_profile(fixtures::recursive_artifact(3), profile);
    assert_eq!(Outcome::Crashed(GuestTrap::StackOverflow), machine.run_slice(128).unwrap());
    assert_eq!(3, machine.frame_depth());
    assert_eq!(fixtures::recursive_pre_call_state(3), machine.test_active_registers());
}
```

The observed-depth value counts the root as depth one, so the assertion records
the root plus two nested callees. Use that convention consistently across
admission, dispatch, and tests.

- [x] **Step 2: Run direct-call tests and verify they fail**

Run: `cargo test --locked --offline execution::tests -- --nocapture`

Expected: FAIL because direct call dispatch is not implemented.

- [x] **Step 3: Implement atomic call entry**

Resolve the already admitted target, copy every source argument to fixed local scratch slots before changing frame depth, and check maximum depth before reserving the callee slot. On overflow, produce `GuestTrap::StackOverflow` with the caller unchanged. Initialize callee parameters, clear its remaining fixed register window to `Uninitialized`, save caller continuation and optional destination, then increment active depth. Premature arena exhaustion becomes `VmFault::InvalidStoragePlan`.

- [x] **Step 4: Implement callee return publication**

Read the result before clearing the callee. Decrement depth, restore the saved caller continuation, and only then initialize its destination. Unit calls never have a destination. Value calls always do; any mismatch is `VmFault::InvalidValueType` because the verifier/admission invariant was broken.

- [x] **Step 5: Run call and full execution suites**

Run: `cargo test --locked --offline execution::tests -- --nocapture`

Expected: all execution tests PASS, including recursion limit before frame mutation.

- [x] **Step 6: Commit calls**

```bash
git add src/execution/machine.rs src/execution/fixtures.rs src/execution/tests.rs
git commit -m "feat(vm): execute bounded Tier 0 direct calls (#39)"
```

### Task 8: Add canonical block-boundary traces and conformance digests

**Files:**
- Modify: `src/execution/machine.rs`
- Modify: `src/execution/value.rs`
- Modify: `src/execution/fixtures.rs`
- Modify: `src/execution/tests.rs`

- [x] **Step 1: Write trace digest golden-vector tests**

Define a stable trace record containing version byte, artifact content hash, entered module/function/block, frame depth, remaining slice budget, cumulative fixed/dynamic cost, and canonical active-register encodings. Hash length-prefixed little-endian fields with SHA-256. Add explicit expected 32-byte digests for straight-line, branch, switch, nested call, trap, exact-fit, discarded-remainder, and infinite-loop vectors:

```rust
#[test]
fn block_boundary_trace_digests_are_stable() {
    for case in fixtures::trace_cases() {
        let mut machine = fixtures::started(case.artifact, &case.args);
        let outcome = machine.run_slice(case.budget).unwrap();
        assert_eq!(case.outcome, outcome, "{}", case.name);
        assert_eq!(case.digest, machine.trace_digest(), "{}", case.name);
        assert_eq!(case.fixed_cost, machine.consumed_fixed_cost(), "{}", case.name);
        assert_eq!(0, machine.consumed_dynamic_cost(), "{}", case.name);
    }
}
```

- [x] **Step 2: Run trace tests with zero placeholders and verify failure**

Initially give each expected digest `[0; 32]`.

Run: `cargo test --locked --offline execution::tests::block_boundary_trace_digests_are_stable -- --nocapture`

Expected: FAIL and print actual non-zero digest values.

- [x] **Step 3: Implement allocation-free incremental tracing**

Store a `Sha256` state in `Machine`, initialized during `Machine::new`. At each successfully charged block entry, feed the canonical record directly through fixed stack byte arrays; never build a `Vec` or format a string. Encode runtime discriminant, primitive little-endian bits, null, and symbolic `(type module, type id, handle, generation)` reference identity. Include initialized/uninitialized register markers. Exclude wall time, addresses, native enum layout, allocator state, and switch search steps.

- [x] **Step 4: Replace zero digests with reviewed golden values**

Copy the actual digest values printed by the failing test into `fixtures::trace_cases()`, rerun once, then independently compute one straight-line digest in the test by feeding the documented bytes to a fresh `Sha256` and assert it equals the committed value.

- [x] **Step 5: Run trace tests in debug and release**

Run: `cargo test --locked --offline execution::tests::block_boundary_trace_digests_are_stable -- --nocapture`

Expected: PASS.

Run: `cargo test --release --locked --offline execution::tests::block_boundary_trace_digests_are_stable -- --nocapture`

Expected: PASS with the same committed digests.

- [x] **Step 6: Commit conformance traces**

```bash
git add src/execution/machine.rs src/execution/value.rs src/execution/fixtures.rs src/execution/tests.rs
git commit -m "test(vm): lock Tier 0 conformance traces (#39)"
```

### Task 9: Prove steady-state allocation freedom and record performance workloads

**Files:**
- Modify: `src/execution/tests.rs`
- Modify: `src/execution/fixtures.rs`
- Create: `docs/performance/tier0-baseline.md`

- [x] **Step 1: Add a thread-local counting allocator test harness**

Define one test-only global allocator wrapper around `std::alloc::System`. Use thread-local `Cell<bool>` and `Cell<u64>` so allocations from parallel test threads do not contaminate the active measurement. Count `alloc`, `alloc_zeroed`, and growing `realloc` only while the current thread's flag is enabled. Keep allocator method bodies limited to counter update plus direct delegation to `System`.

- [x] **Step 2: Write the steady-state allocation test**

```rust
#[test]
fn scalar_control_steady_state_allocates_nothing() {
    for artifact in fixtures::allocation_workloads() {
        let mut machine = fixtures::started_zero_arg(artifact);
        allocation_counter::reset_and_enable();
        for _ in 0..1_000 {
            assert!(matches!(machine.run_slice(4_096).unwrap(), Outcome::SliceExhausted));
        }
        let allocations = allocation_counter::disable_and_read();
        assert_eq!(0, allocations);
    }
}
```

Use only non-terminal looping workloads so setup, admission, start, outcome formatting, and teardown occur outside the measured region.

- [x] **Step 3: Run the allocation test and remove every lazy allocation**

Run: `cargo test --release --locked --offline execution::tests::scalar_control_steady_state_allocates_nothing -- --nocapture --test-threads=1`

Expected: PASS with zero counted allocations. If it fails, replace lazy metadata, capacity growth, trace buffers, or dispatch scratch collections with admission/start-time fixed storage; do not exempt allocations from the counter.

- [x] **Step 4: Add ignored release performance workloads**

Add one ignored test `tier0_performance_baseline` that runs hot integer
arithmetic, mixed branch/switch, nested direct calls, and empty quota loop for a
fixed warmup and measurement iteration count. Use `std::time::Instant` only in
the host test harness. After the measured region, invoke `rustc -Vv` through
`std::process::Command` and print one TSV row per workload with artifact hash,
workload name, blocks, instructions, elapsed nanoseconds, blocks/s,
instructions/s, compiler release/host target, and CPU text accepted from
`COMPUKTER_BENCH_CPU`. Do not feed timing into VM state or assertions.

- [x] **Step 5: Run and record the release baseline**

Run:

```bash
COMPUKTER_BENCH_CPU="$(uname -m) local" cargo test --release --locked --offline execution::tests::tier0_performance_baseline -- --ignored --nocapture --test-threads=1
```

Expected: PASS and four TSV workload rows with non-zero block/instruction rates.

Create `docs/performance/tier0-baseline.md` describing the exact command, build profile, workload parameters, reported fields, and the four observed rows. State explicitly that CI has no absolute hardware-specific throughput threshold; semantic, accounting, and allocation regressions remain hard failures.

- [x] **Step 6: Run the complete quality gate**

Run: `cargo fmt --check`

Expected: PASS.

Run: `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`

Expected: PASS with no warnings.

Run: `cargo test --locked --offline`

Expected: all existing and new tests PASS; only explicit regeneration/performance tests remain ignored.

Run: `cargo test --release --locked --offline execution::tests -- --nocapture --test-threads=1`

Expected: all non-ignored Tier 0 semantic, trace, and allocation tests PASS.

Run: `cargo tree --locked --offline`

Expected: no new runtime dependency beyond the existing `sha2` stack.

- [x] **Step 7: Inspect the public API boundary**

Run: `cargo doc --no-deps --locked --offline` and
`rg -n "ExecutionImage|ExecutionProfile|Machine|RuntimeValue|Outcome|GuestTrap" target/doc/compukter_vm/index.html`

Expected: documentation succeeds and `rg` returns no public execution symbol matches.

- [x] **Step 8: Commit performance and final verification assets**

```bash
git add src/execution/fixtures.rs src/execution/tests.rs docs/performance/tier0-baseline.md
git commit -m "test(vm): verify Tier 0 allocation and throughput (#39)"
```

### Task 10: Reconcile documentation and issue acceptance

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/plans/2026-08-22-issue-39-tier0-scalar-control-interpreter.md`

- [x] **Step 1: Document the implemented private boundary**

Add a concise README development-status paragraph: artifacts can be decoded, verified, and exercised by a crate-private scalar/control semantic oracle; public execution remains intentionally unavailable until mandatory v1 families exist. Link #38, #39, the accepted design, this plan, and `docs/performance/tier0-baseline.md`.

- [x] **Step 2: Mark completed plan checkboxes and run diff hygiene**

Change each executed `- [ ]` in this plan to `- [x]` only after its command has passed.

Run: `git diff --check`

Expected: PASS with no whitespace errors.

- [x] **Step 3: Run the final fresh gate**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked --offline -- -D warnings
cargo test --locked --offline
cargo test --release --locked --offline execution::tests -- --nocapture --test-threads=1
```

Expected: every command exits zero; record exact test counts and ignored tests in the #39 completion comment.

- [x] **Step 4: Commit documentation**

```bash
git add README.md docs/superpowers/plans/2026-08-22-issue-39-tier0-scalar-control-interpreter.md
git commit -m "docs(vm): record Tier 0 interpreter status (#39)"
```

- [ ] **Step 5: Close the exact Roadmap unit only after acceptance is proven**

Comment on #39 with commit range, exact verification commands/counts, allocation result, baseline document, and confirmation that execution remains private. Close #39 as `completed` and set its Roadmap item to Done. If any acceptance item is unverified, leave #39 open in Now and name the exact remaining check instead.
