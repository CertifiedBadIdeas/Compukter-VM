use sha2::{Digest, Sha256};

const HEADER_SIZE: usize = 64;
const DIRECTORY_ENTRY_SIZE: usize = 32;
const DIGEST_SIZE: usize = 32;

fn align8(value: usize) -> usize {
    (value + 7) & !7
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn indexed(records: &[&[u8]]) -> Vec<u8> {
    let record_bytes = records.iter().map(|record| record.len()).sum::<usize>();
    let mut bytes = Vec::new();
    push_u32(&mut bytes, records.len() as u32);
    push_u32(&mut bytes, 0);
    push_u64(&mut bytes, record_bytes as u64);
    let mut offset = 0_u32;
    push_u32(&mut bytes, offset);
    for record in records {
        offset += record.len() as u32;
        push_u32(&mut bytes, offset);
    }
    bytes.resize(align8(bytes.len()), 0);
    for record in records {
        bytes.extend_from_slice(record);
    }
    bytes
}

pub(crate) fn minimal_vector() -> Vec<u8> {
    vector_with_strings(&[b"app", b"entry"])
}

#[allow(dead_code)]
pub(crate) fn bounded_vector() -> Vec<u8> {
    let strings = indexed(&[b"app", b"entry"]);
    let empty = indexed(&[]);

    let mut class_type = vec![0, 0];
    push_u16(&mut class_type, 0);
    for value in [0, u32::MAX, 0, 0, 0, 0, 0] {
        push_u32(&mut class_type, value);
    }
    let mut function_type = vec![3, 0];
    push_u16(&mut function_type, 0);
    push_u32(&mut function_type, 1);
    push_u16(&mut function_type, 1);
    push_u16(&mut function_type, 0);
    function_type.extend(value_type(0, 0, u32::MAX));
    function_type.extend(value_type(7, 0, 0));
    let types = indexed(&[&class_type, &function_type]);

    let mut function = Vec::new();
    for value in [u32::MAX, 1, 1, 2] {
        push_u32(&mut function, value);
    }
    push_u16(&mut function, 1);
    push_u16(&mut function, 1);
    for value in [0, 2, 0, 1] {
        push_u32(&mut function, value);
    }
    function.extend(value_type(7, 0, 0));
    let functions = indexed(&[&function]);

    let mut protected = Vec::new();
    let mut handler = Vec::new();
    for (record, values) in [
        (&mut protected, [0, 0, 1, 2, 0, 0]),
        (&mut handler, [0, 1, 1, 1, 0, 0]),
    ] {
        for value in values {
            push_u32(record, value);
        }
    }
    let blocks = indexed(&[&protected, &handler]);
    let throw = [0xe4, 0, 6, 0, 0, 0];
    let return_unit = [0xe3, 0, 6, 0, 0xff, 0xff];
    let code = indexed(&[&throw, &return_unit]);

    let mut exception = Vec::new();
    for value in [0, 0, 1, 0, 1] {
        push_u32(&mut exception, value);
    }
    push_u16(&mut exception, 0);
    push_u16(&mut exception, 0);
    let exceptions = indexed(&[&exception]);

    let semantic_sections = vec![
        (0x0100_u16, strings, 2),
        (0x0101, types, 2),
        (0x0102, empty.clone(), 0),
        (0x0103, empty.clone(), 0),
        (0x0104, empty.clone(), 0),
        (0x0105, empty.clone(), 0),
        (0x0106, functions, 1),
        (0x0107, blocks, 2),
        (0x0108, code, 2),
        (0x0109, exceptions, 1),
        (0x010a, empty.clone(), 0),
    ];
    let module_hash = semantic_hash(&semantic_sections);

    let mut capability = Vec::new();
    push_u32(&mut capability, 0);
    push_u32(&mut capability, 1);
    push_u16(&mut capability, 1);
    push_u16(&mut capability, 0);
    push_u32(&mut capability, 1);
    push_u32(&mut capability, 0);
    push_u32(&mut capability, 0);
    let capabilities = indexed(&[&capability]);

    let mut manifest = manifest();
    manifest[24..28].copy_from_slice(&2_u32.to_le_bytes());
    manifest[28..32].copy_from_slice(&2_u32.to_le_bytes());
    manifest[32..36].copy_from_slice(&1_u32.to_le_bytes());
    let module = module_record_with_counts(module_hash, 2, 1, 0, 0);
    let modules = indexed(&[&module]);
    let mut sections = vec![
        (0x0001_u16, 0_u32, manifest, 1),
        (0x0002, 0, modules, 1),
        (0x0003, 0, capabilities, 1),
    ];
    sections.extend(
        semantic_sections
            .into_iter()
            .map(|(kind, payload, count)| (kind, 1, payload, count)),
    );

    let mut debug = Vec::new();
    for value in [0, 1, 0, 0, 1, u32::MAX, 8] {
        push_u32(&mut debug, value);
    }
    debug.extend_from_slice(b"src/a.kt");
    sections.push((0x0110, 1, indexed(&[&debug]), 1));

    assemble(sections, (1 << 0) | (1 << 2))
}

#[allow(dead_code)]
pub(crate) fn language_runtime_vector() -> Vec<u8> {
    let strings = indexed(&[b"Box", b"app", b"array", b"entry"]);
    let empty = indexed(&[]);

    let mut class_type = vec![0, 0];
    push_u16(&mut class_type, 0);
    for value in [0, u32::MAX, 0, 0, 0, 0, 0] {
        push_u32(&mut class_type, value);
    }
    let mut array_type = vec![2, 0];
    push_u16(&mut array_type, 0);
    push_u32(&mut array_type, 2);
    array_type.extend(value_type(1, 0, u32::MAX));
    let mut function_type = vec![3, 0];
    push_u16(&mut function_type, 0);
    push_u32(&mut function_type, 3);
    push_u16(&mut function_type, 0);
    push_u16(&mut function_type, 0);
    function_type.extend(value_type(0, 0, u32::MAX));
    let types = indexed(&[&class_type, &array_type, &function_type]);

    let zero = [0, 0, 0, 0, 0];
    let one = [0, 1, 0, 0, 0];
    let constants = indexed(&[&zero, &one]);

    let registers = [
        value_type(7, 0, 0),
        value_type(7, 1, 0),
        value_type(5, 0, u32::MAX),
        value_type(1, 0, u32::MAX),
        value_type(7, 0, 1),
        value_type(1, 0, u32::MAX),
        value_type(7, 0, 0),
        value_type(1, 0, u32::MAX),
    ];
    let mut function = Vec::new();
    for value in [u32::MAX, 3, 2, 2] {
        push_u32(&mut function, value);
    }
    push_u16(&mut function, registers.len() as u16);
    push_u16(&mut function, 0);
    for value in [0, 5, 0, 1] {
        push_u32(&mut function, value);
    }
    for register in registers {
        function.extend(register);
    }
    let functions = indexed(&[&function]);

    let block_values = [
        [0, 0, 6, 10, 0, 0],
        [0, 1, 4, 9, 0, 0],
        [0, 2, 2, 6, 0, 0],
        [0, 3, 1, 1, 1, 0],
        [0, 4, 1, 1, 0, 0],
    ];
    let block_records: Vec<Vec<u8>> = block_values
        .into_iter()
        .map(|values| {
            let mut record = Vec::new();
            for value in values {
                push_u32(&mut record, value);
            }
            record
        })
        .collect();
    let block_refs: Vec<&[u8]> = block_records.iter().map(Vec::as_slice).collect();
    let blocks = indexed(&block_refs);

    let block_zero = concat_frames(&[
        frame(0x30, 0, &[0, 0, 0]),
        frame(0x03, 0, &[1, 0]),
        frame(0x02, 0, &[3, 0, 1]),
        frame(0x02, 0, &[7, 0, 0]),
        frame(0x39, 0, &[2, 0, 1, 0, 0]),
        frame(0xe1, 0, &[2, 0, 1, 2]),
    ]);
    let block_one = concat_frames(&[
        frame(0x31, 0, &[4, 0, 1, 3, 0]),
        frame(0x34, 0, &[4, 0, 7, 0, 3, 0]),
        frame(0x33, 0, &[5, 0, 4, 0, 7, 0]),
        frame(0xe0, 0, &[3]),
    ]);
    let block_two = concat_frames(&[frame(0x30, 0, &[6, 0, 0]), frame(0xe4, 0, &[6, 0])]);
    let block_three = frame(0xe0, 0, &[3]);
    let block_four = frame(0xe3, 0, &[0xff, 0xff]);
    let code = indexed(&[
        &block_zero,
        &block_one,
        &block_two,
        &block_three,
        &block_four,
    ]);

    let mut exception = Vec::new();
    for value in [0, 2, 1, 0, 4] {
        push_u32(&mut exception, value);
    }
    push_u16(&mut exception, 6);
    push_u16(&mut exception, 0);
    let exceptions = indexed(&[&exception]);

    let semantic_sections = vec![
        (0x0100_u16, strings, 4),
        (0x0101, types, 3),
        (0x0102, constants, 2),
        (0x0103, empty.clone(), 0),
        (0x0104, empty.clone(), 0),
        (0x0105, empty.clone(), 0),
        (0x0106, functions, 1),
        (0x0107, blocks, 5),
        (0x0108, code, 5),
        (0x0109, exceptions, 1),
        (0x010a, empty.clone(), 0),
    ];
    single_module_artifact(semantic_sections, None, 1 << 0, 1, 3, 10)
}

#[allow(dead_code)]
pub(crate) fn debug_vector() -> Vec<u8> {
    let strings = indexed(&[b"app", b"entry"]);
    let empty = indexed(&[]);
    let mut function_type = vec![3, 0];
    push_u16(&mut function_type, 0);
    push_u32(&mut function_type, 1);
    push_u16(&mut function_type, 0);
    push_u16(&mut function_type, 0);
    function_type.extend(value_type(0, 0, u32::MAX));
    let types = indexed(&[&function_type]);

    let mut function = Vec::new();
    for value in [u32::MAX, 1, 0, 2] {
        push_u32(&mut function, value);
    }
    push_u16(&mut function, 0);
    push_u16(&mut function, 0);
    for value in [0, 1, 0, 0] {
        push_u32(&mut function, value);
    }
    let functions = indexed(&[&function]);
    let mut block = Vec::new();
    for value in [0, 0, 2, 2, 0, 0] {
        push_u32(&mut block, value);
    }
    let blocks = indexed(&[&block]);
    let code_record = concat_frames(&[frame(0x00, 0, &[]), frame(0xe3, 0, &[0xff, 0xff])]);
    let code = indexed(&[&code_record]);
    let semantic_sections = vec![
        (0x0100_u16, strings, 2),
        (0x0101, types, 1),
        (0x0102, empty.clone(), 0),
        (0x0103, empty.clone(), 0),
        (0x0104, empty.clone(), 0),
        (0x0105, empty.clone(), 0),
        (0x0106, functions, 1),
        (0x0107, blocks, 1),
        (0x0108, code, 1),
        (0x0109, empty.clone(), 0),
        (0x010a, empty, 0),
    ];

    let path = b"src/main.kts";
    let mut first = Vec::new();
    for value in [0, 0, 0, 0, 5, u32::MAX, path.len() as u32] {
        push_u32(&mut first, value);
    }
    first.extend_from_slice(path);
    let mut second = Vec::new();
    for value in [0, 0, 1, 6, 12, 0, path.len() as u32] {
        push_u32(&mut second, value);
    }
    second.extend_from_slice(path);
    let debug = indexed(&[&first, &second]);
    single_module_artifact(semantic_sections, Some((debug, 2)), 0, 0, 1, 2)
}

#[allow(dead_code)]
pub(crate) fn host_runtime_code() -> Vec<u8> {
    let spawn = concat_frames(&[frame(0x50, 0, &[0, 0, 0, 0]), frame(0xe3, 0, &[0xff, 0xff])]);
    let sleep = frame(0xe7, 0, &[0, 0, 0]);
    let join = frame(0xe8, 0, &[0xff, 0xff, 0, 0, 0]);
    let synchronous = concat_frames(&[
        frame(0x51, 0, &[0xff, 0xff, 0, 0, 0]),
        frame(0xe3, 0, &[0xff, 0xff]),
    ]);
    let asynchronous = frame(0xe9, 0, &[0xff, 0xff, 0, 1, 0, 0]);
    indexed(&[&spawn, &sleep, &join, &synchronous, &asynchronous])
}

fn single_module_artifact(
    semantic_sections: Vec<(u16, Vec<u8>, u32)>,
    debug: Option<(Vec<u8>, u32)>,
    semantic_features: u32,
    module_name: u32,
    type_count: u32,
    maximum_block_cost: u32,
) -> Vec<u8> {
    let module_hash = semantic_hash(&semantic_sections);
    let mut manifest = manifest();
    manifest[24..28].copy_from_slice(&maximum_block_cost.to_le_bytes());
    manifest[28..32].copy_from_slice(&maximum_block_cost.to_le_bytes());
    let module = module_record_full(module_name, 1, module_hash, 0, 0, type_count, 1);
    let modules = indexed(&[&module]);
    let empty = indexed(&[]);
    let mut sections = vec![
        (0x0001_u16, 0_u32, manifest, 1),
        (0x0002, 0, modules, 1),
        (0x0003, 0, empty, 0),
    ];
    sections.extend(
        semantic_sections
            .into_iter()
            .map(|(kind, payload, count)| (kind, 1, payload, count)),
    );
    if let Some((payload, count)) = debug {
        sections.push((0x0110, 1, payload, count));
    }
    assemble(sections, semantic_features)
}

fn frame(opcode: u8, form: u8, operands: &[u8]) -> Vec<u8> {
    let mut bytes = vec![opcode, form];
    push_u16(&mut bytes, (operands.len() + 4) as u16);
    bytes.extend_from_slice(operands);
    bytes
}

fn concat_frames(frames: &[Vec<u8>]) -> Vec<u8> {
    frames.iter().flatten().copied().collect()
}

#[allow(dead_code)]
fn value_type(kind: u8, flags: u8, nominal_type: u32) -> Vec<u8> {
    let mut bytes = vec![kind, flags];
    push_u16(&mut bytes, 0);
    push_u32(&mut bytes, nominal_type);
    bytes
}

pub(crate) fn minimal_vector_with_string_records(records: &[&[u8]]) -> Vec<u8> {
    vector_with_strings(records)
}

pub(crate) fn two_module_vector() -> Vec<u8> {
    let library_sections = minimal_module_sections(&[b"lib", b"work"], 1, &[], &[export_record(1)]);
    let library_hash = semantic_hash(&library_sections);
    let application_import = import_record(1, 1, 1, 0, library_hash);
    let application_sections =
        minimal_module_sections(&[b"app", b"entry"], 1, &[application_import], &[]);
    let application_hash = semantic_hash(&application_sections);

    let manifest = manifest();
    let application = module_record(0, 1, application_hash, 1, 0);
    let library = module_record(0, 2, library_hash, 0, 1);
    let modules = indexed(&[&application, &library]);
    let empty = indexed(&[]);
    let mut sections = vec![
        (0x0001_u16, 0_u32, manifest, 1),
        (0x0002, 0, modules, 2),
        (0x0003, 0, empty, 0),
    ];
    for (scope, module) in [(1, application_sections), (2, library_sections)] {
        for (kind, payload, count) in module {
            sections.push((kind, scope, payload, count));
        }
    }
    assemble(sections, 1 << 3)
}

fn minimal_module_sections(
    strings: &[&[u8]],
    function_name: u32,
    imports: &[Vec<u8>],
    exports: &[Vec<u8>],
) -> Vec<(u16, Vec<u8>, u32)> {
    let strings_payload = indexed(strings);
    let mut function_type = vec![3, 0];
    push_u16(&mut function_type, 0);
    push_u32(&mut function_type, function_name);
    push_u16(&mut function_type, 0);
    push_u16(&mut function_type, 0);
    function_type.extend_from_slice(&[0, 0, 0, 0]);
    push_u32(&mut function_type, u32::MAX);
    let types = indexed(&[&function_type]);
    let empty = indexed(&[]);

    let mut function = Vec::new();
    push_u32(&mut function, u32::MAX);
    push_u32(&mut function, function_name);
    push_u32(&mut function, 0);
    push_u32(&mut function, 2);
    push_u16(&mut function, 0);
    push_u16(&mut function, 0);
    push_u32(&mut function, 0);
    push_u32(&mut function, 1);
    push_u32(&mut function, 0);
    push_u32(&mut function, 0);
    let functions = indexed(&[&function]);

    let mut block = Vec::new();
    for value in [0, 0, 1, 1, 0, 0] {
        push_u32(&mut block, value);
    }
    let blocks = indexed(&[&block]);
    let return_unit = [0xe3, 0, 6, 0, 0xff, 0xff];
    let code = indexed(&[&return_unit]);
    let import_refs: Vec<_> = imports.iter().map(Vec::as_slice).collect();
    let export_refs: Vec<_> = exports.iter().map(Vec::as_slice).collect();
    vec![
        (0x0100, strings_payload, strings.len() as u32),
        (0x0101, types, 1),
        (0x0102, empty.clone(), 0),
        (0x0103, indexed(&import_refs), imports.len() as u32),
        (0x0104, indexed(&export_refs), exports.len() as u32),
        (0x0105, empty.clone(), 0),
        (0x0106, functions, 1),
        (0x0107, blocks, 1),
        (0x0108, code, 1),
        (0x0109, empty.clone(), 0),
        (0x010a, empty, 0),
    ]
}

fn semantic_hash(sections: &[(u16, Vec<u8>, u32)]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"Compukter module v1\0");
    for (kind, payload, _) in sections {
        hasher.update(kind.to_le_bytes());
        hasher.update((payload.len() as u64).to_le_bytes());
        hasher.update(payload);
    }
    hasher.finalize().into()
}

