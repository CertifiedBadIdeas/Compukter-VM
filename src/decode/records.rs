use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::{
    artifact::{
        format, Block, BlockId, ByteRange, Capability, Constant, DebugEntry, DecodedArtifact,
        DecodedModule, ExceptionEntry, Export, Field, Function, FunctionId, Import, Manifest,
        ModuleId, NominalType, TypeId, ValueType,
    },
    bytes::Cursor,
    decode::{container::decode_container, indexed::IndexedSection},
    diagnostic::{Code, Diagnostic, DiagnosticSet, Family},
    limits::ArtifactLimits,
};

pub(crate) fn decode_artifact(
    bytes: Arc<[u8]>,
    limits: &ArtifactLimits,
) -> Result<DecodedArtifact, DiagnosticSet> {
    let container = decode_container(&bytes, limits)?;
    let manifest_entry = find(&container, 0, format::MANIFEST, limits)?;
    let modules_entry = find(&container, 0, format::MODULES, limits)?;
    let capabilities_entry = find(&container, 0, format::CAPABILITIES, limits)?;
    let manifest = decode_manifest(&container, manifest_entry, limits)?;
    let module_section = IndexedSection::decode(&container, modules_entry, limits)
        .map_err(|error| single(limits, error))?;
    let capabilities = IndexedSection::decode(&container, capabilities_entry, limits)
        .map_err(|error| single(limits, error))?;
    let declared_capabilities = manifest
        .required_capabilities
        .checked_add(manifest.optional_capabilities)
        .ok_or_else(|| {
            single(
                limits,
                at(
                    Code::BadRecord,
                    format::MANIFEST,
                    manifest_entry.offset as usize + 32,
                    "capability counts overflow",
                ),
            )
        })?;
    if capabilities.len() != declared_capabilities as usize {
        return Err(single(
            limits,
            at(
                Code::BadRecord,
                format::CAPABILITIES,
                capabilities_entry.offset as usize,
                "capability count disagrees with manifest",
            ),
        ));
    }
    if capabilities.len() > limits.capabilities {
        return Err(single(
            limits,
            at(
                Code::LimitExceeded,
                format::CAPABILITIES,
                capabilities_entry.offset as usize,
                "capability limit exceeded",
            ),
        ));
    }
    let capabilities = parse_capabilities(&capabilities, limits)?;
    if module_section.len() > limits.modules {
        return Err(single(
            limits,
            at(
                Code::LimitExceeded,
                format::MODULES,
                modules_entry.offset as usize,
                "module limit exceeded",
            ),
        ));
    }

    if container
        .directory
        .iter()
        .any(|entry| format::is_module(entry.kind) && entry.scope as usize > module_section.len())
    {
        return Err(single(
            limits,
            at(
                Code::BadSection,
                format::MODULES,
                modules_entry.offset as usize,
                "module section scope is not dense",
            ),
        ));
    }

    let mut modules = Vec::new();
    modules
        .try_reserve_exact(module_section.len())
        .map_err(|_| {
            single(
                limits,
                at(
                    Code::LimitExceeded,
                    format::MODULES,
                    modules_entry.offset as usize,
                    "cannot reserve modules",
                ),
            )
        })?;
    let mut total_string_bytes = 0;
    let mut total_code_bytes = 0;
    let mut total_debug_bytes = 0;
    let mut total_imports = 0;
    let mut total_functions = 0;
    let mut total_blocks = 0;
    let mut total_exceptions = 0;
    for module_id in 0..module_section.len() {
        let record = module_section
            .record(module_id as u32)
            .map_err(|error| single(limits, error))?;
        let mut cursor = Cursor::new(record);
        let name_string = read(&mut cursor, format::MODULES, modules_entry.offset as usize)?;
        let flags = read(&mut cursor, format::MODULES, modules_entry.offset as usize)?;
        let semantic_hash: [u8; 32] = cursor
            .take(32)
            .map_err(|error| single(limits, relocate(error, modules_entry.offset as usize)))?
            .try_into()
            .expect("fixed semantic hash");
        let declared_imports = read(&mut cursor, format::MODULES, modules_entry.offset as usize)?;
        let declared_exports = read(&mut cursor, format::MODULES, modules_entry.offset as usize)?;
        let declared_types = read(&mut cursor, format::MODULES, modules_entry.offset as usize)?;
        let declared_functions = read(&mut cursor, format::MODULES, modules_entry.offset as usize)?;
        let reserved: u32 = read(&mut cursor, format::MODULES, modules_entry.offset as usize)?;
        if cursor.position() != record.len() || reserved != 0 || flags & !0b11 != 0 {
            return Err(single(
                limits,
                at(
                    Code::BadRecord,
                    format::MODULES,
                    modules_entry.offset as usize,
                    "invalid module record",
                ),
            ));
        }
        let strings_entry = find(&container, module_id as u32 + 1, format::STRINGS, limits)?;
        super::indexed::decode_string_table(&container, strings_entry, limits)
            .map_err(|error| single(limits, error))?;
        let string_section = IndexedSection::decode(&container, strings_entry, limits)
            .map_err(|error| single(limits, error))?;
        add_to_limit(
            &mut total_string_bytes,
            string_section.record_bytes_len(),
            limits.strings_bytes,
            limits,
            "total string byte limit exceeded",
        )?;
        let strings = collect_ranges(&string_section, limits)?;
        let scope = module_id as u32 + 1;
        let types_section = indexed(&container, scope, format::TYPES, limits)?;
        let imports_section = indexed(&container, scope, format::IMPORTS, limits)?;
        let exports_section = indexed(&container, scope, format::EXPORTS, limits)?;
        let constants_section = indexed(&container, scope, format::CONSTANTS, limits)?;
        let fields_section = indexed(&container, scope, format::FIELDS, limits)?;
        let functions_section = indexed(&container, scope, format::FUNCTIONS, limits)?;
        let blocks_section = indexed(&container, scope, format::BLOCKS, limits)?;
        let code_section = indexed(&container, scope, format::CODE, limits)?;
        let exceptions_section = indexed(&container, scope, format::EXCEPTIONS, limits)?;
        add_to_limit(
            &mut total_imports,
            imports_section.len(),
            limits.imports,
            limits,
            "total import limit exceeded",
        )?;
        add_to_limit(
            &mut total_functions,
            functions_section.len(),
            limits.functions,
            limits,
            "total function limit exceeded",
        )?;
        add_to_limit(
            &mut total_blocks,
            blocks_section.len(),
            limits.blocks,
            limits,
            "total block limit exceeded",
        )?;
        add_to_limit(
            &mut total_exceptions,
            exceptions_section.len(),
            limits.exceptions,
            limits,
            "total exception limit exceeded",
        )?;
        add_to_limit(
            &mut total_code_bytes,
            code_section.record_bytes_len(),
            limits.code_bytes,
            limits,
            "total code byte limit exceeded",
        )?;
        if types_section.len() != declared_types as usize
            || imports_section.len() != declared_imports as usize
            || exports_section.len() != declared_exports as usize
            || functions_section.len() != declared_functions as usize
        {
            return Err(single(
                limits,
                at(
                    Code::BadRecord,
                    format::MODULES,
                    modules_entry.offset as usize,
                    "module table counts disagree with module record",
                ),
            ));
        }
        let types = parse_types(&types_section, limits)?;
        let constants = parse_constants(&constants_section, limits)?;
        let imports = parse_imports(&imports_section, limits)?;
        let exports = parse_exports(&exports_section, limits)?;
        let fields = parse_fields(&fields_section, limits)?;
        let functions = parse_functions(&functions_section, limits)?;
        let blocks = parse_blocks(&blocks_section, limits)?;
        let code = collect_ranges(&code_section, limits)?;
        let exceptions = parse_exceptions(&exceptions_section, limits)?;
        let debug = match optional(&container, scope, format::DEBUG) {
            Some(entry) => {
                let section = IndexedSection::decode(&container, entry, limits)
                    .map_err(|error| single(limits, error))?;
                add_to_limit(
                    &mut total_debug_bytes,
                    section.record_bytes_len(),
                    limits.debug_bytes,
                    limits,
                    "total debug byte limit exceeded",
                )?;
                parse_debug(&section, limits)?
            }
            None => Vec::new(),
        };
        modules.push(DecodedModule {
            name_string,
            flags,
            semantic_hash,
            declared_imports,
            declared_exports,
            declared_types,
            declared_functions,
            strings,
            types,
            constants,
            imports,
            exports,
            fields,
            functions,
            blocks,
            code,
            exceptions,
            debug,
        });
    }

    validate_local_tables(
        &bytes,
        &container.header,
        &manifest,
        &capabilities,
        &modules,
        limits,
    )?;

    let content_hash = Sha256::digest(&bytes[..bytes.len() - format::DIGEST_SIZE]).into();
    let header = container.header;
    drop(container);
    Ok(DecodedArtifact {
        bytes,
        content_hash,
        header,
        manifest,
        capabilities,
        modules,
    })
}

