# Compukter VM

Compukter VM is the standalone managed Rust runtime used by
[Compukter Kraft](https://github.com/CertifiedBadIdeas/Compukter-Kraft).

The project is in an intentional clean-break interval. The retired RISC-V
machine, ELF runtime, and native-code backends have been removed. The new
platform compiles Kotlin scripts through a pinned K2/Kotlin IR target into a
versioned Compukter bytecode executed only inside this resource-bounded VM.

The accepted architecture is tracked by
[Compukter-Kraft issue #500](https://github.com/CertifiedBadIdeas/Compukter-Kraft/issues/500).
Artifact, verifier, interpreter, heap, scheduler, snapshot, and optimization
contracts will be introduced through independently verified roadmap slices.

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
cargo fmt --check
cargo clippy --all-targets --all-features --locked --offline -- -D warnings
cargo test --locked --offline
cargo test --release --test bounded_failures --locked --offline
cargo doc --no-deps --document-private-items --locked --offline
```

## Compukter Kraft integration

Compukter Kraft consumes this repository as its pinned
`host/compukter-vm` submodule. Runtime changes are committed in the submodule
repository first; the consuming repository then records the selected submodule
commit.

The VM must remain independent of Minecraft, NeoForge, Kotlin compiler
internals, and files outside its repository checkout.
