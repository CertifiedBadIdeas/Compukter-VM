# Compukter VM

Compukter VM is the standalone managed Rust runtime used by
[Compukters](https://github.com/CertifiedBadIdeas/Compukters).

The platform compiles Kotlin projects through a pinned K2/Kotlin IR target into
versioned Compukter bytecode executed inside this resource-bounded VM.

The accepted architecture is tracked by
[Compukters issue #500](https://github.com/CertifiedBadIdeas/Compukters/issues/500).
Artifact, verifier, interpreter, heap, scheduler, snapshot, and optimization
contracts will be introduced through independently verified roadmap slices.

Artifacts can be decoded, verified, admitted into public host-neutral
`Session`s, and executed by the Tier 0 interpreter. Capability operations
suspend as typed requests and resume with bounded host responses; adapters own
all external I/O and scheduling policy.
Artifact 1.0 models Kotlin `Char` as one arbitrary UTF-16 code unit and keeps
guest string literals in a bounded `UTF16_LITERALS` pool separate from strict
UTF-8 metadata. The runtime materializes literals and host responses into its
managed compact Latin-1/UTF-16 string layout.
The accepted semantics and implementation evidence are tracked in
[#38](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/38),
[#39](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/39), the
[#41 text ABI correction](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/41), the
[#43 host session boundary](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/43), the
[execution design](docs/superpowers/specs/2026-08-22-issue-38-deterministic-tier0-execution-semantics-design.md),
[implementation plan](docs/superpowers/plans/2026-08-22-issue-39-tier0-scalar-control-interpreter.md),
and [release baseline](docs/performance/tier0-baseline.md).
The public execution boundary is documented in the
[host-neutral session contract](docs/architecture/host-neutral-session-api.md),
with its [release baseline](docs/performance/host-session-baseline.md).

## Verified artifact loading

Artifact bytes enter the VM as an immutable `Arc<[u8]>`. The caller also
supplies an explicit `ArtifactLimits` value; its defaults are conservative
parser bounds, not a device admission profile. A server should derive stricter
limits from the concrete computer tier before loading an artifact.

```rust,no_run
use std::sync::Arc;

use compukter_vm::{verify_artifact, ArtifactLimits, DiagnosticSet, VerifiedArtifact};

fn load(bytes: Arc<[u8]>) -> Result<VerifiedArtifact, DiagnosticSet> {
    let limits = ArtifactLimits::default();
    verify_artifact(bytes, limits)
}
```

Only `VerifiedArtifact` crosses the public loading boundary. Decoded records,
partially verified tables, and their constructors remain internal. Verification
proves that the container is structurally and semantically valid and that its
SHA-256 trailer matches; the digest provides integrity, not publisher
authenticity. Trust policy, signatures, device admission, cache ownership, and
execution are separate host responsibilities. In particular, successful
verification does not reserve device resources or start guest code.

## Local verification

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings
cargo test --workspace --locked --offline
cargo test --release --test bounded_failures --locked --offline
cargo doc --workspace --no-deps --document-private-items --locked --offline
```

## Managed object storage

Allocated blocks have an eight-byte allocator header and a four-byte runtime
type identifier, followed by user payload. Blocks remain eight-byte aligned
with a 16-byte minimum. Empty and one-Int objects, including managed Int
boxes, occupy 16 bytes. Two-Int and three-Int objects both occupy 24 bytes.
Sixteen-byte free tails are split and reused; only smaller tails are absorbed.
Payload access uses little-endian byte reads and writes; wide fields do not
require the payload address itself to be eight-byte aligned.

Managed references are direct non-moving offsets. Reference equality and
aliases do not require a separately stored identity token. The unused
allocation token and ordinal counter have been removed; no Guest identity
hash operation is currently exposed by the VM instruction set.
Free-list links overlap the type identifier and first payload word only for
free blocks. Collection continues to borrow the predecessor-size word for
the gray queue and restores it during the bounded forward sweep.

## Native Runtime bundles

Compukter Runtime releases and Rust workspace packages use one pre-1.0 SemVer
`0.x.y`:

- `0` marks the runtime as pre-1.0;
- `x` is exactly the exported FFI ABI returned by `compukter_abi_version`;
- `y` is incremented for a compatible implementation replacement;
- an ABI break changes `x` and resets `y` to zero.

A Runtime `0.10.0` release uses tag `v0.10.0`. The first supported targets are Linux
x86_64 (`x86_64-unknown-linux-gnu`) and Windows x86_64
(`x86_64-pc-windows-msvc`). Each release contains these immutable assets:

```text
compukter-runtime-0.10.0-linux-x86_64.tar.gz
compukter-runtime-0.10.0-windows-x86_64.zip
compukter-runtime-0.10.0-checksums.sha256
```

Each platform archive has one fixed, self-describing layout containing both
supported JVM transports:

```text
native/<platform FFI library>
native/<platform JNI library>
manifest.json
LICENSE.txt
NOTICE.txt
```

The manifest binds the Runtime version and tag, full VM commit, FFI ABI,
external format versions, pinned Rust compiler, target, release profile, and
the canonical filename, byte size, and SHA-256 of each transport library.
Consumers must pin both the exact Runtime release and the exact VM commit from
that manifest. Published assets are
immutable; a compatible replacement is a new revision such as `0.10.1`, never
an overwrite of `0.10.0`.

Before preparing a tag, run `cargo xtask check` to verify that the canonical
Runtime version, workspace package versions, lockfile, and exported FFI ABI agree.
After an exported ABI increment, `cargo xtask bump abi` aligns those version files
and creates a local commit. C ABI 21 therefore requires Runtime `0.21.0` or a
compatible `0.21.x` revision; changing an existing `v0.20.0` tag cannot repair it.

Every revision of one ABI must remain compatible in both directions for native calls,
executable admission, filesystem persistence and execution checkpoints. Checkpoint identity
includes the numeric ABI and logical schema, never the package revision. Breaking those
contracts requires an ABI bump. Before publication, the previous development checkpoint
identity based on the full version is replaced without a legacy fallback.
The shared verification script exchanges a full-computer checkpoint between the current
revision and an actual `cargo xtask bump revision` build in both directions.

Ordinary CI tests the workspace on Linux and Windows, builds both native transports,
checks the FFI ABI and Java 21 JNI calls, packages and inspects each bundle, then
verifies the complete two-platform set and its checksums. One `CI` workflow in
`.github/workflows/ci.yml` handles branch pushes, pull requests, Runtime tag pushes
and manual runs with the same workspace, native and bundle verification scripts
under `tools/runtime-bundler/`. A Runtime tag push additionally validates the tag
identity and publishes the verified assets through a separate release job.
Manual dispatch runs all verification without publishing; its optional `tag`
input checks a requested identity against `runtime-version.toml`.
It publishes a durable GitHub Release only for a pushed tag matching `v0.X.Y`.
Local release preparation is explicit:

```text
cargo xtask check
cargo xtask bump revision
cargo xtask release
git push origin main v0.9.1
```

`bump` updates every canonical version file and creates a local commit.
`release` runs formatting, Clippy, and workspace tests before creating a local
annotated tag. Neither command pushes or publishes anything. Archive names keep
the descriptive `compukter-runtime-*` prefix; GitHub publishes only after the
maintainer pushes the prepared `v0.X.Y` tag.

## Compact source paths

Source builds after Compukters issue [#704](https://github.com/CertifiedBadIdeas/Compukters/issues/704)
accept both legacy inline-path DEBUG and compact DEBUG selected by the module
DEBUG_PATHS section (`0x0111`). The indexed pool contains unique canonical UTF-8
relative paths in first-use order; every entry is referenced. Compact DEBUG
records are exactly seven little-endian u32 fields, with the final field holding
a pool index. Source positions and inline parents retain their existing meaning.
Pool bytes count against debug limits and are shared as artifact byte ranges.

DEBUG_PATHS is critical but nonsemantic (flags 1): older readers reject it,
while module semantic hashes and container format 3.0 stay unchanged. Semantic
Runtime ABI 1.15 and native C ABI 20 do not signal this reader capability.
Published 0.20.0 bundles lack it; consumers need updated source-built natives
until a new immutable Runtime release is published and pinned.

## Safepoint root ranges

Source builds after Compukters [#706](https://github.com/CertifiedBadIdeas/Compukters/issues/706)
also accept the critical nonsemantic SAFEPOINT_ROOT_RANGES module marker
(`0x0112`, flags 1, count 1). Its raw payload is encoding version 1 (u32), expanded
row count (u32) and canonical expanded indexed payload length (u64). It selects
range records in SAFEPOINT_ROOTS: function, block, first boundary and positive
run count (u32), reference count and reserved zero (u16), then reference pairs.

Admission reconstructs every boundary map with checked block bounds, counts,
per-function limits and a cumulative expanded-root byte budget against the
artifact-byte policy. Legacy decoding remains supported. Module hashing streams
the canonical expanded legacy payload to preserve imports and module identities.
GC execution and checkpoint coverage are unchanged. Older readers reject the
marker; updated source-built natives are needed until a new Runtime is published.

## Golden fixtures

The committed artifact v1 compatibility set lives in `tests/fixtures/`:

- `vector-a.cpkt` is the canonical minimal executable artifact;
- `two-module.cpkt` covers a resolved cross-module import;
- `language-runtime.cpkt` covers object and array operations, nullable
  references, control flow, a loop safepoint, and exception handling;
- `debug.cpkt` covers UTF-16 source ranges and inline ancestry;
- `host-runtime.code` covers coroutine and synchronous/asynchronous capability
  instructions as an indexed CODE payload.

Each artifact has a committed Markdown manifest; the host-runtime payload has a
matching manifest as well. Ordinary tests only read these files and prove that
their independently generated bytes, manifests, public verification results,
and decoded canonical encodings remain unchanged.

Golden files are rewritten only by this explicit ignored test:

```bash
cargo test --test golden_fixtures regenerate_committed_fixtures --locked --offline -- --ignored --exact
```

Review regenerated binaries and their manifests together before committing
them. CI intentionally runs only the read-only golden suite.

## Compukters integration

Compukters consumes this repository as its pinned
`host/compukter-vm` submodule. Runtime changes are committed in the submodule
repository first; the consuming repository then records the selected submodule
commit.

The VM must remain independent of Minecraft, NeoForge, Kotlin compiler
internals, and files outside its repository checkout.

Computer terminal input reserves Ctrl+T (key 84, CONTROL) to forcibly terminate the foreground command tree.
Termination runs at the next advance, emits ordered ProcessExited events and returns status 130 to its parent.
The shell/root is preserved when idle; repeat events do not terminate another command. Existing terminal-key
FFI/JNI calls and C ABI layouts are unchanged.

## Production-path Guest benchmark

The opt-in `guest-benchmark` binary compiles the same private verifier and
interpreter modules without unit-test instrumentation. It uses the existing
untraced session path, checks a traced control for identical deterministic work,
and validates the `ready` marker, checksum, termination and sample counters.
It introduces no public runtime API and is not part of production Runtime bundles.

```sh
CARGO_PROFILE_RELEASE_DEBUG=1 cargo build --release --locked --offline \
  --features guest-benchmark --bin guest-benchmark
./target/release/guest-benchmark ARTIFACT_DIR REPORT_DIR 7 16384 untraced
```

`ARTIFACT_DIR` must contain a generated TSV manifest with `id` and `checksum`
columns plus verified `.cpkt` measurement artifacts that print `ready` before
an operation and the checksum afterward. Use the parent Compukters benchmark
artifact generators. Supply the requested Guest heap explicitly (for example,
16384 for loops or 262144 for collection reuse). This benchmark does not lower
artifact admission limits or silently fall back to another heap budget.
An optional final comma-separated list of ID prefixes selects profile workloads.
Use `traced` as the fifth argument for a diagnostic-trace timing control.
Samples must be an odd number of at least three. The harness warms each case,
reverses order on alternate rounds, excludes verification/admission/start from
timers and records construction and operation separately in `samples.tsv` and
`measurements.tsv`. Operation includes the final scan and output text copy.

On 64-bit Linux, additional `create_cpu_ns` and `hot_cpu_ns` sample columns
and their summary medians measure `CLOCK_THREAD_CPUTIME_ID`. These exclude
descheduled time; CPU frequency, cache contention and other host activity still
affect them. Other platforms leave these columns empty. Existing wall-time and
work-counter columns retain their meaning. Compare identical harness builds,
workloads and budgets, and retain both clocks when the host is busy.

Profile separately from ordinary timing. With symbols enabled, for example:

```sh
perf record -e cycles:u -F 499 --call-graph dwarf -o profile.data -- \
  ./target/release/guest-benchmark ARTIFACT_DIR REPORT_DIR 99 16384 untraced while-indexed
perf report -i profile.data --stdio
```

Profiles also include warmup/control, setup and final termination; use sampling
symbols to identify hot execution and keep profiled samples out of timing claims.
