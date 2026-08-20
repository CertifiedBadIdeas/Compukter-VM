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

Wakeup considers individually enabled pending interrupts even when global
`mstatus.MIE` is clear. This currently includes the timer pair
`mip.MTIP`/`mie.MTIE` and external pair `mip.MEIP`/`mie.MEIE`. Global `MIE`
controls whether wakeup also enters an interrupt handler. Machine timer
interrupt entry writes `0x8000_0007` to `mcause`; machine external interrupt
entry writes `0x8000_000b`. Both support direct and vectored `mtvec` modes.

When timer and external interrupts are simultaneously actionable, the machine
external interrupt is taken first.

## External interrupts

The standard platform installs a ratified PLIC 1.0-compatible, single-hart,
machine-mode controller at `0x0c00_0000`. This base matches QEMU `virt` to
reduce driver porting, but Compukter does not claim compatibility with the rest
of that board.

PLIC registers accept aligned 32-bit little-endian MMIO accesses only. Its
single-context region has size `0x201000`:

| Offset | Register block |
|---:|---|
| `0x000000` | Source priorities; source 0 is reserved |
| `0x001000` | Read-only pending bits |
| `0x002000` | Enable bits for machine context 0 |
| `0x200000` | Priority threshold for machine context 0 |
| `0x200004` | Claim/complete for machine context 0 |

Reserved and unassigned registers in this window read as zero and ignore
writes. Priority and threshold are three-bit WARL values from 0 through 7.
Priority 0 disables delivery; a source is eligible only when its priority is
strictly greater than the context threshold. The highest priority wins, with
the lowest source ID breaking ties.

Reading claim returns the winning source ID, clears its pending bit, and marks
the gateway in flight. Writing that ID to the same register completes it. A
level source that is still asserted after completion immediately becomes
pending again. Deassertion does not discard a request that was already
accepted as pending. Invalid and non-in-flight completion IDs are ignored.

Hosts install an interrupt-capable device before machine construction:

```rust
let (device, irq) = builder.add_mmio_device_with_irq(base, device);
assert_eq!(irq.get(), 1);
```

IRQ sources are assigned densely from 1 in interrupt-device insertion order.
The built machine accepts at most 1023 sources. `Rv32PlicSource` is
guest-visible metadata for drivers and future platform descriptions; it is not
a mutable interrupt handle. The device exposes its active-high level through
`MmioDevice::interrupt_level()`.

Topology is immutable after `build()`. Host mutations made through
`machine.device_mut(handle)` are sampled at the next non-zero-budget `run()`
entry. Guest MMIO mutations are sampled after the MMIO slow path. A wrapping
bus MMIO epoch prevents interpreted non-MMIO instructions from rescanning IRQ
routes, and generated DBT code contains no per-instruction IRQ poll. Host state
must not be mutated concurrently with `run()`.

## Optional concrete devices

The core `compukter-vm` crate installs only its standard control, debug, timer,
and PLIC devices. Reusable optional controllers live in the one-way-dependent
`compukter-vm-devices` workspace crate, keeping the VM core independent of
concrete peripherals.

The first optional controller is a bounded, interrupt-capable 16550-style UART.
It is installed with `add_mmio_device_with_irq` at a host-selected free address;
`0x1000_1000` is used by current examples and tests. The QEMU-typical UART
address `0x1000_0000` is unavailable because the Compukter control device
already owns it. UART register, FIFO, attachment, and interrupt behavior is
specified in `crates/compukter-vm-devices/README.md`.

The devices crate also provides a modern VirtIO-MMIO v2 transport. It is not a
fixed standard-platform device: the embedding application chooses its address,
concrete `VirtioDevice`, and PLIC source by installing it with
`add_mmio_device_with_irq`. The current transport exposes one bounded split
queue of at most 128 descriptors, requires `VIRTIO_F_VERSION_1`, and performs
no heap allocation while processing warmed steady-state notifications. Its
register and queue-validation ABI is specified in
`crates/compukter-vm-devices/README.md`.

## Host inspection

Trusted embedding tools may call `Rv32Machine::inspection_snapshot()` between
`run()` calls. The returned fixed-size value copies the hart registers and
machine CSRs, timer, PLIC sources, immutable IRQ routes and sampled line levels,
control status, and backend statistics. Repeated inspection performs no heap
allocation and does not acknowledge, claim, clear, synchronize, or otherwise
mutate guest-visible state.

Inspection is deliberately absent from the guest ABI. It is also not a
versioned persistence snapshot and cannot be restored. Guest platform discovery
and future machine save states remain separate contracts.