fn indexed<'a>(
    container: &super::container::Container<'a>,
    scope: u32,
    kind: u16,
    limits: &ArtifactLimits,
) -> Result<IndexedSection<'a>, DiagnosticSet> {
    let entry = find(container, scope, kind, limits)?;
    IndexedSection::decode(container, entry, limits).map_err(|error| single(limits, error))
}

fn optional<'a>(
    container: &'a super::container::Container<'_>,
    scope: u32,
    kind: u16,
) -> Option<&'a super::container::DirectoryEntry> {
    container
        .directory
        .iter()
        .find(|entry| entry.scope == scope && entry.kind == kind)
}

fn parse_capabilities(
    section: &IndexedSection<'_>,
    limits: &ArtifactLimits,
) -> Result<Vec<Capability>, DiagnosticSet> {
    parse_each(section, limits, |cursor, record| {
        let value = Capability {
            namespace: ru32(cursor)?,
            name: ru32(cursor)?,
            abi_major: ru16(cursor)?,
            minimum_abi_minor: ru16(cursor)?,
            flags: ru32(cursor)?,
            operation_count: ru32(cursor)?,
        };
        let reserved = ru32(cursor)?;
        finish(
            cursor,
            record,
            reserved == 0 && matches!(value.flags, 1 | 2),
        )?;
        Ok(value)
    })
}

