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

## Local verification

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --locked --offline
```

## Compukter Kraft integration

Compukter Kraft consumes this repository as its pinned
`host/compukter-vm` submodule. Runtime changes are committed in the submodule
repository first; the consuming repository then records the selected submodule
commit.

The VM must remain independent of Minecraft, NeoForge, Kotlin compiler
internals, and files outside its repository checkout.
