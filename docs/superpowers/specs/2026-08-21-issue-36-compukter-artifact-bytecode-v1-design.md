# Compukter Artifact and Typed Register Bytecode v1

> Issue: [#36](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/36)

## Status and ownership

This design was accepted on 2026-08-21 and corrected in place by [issue #41](https://github.com/CertifiedBadIdeas/Compukter-VM/issues/41) before artifact 1.0 shipped. It defines the portable boundary between the trusted Kotlin compiler service and the standalone Rust runtime. Kotlin IR, verifier proof state, decoded runtime structures, object layout, GC metadata, interpreter dispatch data, native pointers, SSA, JIT IR, and generated machine code are not part of this format.

Artifact bytes are untrusted. A host policy supplies maximum artifact bytes, section count, record counts, metadata string bytes, UTF-16 literal code units, code bytes, modules, functions, registers, blocks, imports, exception entries, capabilities, and debug bytes before decoding begins. A conforming loader may impose lower limits than the representable v1 maxima, but must report a structured limit diagnostic rather than truncate data.

## Compatibility contract

The container and bytecode use a `major.minor` version pair.

- A runtime accepts only its supported major version.
- A runtime may accept a greater minor version when every required section, semantic feature, type tag, and opcode is known.
- Unknown sections may be skipped only when neither `CRITICAL` nor `SEMANTIC` is set.
- Unknown opcodes, value types, nominal type tags, required sections, critical sections, and semantic features always reject the artifact.
- Reserved fields and padding bytes must be zero.
- A given logical v1 artifact has one canonical byte representation.

Container extension is intentionally more permissive than executable semantics. Optional source maps and future non-semantic metadata can be added without changing the bytecode major version; new execution behavior requires a known semantic feature and known opcodes.

## Integer, text, and identifier conventions

- Fixed-width integers are unsigned little-endian unless explicitly named signed.
- `f32` and `f64` constants use their raw IEEE 754 binary32/binary64 bits. NaN payloads are preserved.
- Text is strict UTF-8. Strings are not normalized by the loader; the compiler emits the exact canonical source/symbol spelling selected by the language backend.
- `u32::MAX` is the absent sentinel where this document permits absence. Other uses of the sentinel are invalid.
- Dense table IDs start at zero and contain no gaps.
- Module-local IDs occupy at most 31 bits. A serialized `SymbolRef` is a canonical ULEB128 `u32`: bit 31 clear selects a local table entry and bit 31 set selects an `ImportId` from the matching symbol kind.
- Canonical ULEB128 is limited to five bytes for `u32`. Redundant high zero groups, overflow, and unterminated encodings are invalid.
- Register IDs are fixed-width `u16`; `u16::MAX` is the absent destination sentinel only on instructions whose result is `unit`.

## Physical container

An artifact is one immutable byte buffer:

```text
64-byte header
32-byte section directory entries
zero padding to 8-byte alignment
8-byte-aligned section payloads
32-byte SHA-256 trailer
```

All offsets are absolute from byte zero. The complete file length is `payload_end + 32`. SHA-256 covers exactly bytes `[0, payload_end)`; the final 32 bytes contain the digest. There is no algorithm selector in v1. Integrity is not authenticity: artifact publication and capability authority remain trusted-host responsibilities.

### Header

| Offset | Size | Field | Canonical v1 value or meaning |
|---:|---:|---|---|
| 0 | 4 | `magic` | ASCII `CPKT` |
| 4 | 2 | `format_major` | `1` |
| 6 | 2 | `format_minor` | `0` |
| 8 | 2 | `min_runtime_abi_major` | minimum runtime ABI major |
| 10 | 2 | `min_runtime_abi_minor` | minimum runtime ABI minor |
| 12 | 2 | `header_size` | `64` |
| 14 | 2 | `directory_entry_size` | `32` |
| 16 | 4 | `section_count` | bounded directory count |
| 20 | 4 | `semantic_features` | known bit set described below |
| 24 | 8 | `directory_offset` | `64` |
| 32 | 8 | `payload_end` | first byte of the SHA-256 trailer |
| 40 | 4 | `entry_module_id` | dense artifact module ID |
| 44 | 4 | `entry_function_id` | local function ID in the entry module |
| 48 | 16 | `reserved` | all zero |

V1 feature bits are:

| Bit | Name | Meaning |
|---:|---|---|
| 0 | `EXCEPTIONS` | exception tables and `throw` are used |
| 1 | `COROUTINES` | coroutine instructions are used |
| 2 | `CAPABILITIES` | capability descriptors or calls are used |
| 3 | `MODULE_IMPORTS` | the bundle contains cross-module imports |

Bits 4 through 31 are unknown in v1 and cause rejection. A feature bit must be set if its feature occurs and must be clear otherwise.

### Section directory

Each entry is 32 bytes:

| Offset | Size | Field | Meaning |
|---:|---:|---|---|
| 0 | 2 | `kind` | section kind |
| 2 | 2 | `flags` | section flags |
| 4 | 4 | `scope` | `0` for bundle-global, otherwise `ModuleId + 1` |
| 8 | 8 | `offset` | aligned payload start |
| 16 | 8 | `length` | payload bytes |
| 24 | 4 | `element_count` | logical record count |
| 28 | 4 | `reserved` | zero |

Flag bit 0 is `CRITICAL`; bit 1 is `SEMANTIC`. Bits 2 through 15 are zero in v1. Every known core section is both critical and semantic except `DEBUG`, which has zero flags. Unknown sections must have zero flags to be skipped.

Directory entries are ordered by `(scope, kind)`, with no duplicate key. Global sections therefore precede module sections. Directory bytes end no later than the first payload. Payload offsets are multiples of eight, ranges do not overlap, and gaps contain only zero bytes. The first payload begins at `align8(64 + section_count * 32)`. Payloads are packed in directory order with only the zero padding required by `align8`.

Known section kinds are:

| Kind | Scope | Name |
|---:|---|---|
| `0x0001` | global | `MANIFEST` |
| `0x0002` | global | `MODULES` |
| `0x0003` | global | `CAPABILITIES` |
| `0x0100` | module | `STRINGS` |
| `0x0101` | module | `TYPES` |
| `0x0102` | module | `CONSTANTS` |
| `0x0103` | module | `IMPORTS` |
| `0x0104` | module | `EXPORTS` |
| `0x0105` | module | `FIELDS` |
| `0x0106` | module | `FUNCTIONS` |
| `0x0107` | module | `BLOCKS` |
| `0x0108` | module | `CODE` |
| `0x0109` | module | `EXCEPTIONS` |
| `0x010a` | module | `UTF16_LITERALS` |
| `0x0110` | module | `DEBUG` |

Kinds `0x8000..=0xffff` are reserved for optional non-semantic extensions. Unknown kinds below `0x8000` are rejected regardless of flags.

## Indexed section envelope

Every section except `MANIFEST` uses the same indexed envelope:

```text
u32 record_count
u32 reserved = 0
u64 record_bytes
u32 offsets[record_count + 1]
zero padding to 8-byte alignment
concatenated record bytes
```

The directory `element_count` equals `record_count`. `offsets[0]` is zero, offsets are monotonically increasing, and the last offset equals `record_bytes`. Offsets are relative to the first record byte. Empty records are allowed only where explicitly stated. Padding is zero. No record may refer outside its own section.

`CODE` uses the same envelope but each record corresponds one-to-one with a `BlockId` and consists solely of instruction bytes. `MANIFEST` is a single fixed record and has directory `element_count = 1`.

## Global sections

### MANIFEST

The fixed 112-byte v1 manifest is:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 4 | required heap bytes |
| 4 | 4 | required stack/frame bytes |
| 8 | 4 | maximum coroutines |
| 12 | 4 | maximum call depth per coroutine |
| 16 | 4 | maximum host requests |
| 20 | 4 | maximum event queue entries |
| 24 | 4 | maximum block fixed cost |
| 28 | 4 | minimum slice cost |
| 32 | 4 | required capability count |
| 36 | 4 | optional capability count |
| 40 | 32 | compiler/codegen ABI SHA-256 identity |
| 72 | 32 | target standard-library ABI SHA-256 identity |
| 104 | 8 | reserved zero |

Counts must match `CAPABILITIES`. Admission may reject requirements exceeding a device profile. `minimum slice cost` must be at least `maximum block fixed cost`, preventing permanent starvation on an admitted device.

### MODULES

Each module record is:

```text
u32 name_string_id_in_that_module
u32 flags                 // bit 0: application, bit 1: library
u8  semantic_sha256[32]
u32 import_count
u32 export_count
u32 type_count
u32 function_count
u32 reserved = 0
```

Exactly one module has the application flag and it equals `entry_module_id`. Module IDs are their record indices. A module semantic digest is SHA-256 over:

```text
ASCII "Compukter module v1\0"
for each known semantic module section in ascending kind order:
    u16 section kind
    u64 payload length
    exact section payload bytes
```

`DEBUG` and unknown optional extension sections are excluded. The global `MODULES` section is outside the digest, avoiding a circular hash. Matching module hashes may share immutable verified code and metadata; mutable globals and heap objects remain VM-owned.

### CAPABILITIES

Each record is:

```text
u32 namespace_string_id_in_entry_module
u32 name_string_id_in_entry_module
u16 abi_major
u16 minimum_abi_minor
u32 flags                 // bit 0: required, bit 1: optional
u32 operation_count
u32 reserved = 0
```

Exactly one of required or optional is set. Record order is lexicographic by the raw UTF-8 `(namespace, name, abi_major, minimum_abi_minor)`. `CapabilityId` is the record index and cannot be produced by guest arithmetic.

## Module sections

Every module contains exactly one of each core module section. Empty tables use an empty indexed envelope. `DEBUG` is optional.

### STRINGS

Each record is raw non-empty strict UTF-8 metadata without a terminator. Module, type, function, field, namespace, capability, and source-path names use this table; guest string literal content never does. Records are sorted lexicographically by raw bytes and deduplicated. The empty string, when required, is represented by a single empty record at index zero; no other empty record is valid.

### UTF16_LITERALS

Each record contains zero or more little-endian `u16` Kotlin string code units. Empty literals, embedded nulls, surrogate pairs, and isolated high or low surrogates are all valid. Record byte lengths must be even, and records are strictly sorted and deduplicated by their exact raw bytes. `Utf16LiteralId` is the module-local record index. The host applies an independent checked limit to the cumulative code-unit count before publishing decoded module state.

### Value types

A value type is an eight-byte structure used by signatures and register tables:

```text
u8  kind
u8  flags
u16 reserved = 0
u32 nominal_type_ref
```

Kinds are `0 unit`, `1 i32`, `2 i64`, `3 f32`, `4 f64`, `5 bool`, `6 char`, and `7 ref`. Primitive kinds require zero flags and `nominal_type_ref = u32::MAX`. A reference uses flag bit 0 for nullable and stores a local/import `TypeRef`; all other flag bits are zero. Null is a value admitted only by nullable `ref`; it is not integer zero. Nullable primitives are compiler-created box types and therefore use `ref`.

### TYPES

Nominal type record tags are:

- `0 CLASS`
- `1 INTERFACE`
- `2 ARRAY`
- `3 FUNCTION`

All records begin with:

```text
u8  tag
u8  flags
u16 generic_arity
u32 name_string_id
```

Class and interface records continue with:

```text
u32 super_type_ref_or_absent
u32 interface_count
u32 field_start
u32 field_count
u32 method_start
u32 method_count
u32 interface_type_refs[interface_count]
```

Class flag bit 0 means abstract and bit 1 means final. Interface flag bit 0 means sealed for this closed world. Interfaces have no instance fields and require zero field count. `super_type_ref_or_absent` is absent only for the root object and interfaces without a parent. Interface refs are sorted and unique.

An array record continues with one eight-byte value type and has zero generic arity. Its name is the canonical diagnostic name.

A function type record continues with:

```text
u16 parameter_count
u16 function_flags       // bit 0: suspending
ValueType result
ValueType parameters[parameter_count]
```

Generic parameters are erased to verifier-visible upper-bound reference types in executable signatures. `generic_arity` and names remain diagnostic/link metadata; reified operations are lowered to explicit type operands. The artifact does not serialize compiler IR generic graphs.

### CONSTANTS

Each record begins with `u8 tag` followed by the canonical payload:

| Tag | Constant | Payload |
|---:|---|---|
| 0 | `I32` | `i32` little-endian |
| 1 | `I64` | `i64` little-endian |
| 2 | `F32` | raw `u32` bits |
| 3 | `F64` | raw `u64` bits |
| 4 | `BOOL` | one byte `0` or `1` |
| 5 | `CHAR` | arbitrary UTF-16 code unit as little-endian `u16` |
| 6 | `STRING` | `u32 Utf16LiteralId` |
| 7 | `NULL` | no payload |

Constants are sorted first by tag and then by payload bytes and are deduplicated. Every `u16` bit pattern is a valid `CHAR`; a tag-5 record is exactly three bytes. Non-canonical booleans and out-of-range `Utf16LiteralId` values reject the artifact. Constants cannot encode refs other than null and verified string-literal metadata; they cannot encode capabilities, functions, types as values, pointers, or host handles. Heap materialization and string identity belong to the managed-heap design.

### IMPORTS and EXPORTS

An import record is:

```text
u8  symbol_kind          // 0 type, 1 function, 2 field
u8  reserved[3] = 0
u32 target_module_id
u32 target_name_string_id_in_target_module
u32 expected_signature_type_ref
u8  target_module_semantic_sha256[32]
```

Imports are sorted by `(target_module_id, symbol_kind, target_name bytes, expected signature)`. They must resolve exactly once against an export and the module hash must match. An export record is:

```text
u8  symbol_kind
u8  visibility          // 0 bundle, 1 public library
u16 reserved = 0
u32 name_string_id
u32 local_symbol_id
u32 signature_type_ref
```

Exports are sorted by `(symbol_kind, name bytes, signature)`. Duplicate resolution keys are invalid. Closed-world loading resolves every import before verification; there is no runtime name lookup or guest-controlled code loading.

The module import graph is acyclic. This makes semantic module hashes well-founded: hashes are computed from dependency leaves toward the application module. Cyclic source relationships must be linked into one module rather than represented as mutually importing artifact modules.

### FIELDS

A field record is:

```text
u32 owner_type_ref
u32 name_string_id
ValueType value_type
u32 flags                // bit 0: mutable, bit 1: static
u32 reserved = 0
```

Field IDs are nominal identities, not byte offsets. Object layout and static storage are runtime-derived. Fields are ordered by owner type and declaration order as emitted by the canonical linker.

### FUNCTIONS

A function record begins with:

```text
u32 owner_type_ref_or_absent
u32 name_string_id
u32 signature_type_ref
u32 flags                // bit 0 suspending, 1 static, 2 virtual, 3 abstract
u16 register_count
u16 parameter_count
u32 first_block
u32 block_count
u32 first_exception
u32 exception_count
ValueType registers[register_count]
```

Parameter values occupy registers zero through `parameter_count - 1`; instance methods place the receiver in register zero and include it in `parameter_count`. The signature and register types must agree. Abstract functions have zero blocks and cannot be the entry function. Non-abstract functions have at least one block; `first_block` is their entry.

### BLOCKS and CODE

A block record is fixed at 24 bytes:

```text
u32 owner_function_id
u32 code_record_id        // equal to this BlockId
u32 instruction_count
u32 declared_fixed_cost
u32 flags                 // bit 0: loop-header safepoint
u32 reserved = 0
```

The `CODE` record with the same `BlockId` contains exactly `instruction_count` framed instructions and no trailing bytes. Blocks belonging to a function are contiguous. A block has no implicit fallthrough and ends in exactly one terminator. A backedge targets a block with the loop-header flag and originates at a terminator boundary. Every backedge and every suspending terminator is a quota and GC safepoint.

### EXCEPTIONS

An exception record is:

```text
u32 owner_function_id
u32 first_protected_block
u32 protected_block_count
u32 catch_type_ref        // absent means catch-all cleanup handler
u32 handler_block
u16 exception_register
u16 reserved = 0
```

Protected ranges contain complete contiguous blocks and cannot be empty. Entries are sorted from innermost to outermost source nesting, then by catch order. Overlapping ranges must be properly nested. For each handler, the verifier intersects the initialized-register state immediately before every potentially throwing instruction covered by that handler; those registers, the function parameters, and `exception_register` are initialized on handler entry. Its exception register has a non-null reference type compatible with the catch. `finally` is compiler-lowered explicit control flow, not a runtime handler kind.

### DEBUG

Debug is non-semantic and may be absent. Each variable-length record maps one instruction ordinal:

```text
u32 function_id
u32 block_id
u32 instruction_index
u32 start_utf16_offset
u32 end_utf16_offset
u32 inline_parent_record_or_absent
u32 source_path_byte_length
u8  source_path_utf8[source_path_byte_length]
```

Records are sorted by `(function, block, instruction)`. Source paths are canonical project-relative slash-separated paths with no empty, `.` or `..` segments and no leading slash. UTF-16 offsets match Kotlin compiler diagnostics. Inline-parent references point to an earlier debug record, preventing cycles.

## Instruction framing

Each instruction is:

```text
u8 opcode
u8 form
u16 byte_length
operand bytes
```

`byte_length` includes the four-byte header, is at least four, and exactly matches the known operand schema. Instructions are packed without alignment padding inside a code record. `form` selects a value kind where allowed: `1 i32`, `2 i64`, `3 f32`, `4 f64`, `5 bool`, `6 char`, `7 ref`; zero means no type form. Arithmetic and bitwise opcodes `0x10..=0x1b` use the operated value kind. Ordered/equality primitive comparisons `0x20..=0x25` use the compared value kind. Reference equality opcodes `0x26..=0x27` require form `7`. Every other v1 opcode requires form zero. Unknown or mismatched form values reject the instruction.

Operands named `reg` are `u16`; IDs and counts inside instructions are canonical ULEB128 `u32`. `TypeRef`, `FunctionRef`, and `FieldRef` use the local/import high-bit representation before ULEB128 encoding. `BlockId`, `ConstantId`, `CapabilityId`, and `OperationId` are local dense IDs encoded directly as ULEB128. An `args` list is its ULEB128 count followed by that many `u16` registers. A switch case is a fixed little-endian `i32` followed by a ULEB128 `BlockId`. A `dst?` or `return value?` operand is always present as `u16`, with `u16::MAX` representing the absent value required by a `unit` signature. The maximum list length is checked before traversal.

A suspending terminator's destination is uninitialized on entry to the terminator and becomes initialized only on its normal `resume_block` edge. Exceptional completion follows the exception table from the suspending terminator's block. `yield` and `sleep` have no result. Coroutine handles are ordinary non-null refs to the target standard library's unforgeable coroutine-handle class; only `coroutine_spawn` creates them.

## Opcode table

The following table is the complete v1 opcode set. `dst?` uses `u16::MAX` only for a `unit` result. A type suffix means `form` must be one of the listed value kinds; otherwise form is zero.

| Opcode | Name | Operands | Type rule | Fixed cost |
|---:|---|---|---|---:|
| `0x00` | `nop` | none | none | 1 |
| `0x01` | `move` | `dst, src` | equal register types | 1 |
| `0x02` | `const` | `dst, ConstantId` | constant assignable to dst | 1 |
| `0x03` | `null` | `dst` | dst is nullable ref | 1 |
| `0x04` | `convert` | `dst, src` | numeric conversion named by src/dst types | 2 |
| `0x10` | `add` | `dst, lhs, rhs` | i32/i64/f32/f64 | 1 |
| `0x11` | `sub` | `dst, lhs, rhs` | i32/i64/f32/f64 | 1 |
| `0x12` | `mul` | `dst, lhs, rhs` | i32/i64/f32/f64 | 2 |
| `0x13` | `div` | `dst, lhs, rhs` | i32/i64/f32/f64 | 4 |
| `0x14` | `rem` | `dst, lhs, rhs` | i32/i64/f32/f64 | 4 |
| `0x15` | `neg` | `dst, src` | i32/i64/f32/f64 | 1 |
| `0x16` | `bit_and` | `dst, lhs, rhs` | i32/i64 | 1 |
| `0x17` | `bit_or` | `dst, lhs, rhs` | i32/i64 | 1 |
| `0x18` | `bit_xor` | `dst, lhs, rhs` | i32/i64 | 1 |
| `0x19` | `shift_left` | `dst, lhs, rhs` | lhs/dst i32 or i64; rhs i32 | 1 |
| `0x1a` | `shift_right` | `dst, lhs, rhs` | lhs/dst i32 or i64; rhs i32 | 1 |
| `0x1b` | `shift_unsigned` | `dst, lhs, rhs` | lhs/dst i32 or i64; rhs i32 | 1 |
| `0x20` | `equal` | `dst, lhs, rhs` | equal primitive types; dst bool | 1 |
| `0x21` | `not_equal` | `dst, lhs, rhs` | equal primitive types; dst bool | 1 |
| `0x22` | `less` | `dst, lhs, rhs` | numeric/char; dst bool | 1 |
| `0x23` | `less_equal` | `dst, lhs, rhs` | numeric/char; dst bool | 1 |
| `0x24` | `greater` | `dst, lhs, rhs` | numeric/char; dst bool | 1 |
| `0x25` | `greater_equal` | `dst, lhs, rhs` | numeric/char; dst bool | 1 |
| `0x26` | `ref_equal` | `dst, lhs, rhs` | compatible refs; dst bool | 1 |
| `0x27` | `ref_not_equal` | `dst, lhs, rhs` | compatible refs; dst bool | 1 |
| `0x30` | `new_object` | `dst, TypeRef` | concrete class assignable to dst | 4 |
| `0x31` | `new_array` | `dst, TypeRef, length_reg` | array type; length i32 | 4 |
| `0x32` | `array_length` | `dst, array` | dst i32; array non-null ref | 2 |
| `0x33` | `array_load` | `dst, array, index` | element assignable to dst; index i32 | 2 |
| `0x34` | `array_store` | `array, index, value` | value assignable to element; index i32 | 2 |
| `0x35` | `field_get` | `dst, receiver, FieldRef` | owner/value compatible | 2 |
| `0x36` | `field_set` | `receiver, FieldRef, value` | mutable field and compatible value | 2 |
| `0x37` | `static_get` | `dst, FieldRef` | static field and compatible dst | 2 |
| `0x38` | `static_set` | `FieldRef, value` | mutable static field | 2 |
| `0x39` | `is_type` | `dst, value, TypeRef` | dst bool; value ref | 2 |
| `0x3a` | `checked_cast` | `dst, value, TypeRef` | compatible reference families | 2 |
| `0x40` | `call_direct` | `dst?, FunctionRef, args` | exact signature | `4 + argc` |
| `0x41` | `call_virtual` | `dst?, FunctionRef, args` | receiver first; virtual signature | `5 + argc` |
| `0x42` | `call_interface` | `dst?, FunctionRef, args` | receiver first; interface signature | `6 + argc` |
| `0x50` | `coroutine_spawn` | `dst, FunctionRef, args` | suspending entry; dst coroutine ref | `6 + argc` |
| `0x51` | `cap_call_sync` | `dst?, CapabilityId, OperationId, args` | admitted descriptor signature | `5 + argc` |
| `0xe0` | `jump` | `BlockId` | terminator | 1 |
| `0xe1` | `branch` | `condition, true_block, false_block` | bool condition; terminator | 1 |
| `0xe2` | `switch_i32` | `key, default_block, (i32, BlockId) cases` | sorted unique cases; terminator | `1 + cases` |
| `0xe3` | `return` | `value?` | function result; terminator | 1 |
| `0xe4` | `throw` | `exception` | non-null throwable ref; terminator | 2 |
| `0xe5` | `call_suspend` | `dst?, FunctionRef, args, resume_block` | suspending signature; terminator | `5 + argc` |
| `0xe6` | `yield` | `resume_block` | suspending function; terminator | 2 |
| `0xe7` | `sleep` | `duration_i64, resume_block` | non-negative virtual duration; terminator | 3 |
| `0xe8` | `coroutine_join` | `dst?, coroutine, resume_block` | structured child; terminator | 4 |
| `0xe9` | `cap_call_async` | `dst?, CapabilityId, OperationId, args, resume_block` | async admitted operation; terminator | `6 + argc` |
| `0xff` | `unreachable` | none | compiler-proven dead terminator | 1 |

Allocation fixed cost is charged before allocation. `new_object` additionally charges the admitted allocation units for the derived object size. `new_array` additionally charges `length * admitted_element_units`, checked for overflow before mutation. Capability operations declare deterministic size-based dynamic charges in the admitted capability ABI. Dynamic work either precharges atomically or runs as bounded resumable chunks; it cannot partially publish an object or host request when budget is unavailable.

Integer division behavior, floating-point behavior, conversion overflow, array bounds, null checks, cast failure, and arithmetic exceptions follow the target Kotlin semantic ABI and are independent of host Rust release/debug behavior. This artifact specification records their typed operation; the runtime semantic issue owns executable conformance vectors.

## Register and control-flow verification contract

Each register has one static value type for the entire function and may be assigned repeatedly. The verifier tracks an initialized bit for every register at every program point.

- Parameters are initialized at function entry; other registers are not.
- Reading an uninitialized register is invalid.
- An instruction result initializes its destination after all trapping inputs have been read.
- At a control-flow join, initialized state is the intersection of predecessor out-states.
- Exception handlers use the state defined by their exception record rather than an ordinary predecessor union.
- Register types never merge or change. The compiler inserts typed `move` instructions before branches when different source values must occupy a common destination.
- Every target is a declared block in the same function.
- Every block is reachable from function entry or is targeted by an exception edge; otherwise it is rejected as non-canonical dead code.
- Calls, fields, types, capabilities, and coroutine identities must resolve through their typed tables. Integers cannot be cast to these identities or to `ref`.

The verifier recalculates each block fixed cost from the opcode table and requires exact equality with `declared_fixed_cost`. It rejects arithmetic overflow and a block above the manifest maximum. A runtime enters a block only when the entire fixed cost fits its remaining slice. Consequently an empty infinite loop repeatedly consumes its branch/backedge block cost and yields the VM between slices.

## Decoder and verifier diagnostics

Diagnostics are structured values with:

```text
stable code
artifact byte offset or absent
section kind or absent
module/function/block/instruction identities when known
bounded human-readable detail
```

The decoder owns container length, digest, directory, canonical integer/text, record envelope, local range, and limit failures. The verifier owns graph resolution, type compatibility, definite initialization, control flow, instruction schema, call/field/capability identity, exception nesting, and cost failures. Neither category becomes a catchable guest exception.

Representative stable code families are `CONTAINER_*`, `SECTION_*`, `LIMIT_*`, `MODULE_*`, `SYMBOL_*`, `TYPE_*`, `CODE_*`, `CFG_*`, `REGISTER_*`, `EXCEPTION_*`, `CAPABILITY_*`, and `COST_*`. The implementation issue assigns numeric codes while preserving these ownership boundaries.

## Required specification fixtures

The decoder/verifier implementation issue must derive committed binary fixtures from this specification:

1. A minimal application module whose entry function returns `unit`.
2. A two-module bundle with one typed function import/export and matching module hashes.
3. A class allocation, nullable ref branch, array, loop backedge, and caught exception artifact.
4. Coroutine spawn/sleep/join and synchronous/asynchronous capability-call instruction records.
5. An optional debug section demonstrating UTF-16 Kotlin source offsets and inline ancestry.

Negative mutations cover truncated header/trailer, bad digest, size overflow, excessive count, directory overlap, non-zero gap, duplicate section, non-canonical ULEB128, invalid UTF-8 metadata, odd-length or non-canonical UTF-16 literal records, malformed three-byte `CHAR` records, bad module hash, unresolved/ambiguous import, unknown critical or semantic section, unknown opcode, bad instruction length, jump outside a function, missing terminator, uninitialized register read, type-confused register, forged symbol identity, bad exception nesting, wrong fixed cost, and an unsafepointed backedge.

Fixtures must include a hand-decodable offset manifest containing every section start, length, record offset, module hash, and artifact hash. Re-encoding a decoded artifact must reproduce identical bytes.

### Canonical minimal vector A

Vector A fixes the exact canonical encoding for a one-module application whose static entry function returns `unit`. Its strings are records `0 = "app"` and `1 = "entry"`. Type zero is a non-suspending zero-parameter function returning `unit`. Function zero is static, owns no registers, and contains block zero. Block zero declares one instruction and fixed cost one. Its complete code record payload is the six instruction bytes `e3 00 06 00 ff ff`, meaning `return` with the absent/unit register sentinel. All other module tables and the global capability table are empty. Compiler and standard-library ABI identities are zero only for this format vector.

The vector has fourteen sections and this exact offset manifest:

| Kind | Scope | Offset | Length | Count |
|---:|---:|---:|---:|---:|
| `0x0001` | 0 | 512 | 112 | 1 |
| `0x0002` | 0 | 624 | 84 | 1 |
| `0x0003` | 0 | 712 | 24 | 0 |
| `0x0100` | 1 | 736 | 40 | 2 |
| `0x0101` | 1 | 776 | 44 | 1 |
| `0x0102` | 1 | 824 | 24 | 0 |
| `0x0103` | 1 | 848 | 24 | 0 |
| `0x0104` | 1 | 872 | 24 | 0 |
| `0x0105` | 1 | 896 | 24 | 0 |
| `0x0106` | 1 | 920 | 60 | 1 |
| `0x0107` | 1 | 984 | 48 | 1 |
| `0x0108` | 1 | 1032 | 30 | 1 |
| `0x0109` | 1 | 1064 | 24 | 0 |
| `0x010a` | 1 | 1088 | 24 | 0 |

Its 64-byte header is:

```text
43504b540100000001000000400020000e0000000000000040
000000000000005804000000000000000000000000000000
00000000000000000000000000000000
```

Its module semantic SHA-256 is:

```text
f1379df5fe4e751a1df57cf6be2d1575956f8c3e3ebaabe795820b44de2185ee
```

`payload_end` is 1112, the complete file length is 1144, and the trailing artifact SHA-256 is:

```text
23a3d933f13f78ac679e0cf10eca0355566f25e7e80a5937e45fb65ce8d06876
```

The two-module and language/runtime vectors are specified logically above and become checked binary goldens in the decoder/verifier implementation. Negative vectors are single named mutations of a valid golden; unless the mutation targets the digest check itself, their SHA-256 trailer is recomputed so validation reaches the intended stage.

## Loader pipeline

A conforming runtime performs these stages in order:

1. Apply the host artifact-byte limit to the immutable input buffer.
2. Validate fixed header fields, supported versions, `payload_end`, and exact file length.
3. Validate SHA-256 before interpreting directory-controlled payloads.
4. Validate directory count arithmetic, ordering, flags, scopes, ranges, alignment, gaps, and section presence.
5. Validate indexed envelopes, counts, offset tables, UTF-8, and local record shapes using bounded scans and checked arithmetic.
6. Allocate bounded decoded tables and resolve module hashes, imports, exports, types, signatures, and capability descriptors.
7. Run the semantic verifier and independently recalculate block costs.
8. Admit resources/capabilities against the device profile.
9. Publish immutable executable modules and create mutable VM instance state.

No executable or mutable VM state is observable before all stages succeed. Failure releases temporary loader memory and returns one bounded diagnostic set. The loader does not stream v1 artifacts; transport may stream into a host-limited buffer before stage one.

## Explicit exclusions

- No compression or encryption in the v1 container.
- No signature or trust-chain format; SHA-256 provides integrity/content identity only.
- No arbitrary runtime code loading, reflection-driven discovery, unresolved weak imports, or mutable code.
- No JVM bytecode, Java ABI, native pointer, heap offset, reference width, collector encoding, serialized SSA, or JIT/AOT code.
- No instruction-prefix mechanism for skipping unknown executable semantics.
- No requirement that debug-only changes preserve the complete artifact hash. Module semantic hashes intentionally exclude debug data.
- No concrete Rust public decoder API in this issue.

## Follow-up boundary

The immediate follow-up issue implements a bounded Rust decoder and semantic verifier against this document and its fixtures. The Kotlin IR backend follows only after the loader/verifier can reject malformed artifacts deterministically. Runtime execution issues may refine semantic conformance tests but must not silently change these bytes, identities, canonicalization rules, or compatibility guarantees.
