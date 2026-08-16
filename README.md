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

## Compukter Kraft integration

Compukter Kraft consumes this repository as the pinned
`host/compukter-vm` submodule. In the mod repository, initialize it with:

```bash
git submodule update --init --recursive
```

The VM does not read sources, assets, scripts, or build outputs from the parent
repository.

## Platform ABI

The built-in MMIO layout, deterministic machine timer, and `WFI` host contract
are documented in [docs/PLATFORM.md](docs/PLATFORM.md).
