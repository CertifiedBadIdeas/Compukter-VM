# DBT Metadata Sets Sweep Implementation Plan

> Issue: [#498](https://github.com/CertifiedBadIdeas/Compukter-Kraft/issues/498)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure the effect of DBT metadata-table capacity independently from native-code capacity.

**Architecture:** Generalize the existing C/QEMU runner to select either the retained cache-byte sweep or a new metadata-sets sweep. The sets sweep fixes cache storage at 128 KiB, scratch at 8 KiB, max block length at 8, and two-way associativity while testing 16 through 512 sets.

**Tech Stack:** Rust, Bash, Clang/LLD, QEMU TCG, Cargo tests.

---

### Task 1: Selectable sweep matrix

**Files:**
- Modify: `examples/rv32_c_comparison.rs`
- Modify: `scripts/tests/rv32-c-qemu-comparison.sh`
- Test: `tests/rv32_c_comparison_contract.rs`

- [x] Add failing contracts for `cache` and `sets` selection and six set-specific candidate names.
- [x] Replace the fixed candidate enum with immutable candidate specifications selected by the CLI.
- [x] Rotate an arbitrary candidate count without changing native/QEMU normalization order.
- [x] Make the shell gate validate and retain artifacts for the selected sweep.
- [x] Run focused contract tests.

### Task 2: Verify and measure

**Files:**
- Modify only if verification exposes a scoped defect.

- [x] Run formatting, diff, and full Cargo tests.
- [x] Run `RV32_C_DBT_SWEEP=sets bash scripts/tests/rv32-c-qemu-comparison.sh`.
- [x] Record timing, miss rate, eviction causes, reserved bytes, and the keep/reject decision in #498.
- [x] Commit the slice with `#498` in the subject.
