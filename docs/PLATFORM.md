# Compukter RV32 Platform

The Compukter platform is a deterministic single-hart RV32 machine. Installed
controllers and their MMIO regions are immutable after construction. Host time
and future external attachment state change only through explicit embedding
APIs while the machine is not running.

## Machine timer

The standard platform always installs one machine timer at `0x1000_0200`.
Timer registers accept aligned 32-bit little-endian MMIO accesses only:

| Offset | Register |
|---:|---|
| `0x00` | `mtimecmp[31:0]` |
| `0x04` | `mtimecmp[63:32]` |
| `0x08` | `mtime[31:0]` |
| `0x0c` | `mtime[63:32]` |

Both registers are writable. `mtime` starts at zero and wraps modulo 2^64;
`mtimecmp` starts at `u64::MAX`. The level-triggered machine timer interrupt is
pending while `mtime >= mtimecmp` and is exposed as read-only `mip.MTIP`.
Guest software enables it with `mie.MTIE` and global `mstatus.MIE`.

This compact layout is the Compukter single-hart platform ABI. It is not a
CLINT or ACLINT compatibility claim.

The host advances deterministic time explicitly:

```rust
machine.advance_time(delta_ticks);
let now = machine.virtual_time();
```

`run()` never reads wall-clock time and never advances `mtime` implicitly. The
embedding application owns the mapping between its clock and timer ticks.

## WFI

`WFI` retires, advances the PC, and returns
`Rv32MachineOutcome::WaitingForInterrupt` when no individually enabled
interrupt is pending. Repeated `run()` calls return immediately without
spending instruction budget while the hart remains asleep.

Wakeup considers the individual pending/enable pair (`mip.MTIP` and
`mie.MTIE`) even when global `mstatus.MIE` is clear. Global `MIE` controls
whether wakeup also enters the interrupt handler. Machine timer interrupt
entry writes interrupt cause `0x8000_0007` to `mcause` and supports direct and
vectored `mtvec` modes.

## External interrupts

External interrupt aggregation, PLIC/`MEIP`, and device IRQ routing are not yet
part of this platform ABI. They are the next platform-infrastructure slice
before interrupt-driven UART and other peripheral controllers.