fn parse_types(
    section: &IndexedSection<'_>,
    limits: &ArtifactLimits,
) -> Result<Vec<NominalType>, DiagnosticSet> {
    parse_each(section, limits, |cursor, record| {
        let tag = ru8(cursor)?;
        let flags = ru8(cursor)?;
        let generic_arity = ru16(cursor)?;
        let name = ru32(cursor)?;
        let value = match tag {
            0 | 1 => {
                let super_type = ru32(cursor)?;
                let interface_count = ru32(cursor)? as usize;
                let field_start = ru32(cursor)?;
                let field_count = ru32(cursor)?;
                let method_start = ru32(cursor)?;
                let method_count = ru32(cursor)?;
                let mut interfaces = Vec::new();
                interfaces
                    .try_reserve_exact(interface_count)
                    .map_err(|_| raw(Code::LimitExceeded, "cannot reserve interfaces"))?;
                for _ in 0..interface_count {
                    interfaces.push(TypeId(ru32(cursor)?));
                }
                if interfaces.windows(2).any(|pair| pair[0] >= pair[1]) {
                    return Err(raw(
                        Code::BadType,
                        "interface refs are not sorted and unique",
                    ));
                }
                if tag == 0 {
                    if flags & !0b11 != 0 {
                        return Err(raw(Code::BadType, "invalid class flags"));
                    }
                    NominalType::Class {
                        flags,
                        generic_arity,
                        name,
                        super_type: TypeId(super_type),
                        interfaces,
                        field_start,
                        field_count,
                        method_start,
                        method_count,
                    }
                } else {
                    if flags & !1 != 0 || field_count != 0 {
                        return Err(raw(Code::BadType, "invalid interface flags or field range"));
                    }
                    NominalType::Interface {
                        flags,
                        generic_arity,
                        name,
                        super_type: TypeId(super_type),
                        interfaces,
                        method_start,
                        method_count,
                    }
                }
            }
            2 => {
                if flags != 0 || generic_arity != 0 {
                    return Err(raw(Code::BadType, "invalid array header"));
                }
                NominalType::Array {
                    name,
                    element: value_type(cursor)?,
                }
            }
            3 => {
                if flags != 0 {
                    return Err(raw(
                        Code::BadType,
                        "function type header flags are non-zero",
                    ));
                }
                let parameter_count = ru16(cursor)? as usize;
                let function_flags = ru16(cursor)?;
                if function_flags & !1 != 0 {
                    return Err(raw(Code::BadType, "invalid function type flags"));
                }
                let result = value_type(cursor)?;
                let mut parameters = Vec::new();
                parameters
                    .try_reserve_exact(parameter_count)
                    .map_err(|_| raw(Code::LimitExceeded, "cannot reserve parameters"))?;
                for _ in 0..parameter_count {
                    parameters.push(value_type(cursor)?);
                }
                NominalType::Function {
                    name,
                    flags: function_flags,
                    result,
                    parameters,
                }
            }
            _ => return Err(raw(Code::BadType, "unknown nominal type tag")),
        };
        finish(cursor, record, true)?;
        Ok(value)
    })
}

fn parse_constants(
    section: &IndexedSection<'_>,
    limits: &ArtifactLimits,
) -> Result<Vec<Constant>, DiagnosticSet> {
    ensure_raw_order(section, limits, "constants are not sorted and unique")?;
    parse_each(section, limits, |cursor, record| {
        let value = match ru8(cursor)? {
            0 => Constant::I32(cursor.read_i32().map_err(single_raw)?),
            1 => Constant::I64(cursor.read_u64().map_err(single_raw)? as i64),
            2 => Constant::F32(ru32(cursor)?),
            3 => Constant::F64(cursor.read_u64().map_err(single_raw)?),
            4 => match ru8(cursor)? {
                0 => Constant::Bool(false),
                1 => Constant::Bool(true),
                _ => return Err(raw(Code::BadRecord, "invalid boolean constant")),
            },
            5 => Constant::Char(
                char::from_u32(ru32(cursor)?)
                    .ok_or_else(|| raw(Code::BadRecord, "invalid Unicode scalar"))?,
            ),
            6 => Constant::String(ru32(cursor)?),
            7 => Constant::Null,
            _ => return Err(raw(Code::BadRecord, "unknown constant tag")),
        };
        finish(cursor, record, true)?;
        Ok(value)
    })
}

fn parse_imports(
    section: &IndexedSection<'_>,
    limits: &ArtifactLimits,
) -> Result<Vec<Import>, DiagnosticSet> {
    parse_each(section, limits, |cursor, record| {
        let kind = ru8(cursor)?;
        let reserved = cursor.take(3).map_err(single_raw)?;
        let target_module = ModuleId(ru32(cursor)?);
        let target_name = ru32(cursor)?;
        let expected_signature = TypeId(ru32(cursor)?);
        let target_hash = cursor.take(32).map_err(single_raw)?.try_into().unwrap();
        finish(
            cursor,
            record,
            kind <= 2 && reserved.iter().all(|byte| *byte == 0),
        )?;
        Ok(Import {
            kind,
            target_module,
            target_name,
            expected_signature,
            target_hash,
        })
    })
}

fn parse_exports(
    section: &IndexedSection<'_>,
    limits: &ArtifactLimits,
) -> Result<Vec<Export>, DiagnosticSet> {
    parse_each(section, limits, |cursor, record| {
        let value = Export {
            kind: ru8(cursor)?,
            visibility: ru8(cursor)?,
            name: {
                let reserved = ru16(cursor)?;
                if reserved != 0 {
                    return Err(raw(Code::BadRecord, "export reserved field is non-zero"));
                }
                ru32(cursor)?
            },
            local_symbol: ru32(cursor)?,
            signature: TypeId(ru32(cursor)?),
        };
        finish(cursor, record, value.kind <= 2 && value.visibility <= 1)?;
        Ok(value)
    })
}

fn parse_fields(
    section: &IndexedSection<'_>,
    limits: &ArtifactLimits,
) -> Result<Vec<Field>, DiagnosticSet> {
    parse_each(section, limits, |cursor, record| {
        let value = Field {
            owner: TypeId(ru32(cursor)?),
            name: ru32(cursor)?,
            value_type: value_type(cursor)?,
            flags: ru32(cursor)?,
        };
        let reserved = ru32(cursor)?;
        finish(cursor, record, reserved == 0 && value.flags & !0b11 == 0)?;
        Ok(value)
    })
}

fn parse_functions(
    section: &IndexedSection<'_>,
    limits: &ArtifactLimits,
) -> Result<Vec<Function>, DiagnosticSet> {
    parse_each(section, limits, |cursor, record| {
        let owner = TypeId(ru32(cursor)?);
        let name = ru32(cursor)?;
        let signature = TypeId(ru32(cursor)?);
        let flags = ru32(cursor)?;
        let register_count = ru16(cursor)?;
        let parameter_count = ru16(cursor)?;
        let first_block = ru32(cursor)?;
        let block_count = ru32(cursor)?;
        let first_exception = ru32(cursor)?;
        let exception_count = ru32(cursor)?;
        if register_count as usize > limits.registers_per_function {
            return Err(raw(Code::LimitExceeded, "register limit exceeded"));
        }
        let mut registers = Vec::new();
        registers
            .try_reserve_exact(register_count as usize)
            .map_err(|_| raw(Code::LimitExceeded, "cannot reserve registers"))?;
        for _ in 0..register_count {
            registers.push(value_type(cursor)?);
        }
        finish(cursor, record, flags & !0b1111 == 0)?;
        Ok(Function {
            owner,
            name,
            signature,
            flags,
            register_count,
            parameter_count,
            first_block: BlockId(first_block),
            block_count,
            first_exception,
            exception_count,
            registers,
        })
    })
}

