# RV32 DBT focused self-A/B protocol

Date: 2026-08-14

Issue: [#17](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/17)

## Purpose

Use a short, same-process comparison for ordinary DBT optimization slices. The full Native/QEMU/Wasmtime ceiling remains a periodic validation gate rather than a prerequisite for every local experiment.

The focused runner:

- accepts exactly two product DBT configurations;
- calibrates each configuration independently to the same sample-duration target;
- alternates `A/B` and `B/A` execution order for 21 or more warm samples;
- validates the shared C checksum and equal retired guest-instruction count;
- requires zero steady-state host allocations;
- reports warm throughput, resident/code metrics, and separately timed `lift`, `lower`, and `publish` phases.

## Command

```sh
scripts/tests/rv32-c-self-ab.sh
```

Candidate selection is explicit:

```sh
RV32_C_SELF_AB_BASELINE=rv32-cached-dbt-block-16 \
RV32_C_SELF_AB_CANDIDATE=rv32-cached-dbt-block-32 \
scripts/tests/rv32-c-self-ab.sh
```

The report is written to `target/rv32-c-comparison/self-ab-report.tsv`. The runner depends on the local RV32 C toolchain but does not invoke QEMU or Wasmtime.

## Protocol smoke result

The initial `block-16` versus `block-32` run used 21 paired samples and independently calibrated both candidates to batch 1024.

| Candidate | Median ns/kernel | Delta | Translations | Emitted bytes | Steady allocations |
|---|---:|---:|---:|---:|---:|
| block-16 baseline | 381,572.233 | 0.000% | 89 | 39,990 | 0 |
| block-32 candidate | 380,004.620 | -0.411% | 68 | 38,165 | 0 |

The small throughput difference is not a product-default decision. It demonstrates that the focused gate preserves semantic/resource checks and exposes expected structural differences without paying for the complete external-runtime matrix.

## When the full ceiling is required

Run `scripts/tests/rv32-c-qemu-comparison.sh` before a product-default decision, after a large code-generation or execution-model change, when a focused result is surprising, and periodically to refresh the absolute Native/QEMU/Wasmtime position.
