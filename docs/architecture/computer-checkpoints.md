# Logical computer checkpoints

The checkpoint codec is currently internal. No C ABI, JNI/FFM, world-store publication, or Minecraft lifecycle entry point exposes restoration yet.

Rust owns the logical execution payload. It stores verified executable bytes for the root and every live child, execution profiles and process limits, admitted capability identities, machine and session state, terminal and canonical input, redstone waits, exact `/home` namespace generation and immutable file bytes, and open handle generations. Admission rebuilds execution images from verified artifacts and checks state against those images and trusted host configuration. Rust layouts, addresses, allocation capacities, and process-local machine identity are not persisted.

The internal envelope has a fixed 84-byte little-endian header: `CPKTHIB\0` (8 bytes), format version (u32, currently 1), runtime/schema identity (SHA-256, 32 bytes), computer ID (16 bytes), filesystem generation (u64), execution length (u64), and host descriptor length (u64). Execution and opaque host descriptor bytes follow, then SHA-256 over the complete header and both payloads. Runtime identity hashes the schema identity, a zero byte, and the native crate version. Any constituent codec meaning change requires a format/schema version change before persisted admission is exposed.

Readers reject unsupported identity/version, another computer ID, checksum failure, truncation, trailing bytes, inconsistent filesystem generation, oversized payloads, and allocation budget exhaustion. The execution decoder also applies depth limits and contextual validation. Host descriptor interpretation and resource rebinding remain host responsibilities; native decoding alone never authorizes dispatching saved external requests.

The full filesystem payload is self-contained. Reattaching a persistent filesystem requires the exact trusted namespace generation and contents. A newer filesystem must not silently replace the namespace belonging to saved execution. Atomic publication, consumption/reset policy, and retained world-save generations remain to be implemented before this format becomes a supported durable contract.