fn parse_blocks(
    section: &IndexedSection<'_>,
    limits: &ArtifactLimits,
) -> Result<Vec<Block>, DiagnosticSet> {
    parse_each(section, limits, |cursor, record| {
        let value = Block {
            owner_function: FunctionId(ru32(cursor)?),
            code_record: BlockId(ru32(cursor)?),
            instruction_count: ru32(cursor)?,
            declared_fixed_cost: ru32(cursor)?,
            flags: ru32(cursor)?,
        };
        let reserved = ru32(cursor)?;
        finish(cursor, record, reserved == 0 && value.flags & !1 == 0)?;
        Ok(value)
    })
}

fn parse_exceptions(
    section: &IndexedSection<'_>,
    limits: &ArtifactLimits,
) -> Result<Vec<ExceptionEntry>, DiagnosticSet> {
    parse_each(section, limits, |cursor, record| {
        let value = ExceptionEntry {
            owner_function: FunctionId(ru32(cursor)?),
            first_protected_block: BlockId(ru32(cursor)?),
            protected_block_count: ru32(cursor)?,
            catch_type: TypeId(ru32(cursor)?),
            handler_block: BlockId(ru32(cursor)?),
            exception_register: ru16(cursor)?,
        };
        let reserved = ru16(cursor)?;
        finish(cursor, record, reserved == 0)?;
        Ok(value)
    })
}

