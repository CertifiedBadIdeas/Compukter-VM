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

pub(crate) fn minimal_vector_with_string_records(records: &[&[u8]]) -> Vec<u8> {
    vector_with_strings(records)
}

fn vector_with_strings(string_records: &[&[u8]]) -> Vec<u8> {
    let strings = indexed(string_records);
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
        (0x0102, empty.clone()),
        (0x0103, empty.clone()),
        (0x0104, empty.clone()),
        (0x0105, empty.clone()),
        (0x0106, functions.clone()),
        (0x0107, blocks.clone()),
        (0x0108, code.clone()),
        (0x0109, empty.clone()),
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
            0x0101 | 0x0106 | 0x0107 | 0x0108 => 1,
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

pub(crate) fn indexed_record_offset(bytes: &[u8], kind: u16, scope: u32, id: usize) -> usize {
    let section_count = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
    let entry = (0..section_count)
        .map(|index| HEADER_SIZE + index * DIRECTORY_ENTRY_SIZE)
        .find(|offset| {
            u16::from_le_bytes(bytes[*offset..*offset + 2].try_into().unwrap()) == kind
                && u32::from_le_bytes(bytes[*offset + 4..*offset + 8].try_into().unwrap()) == scope
        })
        .unwrap();
    let section = u64::from_le_bytes(bytes[entry + 8..entry + 16].try_into().unwrap()) as usize;
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
    bytes[676] = 1;
    rehash(&mut bytes);
    bytes
}
