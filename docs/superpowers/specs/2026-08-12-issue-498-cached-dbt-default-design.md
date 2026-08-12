# Cached DBT Default and Cranelift Removal

## Decision

The product VM exposes two active execution profiles: `CachedDbt` and
`DirectDbt`. `CachedDbt` is the default. The interpreter backends remain
available as explicit reference implementations for correctness tests and
specialized benchmarks, but ordinary product sweeps do not select them.

The abandoned Cranelift JIT is removed completely. The native DBT remains the
only runtime code-generation implementation and continues to use its own ABI,
translator, scratch arena, and per-machine code cache.

## Default geometry

The default cached DBT configuration is the measured product point from issue
#498:

- 256 metadata sets;
- 8 guest instructions per translated block;
- 8 KiB translation scratch space;
- 128 KiB executable code cache.

These values live beside `Rv32ExecutionBackendConfig` and are reused by the
product benchmark so the benchmarked product and the API default cannot drift.

## Compatibility boundary

`Cached`, `Predecoded`, and `BlockCached` remain valid explicit configurations.
Their enum variants and implementations are not removed. Only the obsolete
`Jit` configuration, its public preparation/statistics methods, machine loop,
Cranelift modules, and Cargo dependencies are removed.

