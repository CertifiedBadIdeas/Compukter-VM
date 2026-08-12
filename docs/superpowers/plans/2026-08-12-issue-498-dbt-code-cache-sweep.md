# DBT Code Cache Sweep Implementation Plan

> Issue: [#498](https://github.com/CertifiedBadIdeas/Compukter-Kraft/issues/498)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Produce an uncontaminated six-size product DBT code-cache comparison against native Clang and QEMU TCG.

**Architecture:** Give product DBT separate scratch/workspace and persistent-cache capacities. Extend the existing calibrated C/QEMU harness with cached-DBT candidates parameterized only by persistent cache size.

**Tech Stack:** Rust, x86-64 DBT, Linux memfd mappings, Cargo tests, Bash, Clang/LLD, QEMU TCG.

---

### Task 1: Separate transient and persistent executable capacities

**Files:**
- Modify: `src/rv32_machine/dbt.rs`
- Modify: `src/rv32_machine/machine.rs`
- Modify: `src/rv32_machine/mod.rs`
- Test: `src/rv32_machine/dbt.rs`
- Test: `tests/rv32_machine.rs`

- [x] Add failing tests proving a cached DBT with 8 KiB scratch and 16 KiB cache reserves 48 KiB of dual-alias virtual space, while Direct reserves 16 KiB.
- [x] Run the focused tests and confirm their reserve assertions fail under the shared-capacity constructor.
- [x] Change `DirectDbt` to accept `scratch_bytes`; change `CachedDbt` to accept `scratch_bytes` and `cache_bytes`; make the workspace use `scratch_bytes`.
- [x] Expose metadata and overlap eviction counters independently in `Rv32DbtStats` while preserving their sum as `evictions`.
- [x] Update all configuration call sites with an 8 KiB product scratch and their existing cache capacity.
- [x] Run focused DBT and machine tests.

### Task 2: Add the code-cache sweep to the C/QEMU harness

**Files:**
- Modify: `examples/rv32_c_comparison.rs`
- Modify: `scripts/tests/rv32-c-qemu-comparison.sh`
- Test: `tests/rv32_c_comparison_contract.rs`

- [x] Add failing contract assertions for cached-DBT rows at 16, 32, 64, 128, 256, and 512 KiB and separate eviction columns.
- [x] Run the contract test and confirm it fails because the sweep is absent.
- [x] Parameterize cached-DBT candidates by cache bytes while keeping scratch at 8 KiB, sets at 32, and max instructions at 8.
- [x] Include all candidates in rotating sampling, checksum validation, report validation, and disassembly artifacts.
- [x] Run the focused contract tests.

### Task 3: Verify and measure

**Files:**
- Modify only if verification exposes a defect in the planned behavior.

- [x] Run `cargo fmt --check` and `git diff --check`.
- [x] Run `cargo test --locked --offline` and confirm zero failures.
- [x] Run `bash scripts/tests/rv32-c-qemu-comparison.sh` and retain `target/rv32-c-comparison/report.tsv`.
- [x] Compare miss rate, eviction causes, execution time, QEMU ratio, and reserved memory for all six capacities.
- [x] Commit the accepted implementation with `#498` in the subject and record the keep/reject decision on issue #498.
