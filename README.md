# Compukter VM

Compukter VM is the standalone deterministic RV32 virtual machine used by
[Compukter Kraft](https://github.com/CertifiedBadIdeas/Compukter-Kraft). It
implements the product RV32IMA_Zicsr_Zifencei ISA plus the complete ratified
RV32 Zbb 1.0 subset, ELF32/ILP32 execution model, bounded translation caches,
and the VM-owned performance suite. Because Zba and Zbs are not implemented
yet, `misa.B` remains zero and the VM does not claim the complete B extension.

## Local verification

```bash
cargo test --locked --offline
```

The stock-toolchain ELF contracts additionally require Clang and LLD:

```bash
bash scripts/tests/rv32-elf-boot-contract.sh
bash scripts/tests/rv32-elf-trap-contract.sh
bash scripts/tests/rv32-elf-atomic-contract.sh
bash scripts/tests/rv32-elf-zbb-contract.sh
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

To measure the effect of compiler-generated Zbb instructions against the same
current Cached DBT configuration:

```bash
bash scripts/tests/rv32-c-zbb-self-ab.sh
```

For focused measurements of only the current product-default Cached DBT:

```bash
cargo run --release --locked --offline --example rv32_c_comparison -- \
    product-default target/rv32-c-comparison 21
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
