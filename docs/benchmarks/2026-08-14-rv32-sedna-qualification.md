# RV32 Sedna qualification

Date: 2026-08-14  
Issue: #17  
Decision: **REJECT**

## Question and gate

This one-off qualification asks whether upstream Sedna is close enough to the
Compukter VM Cached DBT to deserve a permanent place in the comparison suite.
The decision was fixed before measurement:

- Sedna at or below 1.25x Cached DBT time qualifies;
- 1.25x through 2.0x requires a second independent 21-sample control;
- above 2.0x is a permanent rejection from routine comparisons.

The upstream source was pinned to
`fnuecke/sedna@c6d7d6e591155521f7818de249c11d1223f07926`. Sedna ran in
XLEN=32 mode with its fastest standard `Memory.create` backend, which selected
direct `UnsafeMemory` on this little-endian host.

## Shared workload

Both implementations executed the exact same optimized C RV32IM ELF:

- ELF SHA-256: `4685eacadad20c445d7e4962d6e85d132ac224606549513945140dbce870adab`;
- ELF size: 13,080 bytes;
- batch: 1,024 kernels;
- kernel iterations: 1,000;
- seed: `0x12345678`;
- checksum: `ee053d58`;
- retired guest instructions: 4,012,218,388.

The Sedna adapter loaded the ELF's `PT_LOAD` segments into the same 16 KiB RAM
layout and implemented only the existing Compukter control MMIO page. Every
run used fresh CPU and RAM state. VM construction, ELF parsing, segment loading,
Gradle, and JVM startup were outside the timed interval. The timer covered only
`R5CPU.step()` through the guest's `STATUS_HALTED` store. Three complete
untimed executions warmed the same long-lived JVM before 21 measured samples.

The Cached DBT control also used 21 samples and fresh machines. Its interval
includes lazy DBT translation and publication performed during execution, so
the comparison does not hide Compukter VM's per-machine translation cost.

## Result

| Runtime | Median ns/kernel | p95 ns/kernel | Kernels/s | Time vs Cached DBT |
|---|---:|---:|---:|---:|
| Compukter Cached DBT, block 16 | 381,342.467 | 400,127.0 | 2,622.315 | 1.000x |
| Sedna XLEN=32 interpreter | 11,416,690.766 | 11,762,060.898 | 87.591 | **29.938x** |

The p95 time ratio is 29.396x. Cached block size 32 was also measured as a
same-process control at 380,663.396 ns/kernel, only 0.178% faster than block 16;
using it would make Sedna's ratio slightly worse and does not affect the
decision.

Sedna measured total nanoseconds, normalized by the 1,024-kernel batch:

```text
11667167503 11916088687 11725139031 11702248093 11641497314
11678057519 11692581434 11685828078 12158258454 11750234031
11732334300 11693099099 11732446664 11690691344 11629774467
11688486827 11649753133 12044350360 11621563510 11628589652
11613452276
```

## Environment

- Compukter VM: `7eb643bcd516102ab9a360f07c191f6254c063fd`
- Sedna: `c6d7d6e591155521f7818de249c11d1223f07926`
- CPU: AMD Ryzen 9 9950X3D
- OS: Linux 7.1.8-zen1-3-zen
- Java: OpenJDK 21.0.12
- Rust: 1.95.0
- Clang: 22.1.8

## Conclusion

Sedna is a capable portable Java RISC-V interpreter, but it is not a useful
performance peer for the current product path: it is approximately 30x slower
on the shared compute workload, far beyond the predeclared 2x rejection
threshold. Sedna will not be added to routine benchmark runs. The disposable
adapter and upstream checkout were removed; this dated report is the retained
qualification record.
