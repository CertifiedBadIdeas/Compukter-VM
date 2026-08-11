# Compukter VM

Compukter VM is the standalone deterministic RV32 virtual machine used by
[Compukter Kraft](https://github.com/CertifiedBadIdeas/Compukter-Kraft). It
implements the product RV32IMA_Zicsr_Zifencei, ELF32/ILP32 execution model,
bounded translation caches, and the VM-owned performance suite.

## Local verification

```bash
cargo test --locked --offline
```

The stock-toolchain ELF contracts additionally require Clang and LLD:

```bash
bash scripts/tests/rv32-elf-boot-contract.sh
bash scripts/tests/rv32-elf-trap-contract.sh
bash scripts/tests/rv32-elf-atomic-contract.sh
```

## Benchmarks

The complete-machine benchmark has no external tool dependency:

```bash
cargo run --release --locked --offline --example rv32_machine_benchmarks -- 1000 21 7
```

The shared native, QEMU, and product comparison is intentionally strict. It
requires `clang`, `ld.lld`, LLVM inspection tools, and `qemu-system-riscv32`:

```bash
bash scripts/tests/rv32-c-qemu-comparison.sh
```

Its C kernel, linker scripts, and startup code live in
`benchmarks/rv32-c-comparison`. Generated benchmark artifacts are local to
`target/` and are not committed.

## Compukter Kraft integration

Compukter Kraft consumes this repository as the pinned
`host/compukter-vm` submodule. In the mod repository, initialize it with:

```bash
git submodule update --init --recursive
```

The VM does not read sources, assets, scripts, or build outputs from the parent
repository.