fn import_record(
    kind: u8,
    target_module: u32,
    target_name: u32,
    signature: u32,
    hash: [u8; 32],
) -> Vec<u8> {
    let mut record = vec![kind, 0, 0, 0];
    push_u32(&mut record, target_module);
    push_u32(&mut record, target_name);
    push_u32(&mut record, signature);
    record.extend(hash);
    record
}

fn export_record(name: u32) -> Vec<u8> {
    let mut record = vec![1, 1];
    push_u16(&mut record, 0);
    push_u32(&mut record, name);
    push_u32(&mut record, 0);
    push_u32(&mut record, 0);
    record
}

fn module_record(name: u32, flags: u32, hash: [u8; 32], imports: u32, exports: u32) -> Vec<u8> {
    let mut record = Vec::new();
    push_u32(&mut record, name);
    push_u32(&mut record, flags);
    record.extend(hash);
    for value in [imports, exports, 1, 1, 0] {
        push_u32(&mut record, value);
    }
    record
}

#[allow(dead_code)]
fn module_record_with_counts(
    hash: [u8; 32],
    types: u32,
    functions: u32,
    imports: u32,
    exports: u32,
) -> Vec<u8> {
    let mut record = Vec::new();
    push_u32(&mut record, 0);
    push_u32(&mut record, 1);
    record.extend(hash);
    for value in [imports, exports, types, functions, 0] {
        push_u32(&mut record, value);
    }
    record
}

