# Compukter VM

Compukter VM is the standalone managed Rust runtime used by
[Compukters](https://github.com/CertifiedBadIdeas/Compukters).

The platform compiles Kotlin projects through a pinned K2/Kotlin IR target into
versioned Compukter bytecode executed inside this resource-bounded VM.

The accepted architecture is tracked by
[Compukters issue #500](https://github.com/CertifiedBadIdeas/Compukters/issues/500).
Artifact, verifier, interpreter, heap, scheduler, snapshot, and optimization
contracts will be introduced through independently verified roadmap slices.

## Writable system storage

Each computer owns an ordinary writable root tree. `/boot/init` starts the system,
`/bin` contains bundled utilities, and `/home` contains user programs. `SystemImage`
is an installation source (`CPKTSYS\0`, version 1), not a readonly mount or live overlay.
The image seeds `/bin`, `/boot` and `/home`; it cannot contain paths inside `/home`.
A new computer installs once. Reopening restores saved files and deletions even when
another image is supplied. Explicit `install_system` replaces packaged paths in one
bounded flat journal batch, preserving other files. A file/directory conflict,
quota failure or queue rejection leaves the namespace unchanged. Existing executable
snapshots retain their old bytes. The store-level installation call requires the
caller to close that computer's active machine first and may wait for durable I/O.

File objects remain content-addressed in the world store. Live immutable contents
and fully verified execution images are weakly interned across computers; logical
quotas still charge each file independently. The registries keep every live identity
and remove their entries at final-owner release. Hash matches require byte equality.

This development change intentionally breaks old storage: namespace checkpoint and
journal version 2 persist all root descendants, logical computer checkpoint schema 8
includes the whole tree, and C ABI 25 adds `compukter_store_install_system`. Previous
ROM images, namespace versions and logical checkpoints are rejected explicitly.

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
Version preparation queries the latest complete stable GitHub Release through the
`gh` CLI: the published tag must have both platform bundles and the checksums asset.
Local tags, draft/prerelease metadata, partial uploads and API errors do not admit a bump.
Run bump commands with GitHub access and a clean checkout; tests use a deterministic
publication provider instead of bypassing this requirement.

After published `0.21.2`, `cargo xtask bump revision` prepares `0.21.3` once.
`cargo xtask bump abi` explicitly marks incompatible changes and prepares `0.22.0`,
from either `0.21.2` or pending `0.21.3`, updating the exported FFI constant itself.
Repeated revision or ABI requests keep the pending candidate unchanged until it is
published. A version outside those next-candidate states is rejected for review.
C ABI 21 therefore requires Runtime `0.21.0` or a compatible `0.21.x` revision;
changing an existing `v0.20.0` tag cannot repair it.

Every revision of one ABI must remain compatible in both directions for native calls,
executable admission, filesystem persistence and execution checkpoints. Checkpoint identity
includes the numeric ABI and logical schema, never the package revision. Breaking those
contracts requires an ABI bump. Before publication, the previous development checkpoint
identity based on the full version is replaced without a legacy fallback.
The shared verification script exchanges a full-computer checkpoint between the current
revision and an adjacent-revision build fixture in both directions, entirely offline.

Ordinary CI tests the workspace on Linux and Windows, builds both native transports,
checks the FFI ABI and Java 21 JNI calls, packages and inspects each bundle, then
verifies the complete two-platform set and its checksums. One `CI` workflow in
`.github/workflows/ci.yml` handles branch pushes, pull requests, Runtime tag pushes
and manual runs with the same workspace, native and bundle verification scripts
under `tools/runtime-bundler/`. A Runtime tag push additionally validates the tag
identity and publishes the verified assets through a separate release job. Only after
successful publication does another job prepare the next development revision on `main`
and push its version commit. A normal fast-forward push preserves concurrent changes;
a rejected push fails the preparation job while the published release remains available.
The standard workflow token's push does not trigger another CI run.
Manual dispatch validates the candidate without publishing; its optional `tag`
input checks a requested identity against `runtime-version.toml`.
It publishes a durable GitHub Release only for a pushed tag matching `v0.X.Y`.

Runs for the same commit share a concurrency queue (`queue: max`) without cancelling
an active or pending publication. The first run builds and verifies both platforms.
Later runs reuse `runtime-verified-<sha>` only from a successful push of the same
workflow, repository and exact commit, with a nonexpired artifact. Tag identity,
bundle manifests and the original checksums are checked again before publication.
If no reusable artifact remains, the run performs full verification itself; API,
download or bundle verification errors fail the run. Pull-request artifacts never
serve as a release source. Both event orders (branch then tag, or tag then branch)
use the same policy. Manual runs may also reuse a verified push result.

Local release preparation is explicit:

```text
cargo xtask check
cargo xtask bump revision
cargo xtask release
git push origin main v0.9.1
```

`bump` updates the canonical version files (and FFI constant for an ABI change) and creates a local commit only when a new candidate is needed.
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

Process capability ABI 3 adds asynchronous selector 3 `(String path, String encodedArgs) -> I32`
for explicit replacement. Success discards every task in the current frame and publishes one
`ProcessReplaced { previous, current }` outcome before any replacement instruction. Failure returns
an ordinary negative process status with a task-owned diagnostic and keeps the old execution alive.
Replacement receives a fresh scope, consumes the lifetime start budget, preserves stack depth and
steady aggregate reservations, and temporarily admits an extra session for rollback. Root replacement
updates the root artifact; nested replacement preserves the original parent's pending run. Native
C ABI 26 encodes tag 15 followed by old/new little-endian u64 scopes. ABI 2's three operations remain
admitted. Boot can replace itself with a shell instead of retaining a suspended boot heap.

Computer terminal input reserves Ctrl+C (key 67, CONTROL) to forcibly terminate the foreground command tree.
Termination runs at the next advance, emits ordered ProcessExited events and returns status 130 to its parent.
An idle shell receives Ctrl+C to cancel its input; the shell/root remains live; repeat events do not terminate another command. Existing terminal-key
FFI/JNI calls and C ABI layouts are unchanged.

## Shared immutable programs

Live byte-identical artifacts normally share their decoded metadata and one immutable
`PreparedProgram`: prepared operations and lifecycle descriptors, function/frame/safepoint/dispatch tables,
payload-relative fields, static layout, literals and type metadata. Every session still
checks its own execution profile and capability/record schemas and derives heap-header-dependent
allocation layouts and mutable resident charges. Heap, stack, statics, type-initialization
state, tasks and host requests remain private to the machine.

A digest-indexed weak registry interns artifacts only after full verification with the caller's
limits and complete-container equality. Every live identity remains discoverable; there is no
live-entry eviction. Final artifact release removes its index entry. Parsing/verification is still
repeated for each incoming binary. Concurrent first preparation may build temporary candidates
outside global locks, then publishes one canonical program via `OnceLock`.

Ordinary execution uses the shared typed prepared stream described below. Optional experimental
representations are cached by shared program/mode; their mutable decode cache stays machine-owned.
Checkpoints contain logical state and rebuild admission against current profile/bindings;
no cache addresses or native layout pointers are persisted.

## Default prepared execution

Verification still reads the existing portable artifact. Shared program preparation lowers its instructions
into one program-wide, dense stream of 16-byte operations with checked physical frame offsets. Only the
allocation boundary is 64-byte aligned; short blocks occupy ranges in the same stream without per-block
padding or separate code allocations. Lifecycle operations index a sparse resolved-descriptor table.
Prepared operations do not retain an additional resolved instruction descriptor in ordinary builds.
Decoded verified metadata remains available for source/type/verification identity.

The default path prepares signed/unsigned arithmetic and comparisons, integer bit/shift operations,
scalar constants and moves, inline component construction/extraction/copies, jumps/Boolean branches,
and scalar/reference field and array access. It reuses canonical numeric semantics and retains heap
reference/type/null/bounds checks. Static initialization, calls, allocation/GC, text/matrix work, switches,
exceptions, tasks, channels and capability suspension retain their existing lifecycle handlers where
not covered by these operations. Every operation preserves one logical instruction and its original
block cost, attempt/retirement, safepoint, source position and checkpoint boundary. No code fusion,
native JIT, SIMD data kernel or artifact/checkpoint/C ABI change is introduced.

## Experimental baseline JIT

`baseline-jit` enables an opt-in Cranelift 0.137.1 tier (Rust 1.97 or newer).
Production builds and sessions continue to use the prepared interpreter by default;
no executable, Runtime ABI or checkpoint format changes. This feature currently has
only private/benchmark selection, and is not enabled by the Minecraft carrier.

A prepared program lazily owns 256 direct-mapped hot-entry slots. Collisions
fall back without growing the table. After 64 shared entry visits, a bounded
plan of up to 128 numeric/frame/checked-heap instructions can enter the process-wide queue
(capacity eight, one compiler thread). Queue bursts retry at exponentially
increasing visit intervals up to eight times. Unsupported entries, compiler errors,
exhausted module capacity and final retry failure retain interpretation. No compile
job owns a Guest heap, frame, host binding or strong program reference.
Untraced entries prefer connected regions spanning at most 16 same-function blocks,
including loops. Admission prefers a reachable native cycle, or an acyclic plan
that removes at least two dispatcher transitions on every exit path. Short
bridges before lifecycle operations retain the prefix tier. These are generic
profitability rules; no Guest function or benchmark identity is special-cased.
Regions reuse the same slots, queue and module reservations; unsupported
successors exit to the ordinary executor before their block cost is paid.

At most 256 modules, including queued work and active owners, reserve up to 64 KiB
each of page-rounded executable/readonly/data requests: a conservative 16 MiB
mapping envelope. This excludes compiler transient allocations, its one persistent
worker/stack, Cranelift code in the host executable and program-side metadata.
Each finalized module is immutable and shared; the last owner explicitly frees
its mappings after all borrowed calls end. There is no eviction/recompilation tier.

Native entry proves every operand fits the current frame. Single-block prefixes
require sufficient attempt/headroom budget for the entire prefix; smaller budgets
use interpretation. Connected regions instead check attempt limits and accounting
overflow inside generated control flow and charge each entered block exactly once.
An invocation-local stack record publishes remaining budget, exact block/instruction,
attempts and fixed/block counters on every exit. Traced sessions retain the ordinary
trace-preserving path; no native state is persisted. Checked division, Bool/Char and branch
failures publish exact PC/attempts with preceding writes preserved. Floating point
retains canonical NaNs and separate multiply/add semantics. Non-allocating field
and array operations use typed checked Rust callouts within mixed prefixes. These
specialize the ordinary prepared heap executor by opcode, retaining reference,
type, bounds, nullability and admission-specific heap-layout checks. A scoped
invocation context borrows the current frame/image/heap; no instance address or
heap packing is embedded in shared code. A failed callout exits at its exact PC
with earlier frame and heap writes retained. Calls, allocation, initialization,
host operations and GC remain interpreter boundaries; there is no inlining across
those boundaries or automatic SIMD guarantee.