fn parse_debug(
    section: &IndexedSection<'_>,
    limits: &ArtifactLimits,
) -> Result<Vec<DebugEntry>, DiagnosticSet> {
    if section.record_bytes_len() > limits.debug_bytes {
        return Err(raw(Code::LimitExceeded, "debug byte limit exceeded"));
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(section.len())
        .map_err(|_| raw(Code::LimitExceeded, "cannot reserve debug records"))?;
    for id in 0..section.len() {
        let record = section.record(id as u32).map_err(single_raw)?;
        let record_range = section.record_range(id as u32).map_err(single_raw)?;
        let mut cursor = Cursor::new(record);
        let function = FunctionId(ru32(&mut cursor)?);
        let block = BlockId(ru32(&mut cursor)?);
        let instruction = ru32(&mut cursor)?;
        let start_utf16 = ru32(&mut cursor)?;
        let end_utf16 = ru32(&mut cursor)?;
        let inline_parent = ru32(&mut cursor)?;
        let path_length = ru32(&mut cursor)? as usize;
        let path_start = cursor.position();
        let source_path = cursor.read_utf8(path_length).map_err(single_raw)?;
        if !canonical_source_path(source_path) {
            return Err(raw(Code::BadRecord, "debug source path is not canonical"));
        }
        finish(
            &cursor,
            record,
            inline_parent == u32::MAX || inline_parent < id as u32,
        )?;
        values.push(DebugEntry {
            function,
            block,
            instruction,
            start_utf16,
            end_utf16,
            inline_parent,
            source_path: ByteRange {
                start: record_range.start + path_start,
                end: record_range.start + path_start + path_length,
            },
        });
    }
    if values.windows(2).any(|pair| {
        (pair[0].function, pair[0].block, pair[0].instruction)
            >= (pair[1].function, pair[1].block, pair[1].instruction)
    }) {
        return Err(raw(
            Code::BadRecord,
            "debug records are not strictly ordered",
        ));
    }
    Ok(values)
}

fn collect_ranges(
    section: &IndexedSection<'_>,
    _limits: &ArtifactLimits,
) -> Result<Vec<ByteRange>, DiagnosticSet> {
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(section.len())
        .map_err(|_| raw(Code::LimitExceeded, "cannot reserve record ranges"))?;
    for id in 0..section.len() {
        let range = section.record_range(id as u32).map_err(single_raw)?;
        ranges.push(ByteRange {
            start: range.start,
            end: range.end,
        });
    }
    Ok(ranges)
}

fn parse_each<T, F>(
    section: &IndexedSection<'_>,
    _limits: &ArtifactLimits,
    mut parse: F,
) -> Result<Vec<T>, DiagnosticSet>
where
    F: FnMut(&mut Cursor<'_>, &[u8]) -> Result<T, DiagnosticSet>,
{
    let mut values = Vec::new();
    values
        .try_reserve_exact(section.len())
        .map_err(|_| raw(Code::LimitExceeded, "cannot reserve record table"))?;
    for id in 0..section.len() {
        let record = section.record(id as u32).map_err(single_raw)?;
        let mut cursor = Cursor::new(record);
        values.push(parse(&mut cursor, record)?);
    }
    Ok(values)
}

fn value_type(cursor: &mut Cursor<'_>) -> Result<ValueType, DiagnosticSet> {
    let kind = ru8(cursor)?;
    let flags = ru8(cursor)?;
    let reserved = ru16(cursor)?;
    let nominal_type = ru32(cursor)?;
    if reserved != 0
        || kind > 7
        || (kind != 7 && (flags != 0 || nominal_type != u32::MAX))
        || (kind == 7 && (flags & !1 != 0 || nominal_type == u32::MAX))
    {
        return Err(raw(Code::BadType, "invalid value type"));
    }
    Ok(ValueType {
        kind,
        flags,
        nominal_type: TypeId(nominal_type),
    })
}

fn add_to_limit(
    total: &mut usize,
    amount: usize,
    maximum: usize,
    limits: &ArtifactLimits,
    detail: &'static str,
) -> Result<(), DiagnosticSet> {
    let next = total.checked_add(amount);
    if next.is_none_or(|value| value > maximum) {
        Err(single(
            limits,
            Diagnostic::at_offset(Family::Limit, Code::LimitExceeded, 0, detail),
        ))
    } else {
        *total = next.expect("checked bounded total");
        Ok(())
    }
}

fn ensure_raw_order(
    section: &IndexedSection<'_>,
    limits: &ArtifactLimits,
    detail: &'static str,
) -> Result<(), DiagnosticSet> {
    let mut previous: Option<&[u8]> = None;
    for id in 0..section.len() {
        let record = section
            .record(id as u32)
            .map_err(|error| single(limits, error))?;
        if previous.is_some_and(|value| value >= record) {
            return Err(single(
                limits,
                Diagnostic::at_offset(Family::Section, Code::BadRecord, 0, detail),
            ));
        }
        previous = Some(record);
    }
    Ok(())
}

fn canonical_source_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn validate_local_tables(
    bytes: &[u8],
    header: &super::container::Header,
    manifest: &Manifest,
    capabilities: &[Capability],
    modules: &[DecodedModule],
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let entry_module = modules.get(header.entry_module as usize).ok_or_else(|| {
        single(
            limits,
            at(
                Code::BadRecord,
                format::MODULES,
                0,
                "entry module id is out of range",
            ),
        )
    })?;
    if header.entry_function as usize >= entry_module.functions.len() {
        return Err(single(
            limits,
            at(
                Code::BadRecord,
                format::FUNCTIONS,
                0,
                "entry function id is out of range",
            ),
        ));
    }

    let application_modules = modules
        .iter()
        .enumerate()
        .filter(|(_, module)| module.flags == 1)
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    if application_modules != [header.entry_module as usize]
        || modules.iter().any(|module| !matches!(module.flags, 1 | 2))
    {
        return Err(single(
            limits,
            at(
                Code::BadRecord,
                format::MODULES,
                0,
                "module application/library flags are invalid",
            ),
        ));
    }

    let required = capabilities.iter().filter(|value| value.flags == 1).count();
    let optional = capabilities.iter().filter(|value| value.flags == 2).count();
    if required != manifest.required_capabilities as usize
        || optional != manifest.optional_capabilities as usize
    {
        return Err(single(
            limits,
            at(
                Code::BadRecord,
                format::CAPABILITIES,
                0,
                "capability flag counts disagree with manifest",
            ),
        ));
    }
    for capability in capabilities {
        string(bytes, entry_module, capability.namespace, limits)?;
        string(bytes, entry_module, capability.name, limits)?;
    }
    if capabilities.windows(2).any(|pair| {
        capability_key(bytes, entry_module, &pair[0])
            >= capability_key(bytes, entry_module, &pair[1])
    }) {
        return Err(single(
            limits,
            at(
                Code::BadRecord,
                format::CAPABILITIES,
                0,
                "capabilities are not strictly ordered",
            ),
        ));
    }

    for (module_id, module) in modules.iter().enumerate() {
        string(bytes, module, module.name_string, limits)?;
        validate_module_tables(bytes, module_id, module, modules, limits)?;
    }
    Ok(())
}

fn validate_module_tables(
    bytes: &[u8],
    module_id: usize,
    module: &DecodedModule,
    modules: &[DecodedModule],
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    for nominal in &module.types {
        let name = match nominal {
            NominalType::Class {
                name,
                super_type,
                interfaces,
                ..
            }
            | NominalType::Interface {
                name,
                super_type,
                interfaces,
                ..
            } => {
                type_ref(module, *super_type, true, limits)?;
                for interface in interfaces {
                    type_ref(module, *interface, false, limits)?;
                }
                *name
            }
            NominalType::Array { name, element } => {
                value_type_ref(module, *element, limits)?;
                *name
            }
            NominalType::Function {
                name,
                result,
                parameters,
                ..
            } => {
                value_type_ref(module, *result, limits)?;
                for parameter in parameters {
                    value_type_ref(module, *parameter, limits)?;
                }
                *name
            }
        };
        string(bytes, module, name, limits)?;
    }
    for constant in &module.constants {
        if let Constant::String(id) = constant {
            string(bytes, module, *id, limits)?;
        }
    }
    for import in &module.imports {
        let target = modules
            .get(import.target_module.0 as usize)
            .ok_or_else(|| {
                record_error(
                    limits,
                    format::IMPORTS,
                    "import target module is out of range",
                )
            })?;
        string(bytes, target, import.target_name, limits)?;
        type_ref(module, import.expected_signature, false, limits)?;
    }
    if module
        .imports
        .windows(2)
        .any(|pair| import_key(bytes, modules, &pair[0]) >= import_key(bytes, modules, &pair[1]))
    {
        return Err(record_error(
            limits,
            format::IMPORTS,
            "imports are not strictly ordered",
        ));
    }
    for export in &module.exports {
        string(bytes, module, export.name, limits)?;
        type_ref(module, export.signature, false, limits)?;
        let symbol_count = match export.kind {
            0 => module.types.len(),
            1 => module.functions.len(),
            2 => module.fields.len(),
            _ => 0,
        };
        if export.local_symbol as usize >= symbol_count {
            return Err(record_error(
                limits,
                format::EXPORTS,
                "export local symbol is out of range",
            ));
        }
    }
    if module
        .exports
        .windows(2)
        .any(|pair| export_key(bytes, module, &pair[0]) >= export_key(bytes, module, &pair[1]))
    {
        return Err(record_error(
            limits,
            format::EXPORTS,
            "exports are not strictly ordered",
        ));
    }
    for field in &module.fields {
        type_ref(module, field.owner, false, limits)?;
        string(bytes, module, field.name, limits)?;
        value_type_ref(module, field.value_type, limits)?;
    }
    for function in &module.functions {
        type_ref(module, function.owner, true, limits)?;
        string(bytes, module, function.name, limits)?;
        type_ref(module, function.signature, false, limits)?;
        if function.parameter_count > function.register_count {
            return Err(record_error(
                limits,
                format::FUNCTIONS,
                "parameter count exceeds register count",
            ));
        }
        checked_range(
            function.first_block.0,
            function.block_count,
            module.blocks.len(),
            limits,
            format::FUNCTIONS,
            "function block range is out of bounds",
        )?;
        checked_range(
            function.first_exception,
            function.exception_count,
            module.exceptions.len(),
            limits,
            format::FUNCTIONS,
            "function exception range is out of bounds",
        )?;
        for register in &function.registers {
            value_type_ref(module, *register, limits)?;
        }
    }
    for (block_id, block) in module.blocks.iter().enumerate() {
        if block.owner_function.0 as usize >= module.functions.len()
            || block.code_record.0 as usize != block_id
            || block.code_record.0 as usize >= module.code.len()
        {
            return Err(record_error(
                limits,
                format::BLOCKS,
                "block owner or code record is out of range",
            ));
        }
    }
    if module.blocks.len() != module.code.len() {
        return Err(record_error(
            limits,
            format::CODE,
            "block and code record counts disagree",
        ));
    }
    for exception in &module.exceptions {
        if exception.owner_function.0 as usize >= module.functions.len()
            || exception.first_protected_block.0 as usize >= module.blocks.len()
            || exception.handler_block.0 as usize >= module.blocks.len()
        {
            return Err(record_error(
                limits,
                format::EXCEPTIONS,
                "exception reference is out of range",
            ));
        }
        type_ref(module, exception.catch_type, true, limits)?;
    }
    for debug in &module.debug {
        if debug.function.0 as usize >= module.functions.len()
            || debug.block.0 as usize >= module.blocks.len()
        {
            return Err(record_error(
                limits,
                format::DEBUG,
                "debug reference is out of range",
            ));
        }
    }
    let _ = module_id;
    Ok(())
}

fn value_type_ref(
    module: &DecodedModule,
    value: ValueType,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    if value.kind == 7 {
        type_ref(module, value.nominal_type, false, limits)?;
    }
    Ok(())
}

fn type_ref(
    module: &DecodedModule,
    value: TypeId,
    allow_absent: bool,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    if value.0 == u32::MAX {
        return if allow_absent {
            Ok(())
        } else {
            Err(record_error(
                limits,
                format::TYPES,
                "type reference uses an invalid absent sentinel",
            ))
        };
    }
    let (index, bound) = if value.0 & 0x8000_0000 == 0 {
        (value.0 as usize, module.types.len())
    } else {
        ((value.0 & 0x7fff_ffff) as usize, module.imports.len())
    };
    if index >= bound {
        Err(record_error(
            limits,
            format::TYPES,
            "type reference is out of range",
        ))
    } else if value.0 & 0x8000_0000 != 0 && module.imports[index].kind != 0 {
        Err(record_error(
            limits,
            format::TYPES,
            "type reference names a non-type import",
        ))
    } else {
        Ok(())
    }
}

fn checked_range(
    start: u32,
    count: u32,
    bound: usize,
    limits: &ArtifactLimits,
    kind: u16,
    detail: &'static str,
) -> Result<(), DiagnosticSet> {
    let end = start
        .checked_add(count)
        .and_then(|value| usize::try_from(value).ok());
    if end.is_some_and(|value| value <= bound) {
        Ok(())
    } else {
        Err(record_error(limits, kind, detail))
    }
}

fn string<'a>(
    bytes: &'a [u8],
    module: &DecodedModule,
    id: u32,
    limits: &ArtifactLimits,
) -> Result<&'a [u8], DiagnosticSet> {
    module
        .strings
        .get(id as usize)
        .map(|range| range.slice(bytes))
        .ok_or_else(|| record_error(limits, format::STRINGS, "string id is out of range"))
}