fn module_record_full(
    name: u32,
    flags: u32,
    hash: [u8; 32],
    imports: u32,
    exports: u32,
    types: u32,
    functions: u32,
) -> Vec<u8> {
    let mut record = Vec::new();
    push_u32(&mut record, name);
    push_u32(&mut record, flags);
    record.extend(hash);
    for value in [imports, exports, types, functions, 0] {
        push_u32(&mut record, value);
    }
    record
}

fn manifest() -> Vec<u8> {
    let mut manifest = Vec::new();
    for value in [0, 0, 1, 1, 0, 0, 1, 1, 0, 0] {
        push_u32(&mut manifest, value);
    }
    manifest.resize(112, 0);
    manifest
}

fn assemble(sections: Vec<(u16, u32, Vec<u8>, u32)>, semantic_features: u32) -> Vec<u8> {
    let first_payload = align8(HEADER_SIZE + sections.len() * DIRECTORY_ENTRY_SIZE);
    let mut cursor = first_payload;
    let mut entries = Vec::new();
    for (kind, scope, payload, count) in &sections {
        entries.push((*kind, *scope, cursor, payload.len(), *count));
        cursor = align8(cursor + payload.len());
    }
    let payload_end = entries.last().unwrap().2 + entries.last().unwrap().3;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"CPKT");
    for value in [1_u16, 0, 1, 0, 64, 32] {
        push_u16(&mut bytes, value);
    }
    push_u32(&mut bytes, sections.len() as u32);
    push_u32(&mut bytes, semantic_features);
    push_u64(&mut bytes, 64);
    push_u64(&mut bytes, payload_end as u64);
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, 0);
    bytes.resize(HEADER_SIZE, 0);
    for (kind, scope, offset, length, count) in &entries {
        push_u16(&mut bytes, *kind);
        push_u16(&mut bytes, if *kind == 0x0110 { 0 } else { 3 });
        push_u32(&mut bytes, *scope);
        push_u64(&mut bytes, *offset as u64);
        push_u64(&mut bytes, *length as u64);
        push_u32(&mut bytes, *count);
        push_u32(&mut bytes, 0);
    }
    bytes.resize(first_payload, 0);
    for ((_, _, offset, _, _), (_, _, payload, _)) in entries.iter().zip(&sections) {
        bytes.resize(*offset, 0);
        bytes.extend_from_slice(payload);
    }
    bytes.resize(payload_end + DIGEST_SIZE, 0);
    rehash(&mut bytes);
    bytes
}

