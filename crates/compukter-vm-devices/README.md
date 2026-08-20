# Compukter VM Devices

`compukter-vm-devices` contains deterministic, bounded device models built on
the public `compukter-vm` MMIO API. The dependency is intentionally one-way:
the VM core does not know which concrete devices an embedding application
installs.

## UART16550

`Uart16550` is an interrupt-capable, 16550-style byte-stream controller. It is
compatible with the conventional register programming model needed by firmware
and console drivers, but it is not a cycle-accurate NS16550A implementation.

The model has:

- fixed 16-byte RX and TX arrays;
- one-byte effective queues while FIFO mode is disabled;
- no heap growth after construction;
- one active-high interrupt output for PLIC routing;
- explicit host connection, RX injection, and TX draining;
- deterministic drop and overrun diagnostics;
- no wall-clock, baud delay, background task, thread, or callback.

### Installation

The host selects a free MMIO address and the builder assigns a PLIC source:

```rust
use compukter_vm::rv32_machine::{Rv32MachineBuilder, Rv32MachineConfig};
use compukter_vm_devices::Uart16550;

# fn build(elf: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
let mut builder = Rv32MachineBuilder::from_elf(
    elf,
    Rv32MachineConfig {
        ram_size: 64 * 1024,
        debug_limit: 0,
        execution: Default::default(),
    },
)?;
let (uart, source) =
    builder.add_mmio_device_with_irq(0x1000_1000, Uart16550::new());
let mut machine = builder.build()?;

machine.device_mut(uart).unwrap().connect();
assert_eq!(source.get(), 1);
# Ok(())
# }
```

`0x1000_1000` is an example, not a mandatory address. In particular,
`0x1000_0000` cannot be used on the current Compukter platform because its
built-in control device occupies that address.

Topology is immutable after `build()`. Connection changes and byte transfers
must occur while `run()` is not active. A host mutation is visible to interrupt
delivery at the next non-zero-budget `run()` entry.

### Host attachment

```rust,ignore
let uart = machine.device_mut(uart_handle).unwrap();
uart.connect();

let received = uart.inject_rx(b"help\r\n");
assert_eq!(received.transferred + received.dropped, 6);

let mut output = [0_u8; 64];
let count = uart.drain_tx(&mut output);
consume(&output[..count]);
```

All transfers use caller-owned slices. `inject_rx` fills the bounded RX queue
without overwriting unread bytes and returns exact accepted/drop counts.
`drain_tx` removes at most the output slice length.

When disconnected, host RX input is rejected and counted. Guest TX bytes are
treated as physically transmitted into the void: they are counted as dropped,
do not occupy the TX queue, and do not make the transmitter unready.
Disconnecting also drops already queued TX bytes, while already received RX
bytes remain available to the guest.

`diagnostics()` returns saturating cumulative RX/TX drop counters plus the
current guest-visible receive-overrun latch. `clear_diagnostics()` resets only
the cumulative counters; it does not alter queues or acknowledge guest state.

## Guest MMIO ABI

The UART occupies eight bytes and accepts byte accesses only:

| Offset | Read | Write | Behavior |
|---:|---|---|---|
| `0` | RBR / DLL | THR / DLL | RX pop, TX push, or divisor low byte with DLAB |
| `1` | IER / DLM | IER / DLM | IRQ enables or divisor high byte with DLAB |
| `2` | IIR | FCR | Current interrupt reason or FIFO control |
| `3` | LCR | LCR | Line configuration and DLAB |
| `4` | MCR | MCR | Stored low control bits; no flow-control effect |
| `5` | LSR | ignored | RX/TX state and receive overrun |
| `6` | MSR | ignored | Connected CTS/DSR/DCD inputs; no delta bits |
| `7` | SCR | SCR | Guest scratch byte |

Other offsets and non-byte accesses return `MemoryFault` without partial
mutation.

### Reset and FIFO behavior

The UART resets disconnected with empty queues, interrupts disabled, divisor
zero, FIFO mode disabled, and `LSR.THRE | LSR.TEMT` set. An empty RBR read
returns zero.

FCR bit 0 selects FIFO mode. Changing it clears both queues. FCR bits 1 and 2
clear RX and TX respectively and self-clear. Bits 6 and 7 are stored as the RX
trigger selection and reflected by the IIR FIFO-identification bits.

Without a character-time model, waiting for a configured threshold could
strand a final partial FIFO forever. Therefore RX interrupt delivery is
asserted for every non-empty RX queue. A driver may receive an interrupt earlier
than its selected threshold, but never loses the final partial batch.

The divisor latch and LCR format bits have stable readback. They do not delay or
transform bytes in this version.

### Status

Supported LSR bits are:

