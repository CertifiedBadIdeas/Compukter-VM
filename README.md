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

## Native Runtime bundles

Compukter Runtime releases and Rust workspace packages use one pre-1.0 SemVer
`0.x.y`:

- `0` marks the runtime as pre-1.0;
- `x` is exactly the exported FFI ABI returned by `compukter_abi_version`;
- `y` is incremented for a compatible implementation replacement;
- an ABI break changes `x` and resets `y` to zero.

Runtime `0.5.1` is tagged `v0.5.1`. The first supported targets are Linux
x86_64 (`x86_64-unknown-linux-gnu`) and Windows x86_64
(`x86_64-pc-windows-msvc`). Each release contains these immutable assets:

```text
compukter-runtime-0.5.1-linux-x86_64.tar.gz
compukter-runtime-0.5.1-windows-x86_64.zip
compukter-runtime-0.5.1-checksums.sha256
```

Each platform archive has one fixed, self-describing layout:

```text
native/<platform library>
manifest.json
LICENSE.txt
NOTICE.txt
```

The manifest binds the Runtime version and tag, full VM commit, FFI ABI,
external format versions, pinned Rust compiler, target, native filename, byte
size, SHA-256, and release profile. Consumers must pin both the exact Runtime
release and the exact VM commit from that manifest. Published assets are
immutable; a compatible replacement is a new revision such as `0.5.1`, never
an overwrite of `0.5.0`.

The `Runtime release` workflow may be dispatched manually to build and test
temporary Linux and Windows artifacts. It publishes a durable GitHub Release
only for a pushed tag matching `v0.X.Y`. Local release preparation is explicit:

```text
cargo xtask check
cargo xtask bump revision
cargo xtask release
git push origin main v0.5.2
```

`bump` updates every canonical version file and creates a local commit.
`release` runs formatting, Clippy, and workspace tests before creating a local
annotated tag. Neither command pushes or publishes anything. Archive names keep
the descriptive `compukter-runtime-*` prefix; GitHub publishes only after the
maintainer pushes the prepared `v0.X.Y` tag.

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