fn vector_with_strings(string_records: &[&[u8]]) -> Vec<u8> {
    vector_with_tables(string_records, &[], &[])
}

#[allow(dead_code)]
pub(crate) fn minimal_vector_with_utf16_literal_records(records: &[&[u8]]) -> Vec<u8> {
    vector_with_tables(&[b"app", b"entry"], records, &[])
}

#[allow(dead_code)]
pub(crate) fn minimal_vector_with_utf16_string_constant(
    records: &[&[u8]],
    literal_id: u32,
) -> Vec<u8> {
    let mut constant = vec![6];
    push_u32(&mut constant, literal_id);
    vector_with_tables(&[b"app", b"entry"], records, &[&constant])
}

fn vector_with_tables(
    string_records: &[&[u8]],
    utf16_literal_records: &[&[u8]],
    constant_records: &[&[u8]],
) -> Vec<u8> {
    let strings = indexed(string_records);
    let utf16_literals = indexed(utf16_literal_records);
    let constants = indexed(constant_records);
    let mut function_type = vec![3, 0];
    push_u16(&mut function_type, 0);
    push_u32(&mut function_type, 1);
    push_u16(&mut function_type, 0);
    push_u16(&mut function_type, 0);
    function_type.extend_from_slice(&[0, 0, 0, 0]);
    push_u32(&mut function_type, u32::MAX);
    let types = indexed(&[&function_type]);
    let empty = indexed(&[]);

    let mut function = Vec::new();
    push_u32(&mut function, u32::MAX);
    push_u32(&mut function, 1);
    push_u32(&mut function, 0);
    push_u32(&mut function, 2);
    push_u16(&mut function, 0);
    push_u16(&mut function, 0);
    push_u32(&mut function, 0);
    push_u32(&mut function, 1);
    push_u32(&mut function, 0);
    push_u32(&mut function, 0);
    let functions = indexed(&[&function]);

    let mut block = Vec::new();
    for value in [0, 0, 1, 1, 0, 0] {
        push_u32(&mut block, value);
    }
    let blocks = indexed(&[&block]);
    let return_unit = [0xe3, 0, 6, 0, 0xff, 0xff];
    let code = indexed(&[&return_unit]);

    let module_sections = [
        (0x0100_u16, strings.clone()),
        (0x0101, types.clone()),
        (0x0102, constants),
        (0x0103, empty.clone()),
        (0x0104, empty.clone()),
        (0x0105, empty.clone()),
        (0x0106, functions.clone()),
        (0x0107, blocks.clone()),
        (0x0108, code.clone()),
        (0x0109, empty.clone()),
        (0x010a, utf16_literals),
    ];

    let mut module_hasher = Sha256::new();
    module_hasher.update(b"Compukter module v1\0");
    for (kind, payload) in &module_sections {
        module_hasher.update(kind.to_le_bytes());
        module_hasher.update((payload.len() as u64).to_le_bytes());
        module_hasher.update(payload);
    }
    let module_hash = module_hasher.finalize();

    let mut manifest = Vec::new();
    for value in [0, 0, 1, 1, 0, 0, 1, 1, 0, 0] {
        push_u32(&mut manifest, value);
    }
    manifest.resize(112, 0);

    let mut module_record = Vec::new();
    push_u32(&mut module_record, 0);
    push_u32(&mut module_record, 1);
    module_record.extend_from_slice(&module_hash);
    for value in [0, 0, 1, 1, 0] {
        push_u32(&mut module_record, value);
    }
    let modules = indexed(&[&module_record]);

    let mut sections = vec![
        (0x0001_u16, 0_u32, manifest, 1),
        (0x0002, 0, modules, 1),
        (0x0003, 0, empty.clone(), 0),
    ];
    for (kind, payload) in module_sections {
        let count = match kind {
            0x0100 => 2,
            0x0102 => constant_records.len() as u32,
            0x0101 | 0x0106 | 0x0107 | 0x0108 => 1,
            0x010a => utf16_literal_records.len() as u32,
            _ => 0,
        };
        sections.push((kind, 1, payload, count));
    }

    let first_payload = align8(HEADER_SIZE + sections.len() * DIRECTORY_ENTRY_SIZE);
    let mut cursor = first_payload;
    let mut entries = Vec::new();
    for (kind, scope, payload, count) in &sections {
        entries.push((*kind, *scope, cursor, payload.len(), *count));
        cursor = align8(cursor + payload.len());
    }
    let payload_end = entries.last().unwrap().2 + entries.last().unwrap().3;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"CPKT");
    for value in [1_u16, 0, 1, 0, 64, 32] {
        push_u16(&mut bytes, value);
    }
    push_u32(&mut bytes, sections.len() as u32);
    push_u32(&mut bytes, 0);
    push_u64(&mut bytes, 64);
    push_u64(&mut bytes, payload_end as u64);
    push_u32(&mut bytes, 0);
    push_u32(&mut bytes, 0);
    bytes.resize(HEADER_SIZE, 0);

    for (kind, scope, offset, length, count) in &entries {
        push_u16(&mut bytes, *kind);
        push_u16(&mut bytes, 3);
        push_u32(&mut bytes, *scope);
        push_u64(&mut bytes, *offset as u64);
        push_u64(&mut bytes, *length as u64);
        push_u32(&mut bytes, *count);
        push_u32(&mut bytes, 0);
    }
    bytes.resize(first_payload, 0);
    for ((_, _, offset, _, _), (_, _, payload, _)) in entries.iter().zip(&sections) {
        bytes.resize(*offset, 0);
        bytes.extend_from_slice(payload);
    }
    bytes.resize(payload_end + DIGEST_SIZE, 0);
    rehash(&mut bytes);
    bytes
}

