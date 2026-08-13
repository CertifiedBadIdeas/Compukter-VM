# Exact RV32 Cached DBT execution profile

Issue: [#17](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/17)

## Result

The exact profile is deterministic and identifies one narrow next optimization target. The shared-C checksum remained `ee053d58`; both independent runs produced the same 89 block records, 122 static-edge records, all execution counts, ordering, coverage, and dynamic-exit totals. Only the explicitly instrumented wall time varied.

The profile table used 211 of 4096 records, retained 131,144 bytes, and did not overflow. It counted 476,980 block-body entries and 476,975 static edges. Dynamic exits were three `JALR`, two memory/MMIO exits, and one terminal exit; there were no budget or generic slow-instruction exits.

The distribution is unusually concentrated:

- guest block `0x000006a0` accounts for 53.6710% of all block entries;
- the first four blocks account for 93.2953%;
- one block covers 50%, four cover 90%, 13 cover 95%, and 41 cover 99%;
- the four hottest edges are self-loop backedges and together account for 92.4577% of all static edges.

## Environment and commands

- Linux `7.1.8-zen1-3-zen`, x86-64
- Rust/Cargo `1.95.0`
- Clang `22.1.8`
- QEMU `11.1.0`
- Wasmtime `47.0.3`
- profile configuration: Cached DBT, 512 sets, 128 KiB code cache, 16 guest instructions per block, `BlockBase(32)` alignment

```bash
cargo run --release --locked --offline \
  --features dbt-execution-profile,wasmtime-comparison \
  --example rv32_c_comparison -- \
  profile target/rv32-c-comparison 1000 4096

target/release/examples/rv32_c_comparison \
  profile target/rv32-c-comparison 1000 4096 \
  > /tmp/compukter-vm-profile-run-1.tsv
target/release/examples/rv32_c_comparison \
  profile target/rv32-c-comparison 1000 4096 \
  > /tmp/compukter-vm-profile-run-2.tsv
```

The two instrumented runs took 1,273,728 ns and 1,132,289 ns. These timings are diagnostic only and are not product-performance candidates.

## Hottest blocks

| Rank | Guest PC | Executions | Share | Cumulative |
|---:|---:|---:|---:|---:|
| 1 | `0x000006a0` | 256000 | 0.536710135 | 0.536710135 |
| 2 | `0x00000608` | 63000 | 0.132081010 | 0.668791144 |
| 3 | `0x00000648` | 63000 | 0.132081010 | 0.800872154 |
| 4 | `0x00000678` | 63000 | 0.132081010 | 0.932953164 |
| 5 | `0x0000063c` | 1000 | 0.002096524 | 0.935049688 |
| 6 | `0x0000066c` | 1000 | 0.002096524 | 0.937146212 |
| 7 | `0x000006b8` | 1000 | 0.002096524 | 0.939242736 |
| 8 | `0x000006f8` | 1000 | 0.002096524 | 0.941339260 |
| 9 | `0x00000738` | 1000 | 0.002096524 | 0.943435783 |
| 10 | `0x00000778` | 1000 | 0.002096524 | 0.945532307 |
| 11 | `0x000007b8` | 1000 | 0.002096524 | 0.947628831 |
| 12 | `0x000007f8` | 1000 | 0.002096524 | 0.949725355 |
| 13 | `0x00000838` | 1000 | 0.002096524 | 0.951821879 |
| 14 | `0x00000878` | 1000 | 0.002096524 | 0.953918403 |
| 15 | `0x000008b8` | 1000 | 0.002096524 | 0.956014927 |
| 16 | `0x00000284` | 999 | 0.002094427 | 0.958109355 |
| 17 | `0x000002c4` | 999 | 0.002094427 | 0.960203782 |
| 18 | `0x00000304` | 999 | 0.002094427 | 0.962298210 |
| 19 | `0x00000344` | 999 | 0.002094427 | 0.964392637 |
| 20 | `0x00000384` | 999 | 0.002094427 | 0.966487064 |

## Hottest static edges

| Rank | Source | Target | Kind | Executions | Share |
|---:|---:|---:|---|---:|---:|
| 1 | `0x000006a0` | `0x000006a0` | taken | 255000 | 0.534619215 |
| 2 | `0x00000608` | `0x00000608` | taken | 62000 | 0.129985848 |
| 3 | `0x00000648` | `0x00000648` | taken | 62000 | 0.129985848 |
| 4 | `0x00000678` | `0x00000678` | taken | 62000 | 0.129985848 |
| 5 | `0x00000608` | `0x0000063c` | fallthrough | 1000 | 0.002096546 |
| 6 | `0x0000063c` | `0x00000648` | taken | 1000 | 0.002096546 |
| 7 | `0x00000648` | `0x0000066c` | fallthrough | 1000 | 0.002096546 |
| 8 | `0x0000066c` | `0x00000678` | taken | 1000 | 0.002096546 |
| 9 | `0x00000678` | `0x000006a0` | fallthrough | 1000 | 0.002096546 |
| 10 | `0x000006a0` | `0x000006b8` | fallthrough | 1000 | 0.002096546 |
| 11 | `0x000006b8` | `0x000006f8` | fallthrough | 1000 | 0.002096546 |
| 12 | `0x000006f8` | `0x00000738` | fallthrough | 1000 | 0.002096546 |
| 13 | `0x00000738` | `0x00000778` | fallthrough | 1000 | 0.002096546 |
| 14 | `0x00000778` | `0x000007b8` | fallthrough | 1000 | 0.002096546 |
| 15 | `0x000007b8` | `0x000007f8` | fallthrough | 1000 | 0.002096546 |
| 16 | `0x000007f8` | `0x00000838` | fallthrough | 1000 | 0.002096546 |
| 17 | `0x00000838` | `0x00000878` | fallthrough | 1000 | 0.002096546 |
| 18 | `0x00000878` | `0x000008b8` | fallthrough | 1000 | 0.002096546 |
| 19 | `0x00000284` | `0x000002c4` | fallthrough | 999 | 0.002094449 |
| 20 | `0x000002c4` | `0x00000304` | fallthrough | 999 | 0.002094449 |

## Uninstrumented performance context

The final uninstrumented 21-sample gate places the selected 16-instruction Cached DBT at 444,193 ns/kernel: 7.257x native Clang, 0.870x QEMU TCG (therefore about 15.0% faster than QEMU), and 3.208x Wasmtime AOT. The exact-profile time above must not be mixed into this table.

## Next optimization

Implement a same-block backedge fast path that preserves the current register-cache mapping across an internal loop iteration. This is deliberately narrower than a general cross-block register ABI: only a proven `source_pc == target_pc` edge re-enters a local loop header, while fallthrough, slow exits, traps, invalidation, and external entries retain the existing fully materialized contract.

This choice is tied directly to the profile: the four self-loop edges cover 92.4577% of all static edge executions, and `0x000006a0 -> 0x000006a0` alone covers 53.4619%. In the existing uninstrumented code audit, block `0x000006a0` is six guest instructions but 303 emitted bytes/79 decoded host instructions; its taken path stores dirty guest values, jumps back through the budget guard, and reloads loop-carried values. A local backedge can remove that repeated materialization without reducing the guest register set or defining a fragile register convention between unrelated blocks.
