# Host session baseline

Recorded on 2026-08-22 with the release profile (`codegen-units = 1`, thin
LTO), Rust 1.95.0 (`59807616e`), host `x86_64-unknown-linux-gnu`, Linux
7.1.8-zen1-3-zen, and CPU label `x86_64 local`.

The exact throughput command was:

```sh
COMPUKTER_BENCH_CPU="x86_64 local" cargo test --release --locked --offline execution::session_tests::host_session_performance_baseline -- --ignored --nocapture --test-threads=1
```

The unit-capability loop was admitted and started before timing. After 10,000
warmup exchanges, 100,000 measured exchanges each performed one `advance` to a
published request and one successful `resume(Unit)`.

| Exchanges | Elapsed ns | Request/resume pairs per second | Reserved mutable bytes |
| ---: | ---: | ---: | ---: |
| 100,000 | 14,728,563 | 6,789,529 | 2,245,504 |

The reservation figure is test-only accounting for the inline machine plus
its heap, frames, registers, statics, and the session's entry, argument, and
two UTF-16 arenas. Shared immutable image data and native allocator metadata
are excluded. The profile used 1 MiB each for heap and frame storage and 4,096
UTF-16 code units in each direction.

The hard allocation regression test performs 10,000 scalar `Unit` exchanges
and 10,000 four-code-unit managed UTF-16 responses after admission. Both
measured loops report exactly zero native allocations. Guest string objects
are allocated only inside the reserved managed heap.

The terminal conformance vector executes `write(String)`,
`writeLine(String)`, `readLine(): String`, and then returns the Kotlin/JVM hash
of the exact input code units. Debug and release produce the same observation:

| Fixed | Dynamic | Maintenance | Blocks | Instructions | Requests | Responses | Trace SHA-256 |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 23 | 6 | 0 | 4 | 6 | 3 | 3 | `51588960023fe3b9d4c1fe9535a512427d34a40af55740b9bc329d4f175552f4` |

Throughput is a local reference rather than a CI threshold. Exact outcomes,
counters, digest, request identity/order, UTF-16 payloads, stable polling, and
zero steady-state native allocations are test failures if they regress.