pub(crate) fn rehash(bytes: &mut [u8]) {
    let payload_end = bytes.len() - DIGEST_SIZE;
    let digest = Sha256::digest(&bytes[..payload_end]);
    bytes[payload_end..].copy_from_slice(&digest);
}

pub(crate) fn artifact_hash(bytes: &[u8]) -> [u8; 32] {
    let payload_end = bytes.len() - DIGEST_SIZE;
    Sha256::digest(&bytes[..payload_end]).into()
}

#[allow(dead_code)]
pub(crate) fn artifact_manifest(name: &str, bytes: &[u8]) -> String {
    let section_count = read_u32(bytes, 16) as usize;
    let semantic_features = read_u32(bytes, 20);
    let payload_end = read_u64(bytes, 32) as usize;
    assert_eq!(payload_end + DIGEST_SIZE, bytes.len());
    let digest = Sha256::digest(&bytes[..payload_end]);
    assert_eq!(digest.as_slice(), &bytes[payload_end..]);

    let mut manifest = format!(
        "# {name}\n\n- file length: {}\n- payload end: {payload_end}\n- semantic features: 0x{semantic_features:08x}\n- artifact sha256: `{}`\n\n| Kind | Scope | Offset | Length | Count | Record offsets |\n|---:|---:|---:|---:|---:|---|\n",
        bytes.len(),
        hex(&digest),
    );
    let mut module_hashes = Vec::new();
    for id in 0..section_count {
        let directory = HEADER_SIZE + id * DIRECTORY_ENTRY_SIZE;
        let kind = read_u16(bytes, directory);
        let scope = read_u32(bytes, directory + 4);
        let offset = read_u64(bytes, directory + 8) as usize;
        let length = read_u64(bytes, directory + 16) as usize;
        let count = read_u32(bytes, directory + 24) as usize;
        let records = if kind == 0x0001 {
            "fixed".to_owned()
        } else {
            indexed_record_ranges(bytes, offset, count)
                .into_iter()
                .map(|(start, length)| format!("`{start}:{length}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        manifest.push_str(&format!(
            "| `0x{kind:04x}` | {scope} | {offset} | {length} | {count} | {records} |\n"
        ));
        if kind == 0x0002 {
            for (start, _) in indexed_record_ranges(bytes, offset, count) {
                module_hashes.push(bytes[start + 8..start + 40].to_vec());
            }
        }
    }
    manifest.push_str("\n## Module semantic hashes\n\n");
    for (id, hash) in module_hashes.iter().enumerate() {
        manifest.push_str(&format!("- module {id}: `{}`\n", hex(hash)));
    }
    manifest
}

#[allow(dead_code)]
pub(crate) fn code_manifest(name: &str, bytes: &[u8]) -> String {
    let count = read_u32(bytes, 0) as usize;
    let records = align8(16 + 4 * (count + 1));
    let mut manifest = format!(
        "# {name}\n\n- envelope length: {}\n- record count: {count}\n\n| Record | Instruction | Offset | Opcode | Form | Length | Fixed cost |\n|---:|---:|---:|---:|---:|---:|---:|\n",
        bytes.len()
    );
    for record in 0..count {
        let relative_start = read_u32(bytes, 16 + record * 4) as usize;
        let relative_end = read_u32(bytes, 20 + record * 4) as usize;
        let start = records + relative_start;
        let end = records + relative_end;
        let mut cursor = start;
        let mut instruction = 0;
        while cursor < end {
            let opcode = bytes[cursor];
            let form = bytes[cursor + 1];
            let length = read_u16(bytes, cursor + 2) as usize;
            let cost = fixture_instruction_cost(opcode, &bytes[cursor + 4..cursor + length]);
            manifest.push_str(&format!(
                "| {record} | {instruction} | {cursor} | `0x{opcode:02x}` | {form} | {length} | {cost} |\n"
            ));
            cursor += length;
            instruction += 1;
        }
        assert_eq!(cursor, end);
    }
    manifest
}

fn fixture_instruction_cost(opcode: u8, operands: &[u8]) -> u32 {
    match opcode {
        0x50 => 6 + u32::from(operands[3]),
        0x51 => 5 + u32::from(operands[4]),
        0xe7 => 3,
        0xe8 => 4,
        0xe9 => 6 + u32::from(operands[4]),
        0xe3 => 1,
        _ => panic!("opcode {opcode:#04x} is not part of the host-runtime fixture"),
    }
}

fn indexed_record_ranges(bytes: &[u8], section: usize, count: usize) -> Vec<(usize, usize)> {
    assert_eq!(read_u32(bytes, section) as usize, count);
    let records = section + align8(16 + 4 * (count + 1));
    (0..count)
        .map(|id| {
            let start = read_u32(bytes, section + 16 + id * 4) as usize;
            let end = read_u32(bytes, section + 20 + id * 4) as usize;
            (records + start, end - start)
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

pub(crate) fn hex32(value: &str) -> [u8; 32] {
    assert_eq!(value.len(), 64);
    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let text = std::str::from_utf8(pair).unwrap();
        bytes[index] = u8::from_str_radix(text, 16).unwrap();
    }
    bytes
}

pub(crate) fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn directory_entry_offset(bytes: &[u8], kind: u16, scope: u32) -> usize {
    let section_count = read_u32(bytes, 16) as usize;
    (0..section_count)
        .map(|index| HEADER_SIZE + index * DIRECTORY_ENTRY_SIZE)
        .find(|offset| read_u16(bytes, *offset) == kind && read_u32(bytes, *offset + 4) == scope)
        .unwrap()
}

pub(crate) fn section_offset(bytes: &[u8], kind: u16, scope: u32) -> usize {
    let entry = directory_entry_offset(bytes, kind, scope);
    read_u64(bytes, entry + 8) as usize
}

pub(crate) fn indexed_record_offset(bytes: &[u8], kind: u16, scope: u32, id: usize) -> usize {
    let section = section_offset(bytes, kind, scope);
    let count = u32::from_le_bytes(bytes[section..section + 4].try_into().unwrap()) as usize;
    assert!(id < count);
    let prefix = align8(16 + 4 * (count + 1));
    let relative = u32::from_le_bytes(
        bytes[section + 16 + id * 4..section + 20 + id * 4]
            .try_into()
            .unwrap(),
    ) as usize;
    section + prefix + relative
}

pub(crate) fn minimal_vector_with_overlapping_sections() -> Vec<u8> {
    let mut bytes = minimal_vector();
    let first_offset = bytes[HEADER_SIZE + 8..HEADER_SIZE + 16].to_vec();
    let second_offset = HEADER_SIZE + DIRECTORY_ENTRY_SIZE + 8;
    bytes[second_offset..second_offset + 8].copy_from_slice(&first_offset);
    rehash(&mut bytes);
    bytes
}

pub(crate) fn minimal_vector_with_non_zero_gap() -> Vec<u8> {
    let mut bytes = minimal_vector();
    let entry = directory_entry_offset(&bytes, 0x0002, 0);
    let gap = section_offset(&bytes, 0x0002, 0) + read_u64(&bytes, entry + 16) as usize;
    bytes[gap] = 1;
    rehash(&mut bytes);
    bytes
}