fn capability_key<'a>(
    bytes: &'a [u8],
    module: &DecodedModule,
    value: &Capability,
) -> (&'a [u8], &'a [u8], u16, u16) {
    (
        module.strings[value.namespace as usize].slice(bytes),
        module.strings[value.name as usize].slice(bytes),
        value.abi_major,
        value.minimum_abi_minor,
    )
}

fn import_key<'a>(
    bytes: &'a [u8],
    modules: &[DecodedModule],
    value: &Import,
) -> (ModuleId, u8, &'a [u8], TypeId) {
    (
        value.target_module,
        value.kind,
        modules[value.target_module.0 as usize].strings[value.target_name as usize].slice(bytes),
        value.expected_signature,
    )
}

fn export_key<'a>(
    bytes: &'a [u8],
    module: &DecodedModule,
    value: &Export,
) -> (u8, &'a [u8], TypeId) {
    (
        value.kind,
        module.strings[value.name as usize].slice(bytes),
        value.signature,
    )
}

fn record_error(limits: &ArtifactLimits, kind: u16, detail: &'static str) -> DiagnosticSet {
    single(limits, at(Code::BadRecord, kind, 0, detail))
}

fn finish(cursor: &Cursor<'_>, record: &[u8], valid: bool) -> Result<(), DiagnosticSet> {
    if valid && cursor.position() == record.len() {
        Ok(())
    } else {
        Err(raw(
            Code::BadRecord,
            "record shape or reserved fields are invalid",
        ))
    }
}

fn ru8(cursor: &mut Cursor<'_>) -> Result<u8, DiagnosticSet> {
    cursor.read_u8().map_err(single_raw)
}
fn ru16(cursor: &mut Cursor<'_>) -> Result<u16, DiagnosticSet> {
    cursor.read_u16().map_err(single_raw)
}
fn ru32(cursor: &mut Cursor<'_>) -> Result<u32, DiagnosticSet> {
    cursor.read_u32().map_err(single_raw)
}
fn raw(code: Code, detail: &'static str) -> DiagnosticSet {
    single_raw(Diagnostic::at_offset(Family::Section, code, 0, detail))
}