- `DR` (`0x01`): RX is non-empty;
- `OE` (`0x02`): a connected RX injection overflowed; reading LSR clears it;
- `THRE` (`0x20`): the bounded model can accept another TX byte;
- `TEMT` (`0x40`): the host-visible TX queue is empty.

When connected, CTS, DSR, and DCD are set in MSR (`0xb0`). When disconnected,
MSR is zero. Modem delta state and modem interrupts are not implemented.

### Interrupts

IER implements:

- bit 0: received data available;
- bit 1: transmitter ready;
- bit 2: receiver line status.

Bit 3 (modem status) and upper bits read as zero. IIR uses standard reason
identifiers with fixed priority:

| Priority | Condition | IIR reason |
|---:|---|---:|
| 1 | Line-status IRQ enabled and OE latched | `0x06` |
| 2 | RX IRQ enabled and RX non-empty | `0x04` |
| 3 | TX IRQ enabled and TX can accept data | `0x02` |
| — | No condition | `0x01` |

Reading IIR identifies the current level condition; it does not consume a
separate event. Reading LSR acknowledges OE, reading RBR removes RX data,
filling TX removes TX readiness, host draining restores it, and IER can mask
each condition.

The guest services the UART before completing its PLIC claim. If any enabled
condition is still true after completion, the level remains asserted and the
PLIC re-pends the source.

## VirtIO-MMIO transport

`virtio::VirtioMmioDevice<D>` provides a reusable modern VirtIO-MMIO v2
transport for deterministic in-tree devices. `D` implements `VirtioDevice`;
the transport owns feature negotiation, one split virtqueue, descriptor
validation, used-ring publication, reset state, and an active-high interrupt
output.

The transport deliberately has a small, bounded contract:

- `VIRTIO_F_VERSION_1` is mandatory and legacy mode is rejected;
- exactly one queue (index 0), with a maximum of 128 descriptors;
- queue size must be a non-zero power of two;
- direct descriptors only: no indirect tables, packed rings, `EVENT_IDX`,
  notification data, or multiple queues;
- descriptor parsing uses fixed transport-owned scratch storage;
- the steady-state notify/completion path performs no heap allocation;
- all queue metadata and data buffers must be wholly inside RV32 RAM and must
  not overlap each other.

Malformed runtime queue state sets `DEVICE_NEEDS_RESET`, raises the
configuration interrupt, and leaves the failing request unpublished. Earlier
requests completed by the same notification remain visible in the used ring.
Writing zero to `Status` resets both transport and concrete-device state.

The host installs the transport like any other interrupt-capable device:

```rust,ignore
let (disk, source) = builder.add_mmio_device_with_irq(
    0x1000_2000,
    VirtioMmioDevice::new(MyVirtioDevice::new())?,
);
```

The address is selected by the embedding platform. The current acceptance
fixture uses `0x1000_2000`; it is not hard-coded by the transport.

### VirtIO-MMIO header

Header registers require aligned 32-bit accesses. Device-specific
configuration begins at offset `0x100` and preserves the guest access width
when delegated to `VirtioDevice`.

| Offset | Register | Behavior |
|---:|---|---|
| `0x000` | MagicValue | `0x74726976` |
| `0x004` | Version | modern transport version 2 |
| `0x008` | DeviceID | supplied by `VirtioDevice` |
| `0x00c` | VendorID | `COMP` |
| `0x010` / `0x014` | DeviceFeatures / Sel | two 32-bit feature banks |
| `0x020` / `0x024` | DriverFeatures / Sel | accepted driver feature banks |
| `0x030` | QueueSel | only queue 0 exists |
| `0x034` | QueueNumMax | 128 for queue 0, otherwise 0 |
| `0x038` | QueueNum | bounded power-of-two queue size |
| `0x044` | QueueReady | validates and freezes queue configuration |
| `0x050` | QueueNotify | processes queue 0 when the driver is ready |
| `0x060` / `0x064` | InterruptStatus / ACK | bit 0 used-ring, bit 1 configuration |
| `0x070` | Status | cumulative VirtIO device status; zero resets |
| `0x080..0x0a4` | Queue addresses | low/high descriptor, available, and used addresses |
| `0x0fc` | ConfigGeneration | currently zero |

Without `EVENT_IDX`, queue metadata occupies exactly `16 * n` descriptor
bytes, `4 + 2 * n` available-ring bytes, and `4 + 8 * n` used-ring bytes.
The required alignments are 16, 2, and 4 bytes respectively.

## Deliberate omissions

This version does not emulate serialized waveforms, baud time, parity/framing
errors, break timing, RX timeout interrupts, modem delta state, RTS/CTS flow
control, loopback, DMA, snapshots, hot controller installation, legacy VirtIO,
packed queues, or transport-level multi-queue operation.