fn decode_manifest(
    container: &super::container::Container<'_>,
    entry: &super::container::DirectoryEntry,
    limits: &ArtifactLimits,
) -> Result<Manifest, DiagnosticSet> {
    if entry.length != 112 || entry.element_count != 1 {
        return Err(single(
            limits,
            at(
                Code::BadRecord,
                format::MANIFEST,
                entry.offset as usize,
                "manifest shape is not v1",
            ),
        ));
    }
    let start = entry.offset as usize;
    let mut cursor = Cursor::new(&container.bytes[start..start + 112]);
    let mut values = Vec::with_capacity(10);
    for _ in 0..10 {
        values.push(
            cursor
                .read_u32()
                .map_err(|error| single(limits, relocate(error, start)))?,
        );
    }
    let values: [u32; 10] = values.try_into().expect("ten manifest values");
    let compiler_abi = cursor
        .take(32)
        .map_err(|error| single(limits, relocate(error, start)))?
        .try_into()
        .unwrap();
    let standard_library_abi = cursor
        .take(32)
        .map_err(|error| single(limits, relocate(error, start)))?
        .try_into()
        .unwrap();
    if cursor
        .take(8)
        .map_err(|error| single(limits, relocate(error, start)))?
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(single(
            limits,
            at(
                Code::BadRecord,
                format::MANIFEST,
                start + 104,
                "manifest reserved bytes are non-zero",
            ),
        ));
    }
    Ok(Manifest {
        required_heap_bytes: values[0],
        required_stack_bytes: values[1],
        maximum_coroutines: values[2],
        maximum_call_depth: values[3],
        maximum_host_requests: values[4],
        maximum_events: values[5],
        maximum_block_cost: values[6],
        minimum_slice_cost: values[7],
        required_capabilities: values[8],
        optional_capabilities: values[9],
        compiler_abi,
        standard_library_abi,
    })
}

fn find<'a>(
    container: &'a super::container::Container<'_>,
    scope: u32,
    kind: u16,
    limits: &ArtifactLimits,
) -> Result<&'a super::container::DirectoryEntry, DiagnosticSet> {
    container
        .directory
        .iter()
        .find(|entry| entry.scope == scope && entry.kind == kind)
        .ok_or_else(|| {
            single(
                limits,
                at(Code::BadSection, kind, 0, "required section is missing"),
            )
        })
}

fn read(cursor: &mut Cursor<'_>, kind: u16, base: usize) -> Result<u32, DiagnosticSet> {
    cursor
        .read_u32()
        .map_err(|error| single_raw(relocate_section(error, base, kind)))
}

fn relocate(mut error: Diagnostic, base: usize) -> Diagnostic {
    error.location.offset = error
        .location
        .offset
        .and_then(|offset| offset.checked_add(base as u64));
    error
}

fn relocate_section(error: Diagnostic, base: usize, kind: u16) -> Diagnostic {
    let mut error = relocate(error, base);
    error.family = Family::Section;
    error.location.section = Some(kind);
    error
}

fn at(code: Code, kind: u16, offset: usize, detail: &'static str) -> Diagnostic {
    let mut error = Diagnostic::at_offset(Family::Section, code, offset, detail);
    error.location.section = Some(kind);
    error
}

fn single(limits: &ArtifactLimits, error: Diagnostic) -> DiagnosticSet {
    let mut errors = DiagnosticSet::new(limits.diagnostics);
    errors.push(error);
    errors
}

fn single_raw(error: Diagnostic) -> DiagnosticSet {
    let mut errors = DiagnosticSet::new(1);
    errors.push(error);
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16(bytes: &mut Vec<u8>, value: u16) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn value_type_bytes(kind: u8, flags: u8, nominal: u32) -> Vec<u8> {
        let mut bytes = vec![kind, flags];
        u16(&mut bytes, 0);
        u32(&mut bytes, nominal);
        bytes
    }

    fn table(records: &[Vec<u8>], kind: u16) -> (Vec<u8>, Vec<u32>) {
        let mut bytes = Vec::new();
        let mut offsets = vec![0];
        for record in records {
            bytes.extend_from_slice(record);
            offsets.push(bytes.len() as u32);
        }
        assert!(!offsets.is_empty());
        let _ = kind;
        (bytes, offsets)
    }

    fn section<'a>(kind: u16, bytes: &'a [u8], offsets: Vec<u32>) -> IndexedSection<'a> {
        IndexedSection::from_test_records(kind, bytes, offsets)
    }

    #[test]
    fn decodes_every_v1_record_shape() {
        let limits = ArtifactLimits::default();

        let mut capability = Vec::new();
        u32(&mut capability, 0);
        u32(&mut capability, 1);
        u16(&mut capability, 1);
        u16(&mut capability, 2);
        u32(&mut capability, 1);
        u32(&mut capability, 3);
        u32(&mut capability, 0);
        let (bytes, offsets) = table(&[capability], format::CAPABILITIES);
        let values =
            parse_capabilities(&section(format::CAPABILITIES, &bytes, offsets), &limits).unwrap();
        assert_eq!(values[0].operation_count, 3);

        let mut class = vec![0, 0];
        u16(&mut class, 1);
        for value in [0, u32::MAX, 0, 0, 0, 0, 0] {
            u32(&mut class, value);
        }
        let mut interface = vec![1, 1];
        u16(&mut interface, 0);
        for value in [0, u32::MAX, 0, 0, 0, 0, 0] {
            u32(&mut interface, value);
        }
        let mut array = vec![2, 0];
        u16(&mut array, 0);
        u32(&mut array, 0);
        array.extend(value_type_bytes(1, 0, u32::MAX));
        let mut function_type = vec![3, 0];
        u16(&mut function_type, 0);
        u32(&mut function_type, 1);
        u16(&mut function_type, 1);
        u16(&mut function_type, 1);
        function_type.extend(value_type_bytes(0, 0, u32::MAX));
        function_type.extend(value_type_bytes(7, 1, 0));
        let (bytes, offsets) = table(&[class, interface, array, function_type], format::TYPES);
        let values = parse_types(&section(format::TYPES, &bytes, offsets), &limits).unwrap();
        assert!(matches!(values[0], NominalType::Class { .. }));
        assert!(matches!(values[1], NominalType::Interface { .. }));
        assert!(matches!(values[2], NominalType::Array { .. }));
        assert!(matches!(values[3], NominalType::Function { .. }));

        let constants = vec![
            [vec![0], 7_i32.to_le_bytes().to_vec()].concat(),
            [vec![1], 8_i64.to_le_bytes().to_vec()].concat(),
            [vec![2], 1_u32.to_le_bytes().to_vec()].concat(),
            [vec![3], 2_u64.to_le_bytes().to_vec()].concat(),
            vec![4, 1],
            [vec![5], ('x' as u32).to_le_bytes().to_vec()].concat(),
            [vec![6], 1_u32.to_le_bytes().to_vec()].concat(),
            vec![7],
        ];
        let (bytes, offsets) = table(&constants, format::CONSTANTS);
        let values =
            parse_constants(&section(format::CONSTANTS, &bytes, offsets), &limits).unwrap();
        assert_eq!(values.len(), 8);
        assert_eq!(values[4], Constant::Bool(true));
        assert_eq!(values[5], Constant::Char('x'));

        let mut import = vec![2, 0, 0, 0];
        for value in [1, 2, 3] {
            u32(&mut import, value);
        }
        import.extend([4; 32]);
        let (bytes, offsets) = table(&[import], format::IMPORTS);
        assert_eq!(
            parse_imports(&section(format::IMPORTS, &bytes, offsets), &limits).unwrap()[0]
                .target_module,
            ModuleId(1)
        );

        let mut export = vec![1, 1];
        u16(&mut export, 0);
        for value in [2, 3, 4] {
            u32(&mut export, value);
        }
        let (bytes, offsets) = table(&[export], format::EXPORTS);
        assert_eq!(
            parse_exports(&section(format::EXPORTS, &bytes, offsets), &limits).unwrap()[0]
                .signature,
            TypeId(4)
        );

        let mut field = Vec::new();
        u32(&mut field, 0);
        u32(&mut field, 1);
        field.extend(value_type_bytes(1, 0, u32::MAX));
        u32(&mut field, 3);
        u32(&mut field, 0);
        let (bytes, offsets) = table(&[field], format::FIELDS);
        assert_eq!(
            parse_fields(&section(format::FIELDS, &bytes, offsets), &limits).unwrap()[0].flags,
            3
        );

        let mut function = Vec::new();
        for value in [u32::MAX, 1, 2, 2] {
            u32(&mut function, value);
        }
        u16(&mut function, 1);
        u16(&mut function, 1);
        for value in [0, 1, 0, 0] {
            u32(&mut function, value);
        }
        function.extend(value_type_bytes(7, 0, 0));
        let (bytes, offsets) = table(&[function], format::FUNCTIONS);
        assert_eq!(
            parse_functions(&section(format::FUNCTIONS, &bytes, offsets), &limits).unwrap()[0]
                .registers,
            [ValueType {
                kind: 7,
                flags: 0,
                nominal_type: TypeId(0)
            }]
        );

        let mut block = Vec::new();
        for value in [0, 0, 2, 3, 1, 0] {
            u32(&mut block, value);
        }
        let (bytes, offsets) = table(&[block], format::BLOCKS);
        assert_eq!(
            parse_blocks(&section(format::BLOCKS, &bytes, offsets), &limits).unwrap()[0]
                .declared_fixed_cost,
            3
        );

        let mut exception = Vec::new();
        for value in [0, 0, 1, u32::MAX, 1] {
            u32(&mut exception, value);
        }
        u16(&mut exception, 0);
        u16(&mut exception, 0);
        let (bytes, offsets) = table(&[exception], format::EXCEPTIONS);
        assert_eq!(
            parse_exceptions(&section(format::EXCEPTIONS, &bytes, offsets), &limits).unwrap()[0]
                .handler_block,
            BlockId(1)
        );

        let mut debug = Vec::new();
        for value in [0, 0, 0, 1, 2, u32::MAX, 8] {
            u32(&mut debug, value);
        }
        debug.extend(b"src/a.kt");
        let (bytes, offsets) = table(&[debug], format::DEBUG);
        let values = parse_debug(&section(format::DEBUG, &bytes, offsets), &limits).unwrap();
        assert_eq!(values[0].source_path.slice(&bytes), b"src/a.kt");
    }

    #[test]
    fn rejects_noncanonical_record_shapes_and_limits() {
        let limits = ArtifactLimits::default();
        let mut invalid_capability = vec![0; 24];
        invalid_capability[12] = 3;
        let (bytes, offsets) = table(&[invalid_capability], format::CAPABILITIES);
        assert_eq!(
            parse_capabilities(&section(format::CAPABILITIES, &bytes, offsets), &limits)
                .unwrap_err()
                .first()
                .unwrap()
                .code,
            Code::BadRecord
        );

        let records = vec![vec![7], vec![7]];
        let (bytes, offsets) = table(&records, format::CONSTANTS);
        assert_eq!(
            parse_constants(&section(format::CONSTANTS, &bytes, offsets), &limits)
                .unwrap_err()
                .first()
                .unwrap()
                .code,
            Code::BadRecord
        );

        let mut function = Vec::new();
        for value in [u32::MAX, 0, 0, 2] {
            u32(&mut function, value);
        }
        u16(&mut function, 1);
        u16(&mut function, 0);
        for value in [0, 1, 0, 0] {
            u32(&mut function, value);
        }
        function.extend(value_type_bytes(1, 0, u32::MAX));
        let (bytes, offsets) = table(&[function], format::FUNCTIONS);
        let strict = ArtifactLimits {
            registers_per_function: 0,
            ..limits
        };
        assert_eq!(
            parse_functions(&section(format::FUNCTIONS, &bytes, offsets), &strict)
                .unwrap_err()
                .first()
                .unwrap()
                .code,
            Code::LimitExceeded
        );
    }
}
