use sha2::{Digest, Sha256};

use crate::{
    artifact::format,
    bytes::Cursor,
    diagnostic::{Code, Diagnostic, DiagnosticSet, Family},
    limits::ArtifactLimits,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Header {
    pub section_count: u32,
    pub semantic_features: u32,
    pub payload_end: u64,
    pub entry_module: u32,
    pub entry_function: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryEntry {
    pub kind: u16,
    pub flags: u16,
    pub scope: u32,
    pub offset: u64,
    pub length: u64,
    pub element_count: u32,
}

#[derive(Debug)]
pub(crate) struct Container<'a> {
    #[cfg_attr(not(test), allow(dead_code))]
    pub bytes: &'a [u8],
    pub header: Header,
    pub directory: Vec<DirectoryEntry>,
}

pub(crate) fn decode_container<'a>(
    bytes: &'a [u8],
    limits: &ArtifactLimits,
) -> Result<Container<'a>, DiagnosticSet> {
    if bytes.len() > limits.artifact_bytes {
        return Err(single(
            limits,
            diagnostic(
                Family::Limit,
                Code::LimitExceeded,
                0,
                "artifact byte limit exceeded",
            ),
        ));
    }
    if bytes.len() < format::HEADER_SIZE + format::DIGEST_SIZE {
        return Err(single(
            limits,
            diagnostic(
                Family::Container,
                Code::BadLength,
                0,
                "artifact is shorter than header and digest",
            ),
        ));
    }

    let header = decode_header(bytes, limits)?;
    let payload_end = usize::try_from(header.payload_end).map_err(|_| {
        single(
            limits,
            diagnostic(
                Family::Container,
                Code::BadLength,
                32,
                "payload_end does not fit host usize",
            ),
        )
    })?;
    let expected_length = payload_end
        .checked_add(format::DIGEST_SIZE)
        .ok_or_else(|| {
            single(
                limits,
                diagnostic(
                    Family::Container,
                    Code::BadLength,
                    32,
                    "artifact length overflows usize",
                ),
            )
        })?;
    if expected_length != bytes.len() {
        return Err(single(
            limits,
            diagnostic(
                Family::Container,
                Code::BadLength,
                32,
                "payload_end does not match file length",
            ),
        ));
    }

    let actual_digest = Sha256::digest(&bytes[..payload_end]);
    if actual_digest.as_slice() != &bytes[payload_end..] {
        return Err(single(
            limits,
            diagnostic(
                Family::Container,
                Code::BadDigest,
                payload_end,
                "SHA-256 trailer mismatch",
            ),
        ));
    }

    let directory = decode_directory(bytes, header, limits)?;
    Ok(Container {
        bytes,
        header,
        directory,
    })
}

fn decode_header(bytes: &[u8], limits: &ArtifactLimits) -> Result<Header, DiagnosticSet> {
    let mut cursor = Cursor::new(&bytes[..format::HEADER_SIZE]);
    if cursor.take(4).map_err(|error| single(limits, error))? != b"CPKT" {
        return Err(single(
            limits,
            diagnostic(Family::Container, Code::BadMagic, 0, "magic is not CPKT"),
        ));
    }
    let format_major = cursor.read_u16().map_err(|error| single(limits, error))?;
    let _format_minor = cursor.read_u16().map_err(|error| single(limits, error))?;
    let runtime_major = cursor.read_u16().map_err(|error| single(limits, error))?;
    let runtime_minor = cursor.read_u16().map_err(|error| single(limits, error))?;
    let header_size = cursor.read_u16().map_err(|error| single(limits, error))?;
    let directory_entry_size = cursor.read_u16().map_err(|error| single(limits, error))?;
    let section_count = cursor.read_u32().map_err(|error| single(limits, error))?;
    let semantic_features = cursor.read_u32().map_err(|error| single(limits, error))?;
    let directory_offset = cursor.read_u64().map_err(|error| single(limits, error))?;
    let payload_end = cursor.read_u64().map_err(|error| single(limits, error))?;
    let entry_module = cursor.read_u32().map_err(|error| single(limits, error))?;
    let entry_function = cursor.read_u32().map_err(|error| single(limits, error))?;
    let reserved = cursor.take(16).map_err(|error| single(limits, error))?;

    if format_major != format::FORMAT_MAJOR
        || runtime_major > format::RUNTIME_ABI_MAJOR
        || (runtime_major == format::RUNTIME_ABI_MAJOR && runtime_minor > format::RUNTIME_ABI_MINOR)
        || semantic_features & !format::KNOWN_FEATURES != 0
    {
        return Err(single(
            limits,
            diagnostic(
                Family::Container,
                Code::UnsupportedVersion,
                4,
                "unsupported format, runtime ABI, or feature bits",
            ),
        ));
    }
    if usize::from(header_size) != format::HEADER_SIZE
        || usize::from(directory_entry_size) != format::DIRECTORY_ENTRY_SIZE
        || directory_offset != format::HEADER_SIZE as u64
        || reserved.iter().any(|byte| *byte != 0)
    {
        return Err(single(
            limits,
            diagnostic(
                Family::Container,
                Code::BadLength,
                12,
                "non-canonical fixed header fields",
            ),
        ));
    }
    if section_count as usize > limits.sections {
        return Err(single(
            limits,
            diagnostic(
                Family::Limit,
                Code::LimitExceeded,
                16,
                "section count limit exceeded",
            ),
        ));
    }

    Ok(Header {
        section_count,
        semantic_features,
        payload_end,
        entry_module,
        entry_function,
    })
}

fn decode_directory(
    bytes: &[u8],
    header: Header,
    limits: &ArtifactLimits,
) -> Result<Vec<DirectoryEntry>, DiagnosticSet> {
    let count = header.section_count as usize;
    let directory_bytes = count
        .checked_mul(format::DIRECTORY_ENTRY_SIZE)
        .ok_or_else(|| {
            single(
                limits,
                diagnostic(
                    Family::Container,
                    Code::BadDirectory,
                    16,
                    "directory size overflows usize",
                ),
            )
        })?;
    let directory_end = format::HEADER_SIZE
        .checked_add(directory_bytes)
        .ok_or_else(|| {
            single(
                limits,
                diagnostic(
                    Family::Container,
                    Code::BadDirectory,
                    16,
                    "directory end overflows usize",
                ),
            )
        })?;
    let payload_end = header.payload_end as usize;
    if directory_end > payload_end {
        return Err(single(
            limits,
            diagnostic(
                Family::Container,
                Code::BadDirectory,
                16,
                "directory extends into trailer",
            ),
        ));
    }

    let mut cursor = Cursor::new(&bytes[format::HEADER_SIZE..directory_end]);
    let mut entries = Vec::new();
    entries.try_reserve_exact(count).map_err(|_| {
        single(
            limits,
            diagnostic(
                Family::Limit,
                Code::LimitExceeded,
                16,
                "cannot reserve bounded directory",
            ),
        )
    })?;

    let mut previous_key = None;
    let mut expected_offset = align8(directory_end).ok_or_else(|| {
        single(
            limits,
            diagnostic(
                Family::Container,
                Code::BadDirectory,
                directory_end,
                "alignment overflows usize",
            ),
        )
    })?;
    check_zero(bytes, directory_end, expected_offset, limits)?;

    for index in 0..count {
        let entry_offset = format::HEADER_SIZE + index * format::DIRECTORY_ENTRY_SIZE;
        let entry = DirectoryEntry {
            kind: cursor
                .read_u16()
                .map_err(|error| single(limits, relocate(error, format::HEADER_SIZE)))?,
            flags: cursor
                .read_u16()
                .map_err(|error| single(limits, relocate(error, format::HEADER_SIZE)))?,
            scope: cursor
                .read_u32()
                .map_err(|error| single(limits, relocate(error, format::HEADER_SIZE)))?,
            offset: cursor
                .read_u64()
                .map_err(|error| single(limits, relocate(error, format::HEADER_SIZE)))?,
            length: cursor
                .read_u64()
                .map_err(|error| single(limits, relocate(error, format::HEADER_SIZE)))?,
            element_count: cursor
                .read_u32()
                .map_err(|error| single(limits, relocate(error, format::HEADER_SIZE)))?,
        };
        let reserved = cursor
            .read_u32()
            .map_err(|error| single(limits, relocate(error, format::HEADER_SIZE)))?;
        validate_entry(entry, reserved, entry_offset, limits)?;

        let key = (entry.scope, entry.kind);
        if previous_key.is_some_and(|previous| previous >= key) {
            return Err(single(
                limits,
                diagnostic(
                    Family::Container,
                    Code::BadDirectory,
                    entry_offset,
                    "directory keys are not strictly ordered",
                ),
            ));
        }
        previous_key = Some(key);

        let offset = usize::try_from(entry.offset).map_err(|_| {
            single(
                limits,
                diagnostic(
                    Family::Container,
                    Code::BadDirectory,
                    entry_offset + 8,
                    "section offset does not fit usize",
                ),
            )
        })?;
        let length = usize::try_from(entry.length).map_err(|_| {
            single(
                limits,
                diagnostic(
                    Family::Container,
                    Code::BadDirectory,
                    entry_offset + 16,
                    "section length does not fit usize",
                ),
            )
        })?;
        if offset != expected_offset || offset % 8 != 0 {
            return Err(single(
                limits,
                diagnostic(
                    Family::Container,
                    Code::BadDirectory,
                    entry_offset + 8,
                    "section is not canonically packed and aligned",
                ),
            ));
        }
        let end = offset.checked_add(length).ok_or_else(|| {
            single(
                limits,
                diagnostic(
                    Family::Container,
                    Code::BadDirectory,
                    entry_offset + 16,
                    "section range overflows usize",
                ),
            )
        })?;
        if end > payload_end {
            return Err(single(
                limits,
                diagnostic(
                    Family::Container,
                    Code::BadDirectory,
                    entry_offset + 16,
                    "section extends beyond payload_end",
                ),
            ));
        }
        expected_offset = align8(end).ok_or_else(|| {
            single(
                limits,
                diagnostic(
                    Family::Container,
                    Code::BadDirectory,
                    entry_offset,
                    "section alignment overflows usize",
                ),
            )
        })?;
        check_zero(bytes, end, expected_offset.min(payload_end), limits)?;
        entries.push(entry);
    }

    let last_end = entries
        .last()
        .and_then(|entry| entry.offset.checked_add(entry.length))
        .unwrap_or(align8(directory_end).unwrap_or(directory_end) as u64);
    if last_end != header.payload_end {
        return Err(single(
            limits,
            diagnostic(
                Family::Container,
                Code::BadDirectory,
                directory_end,
                "last section does not end at payload_end",
            ),
        ));
    }
    Ok(entries)
}

fn validate_entry(
    entry: DirectoryEntry,
    reserved: u32,
    offset: usize,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let known = format::is_global(entry.kind) || format::is_module(entry.kind);
    if reserved != 0 || entry.flags & !format::KNOWN_FLAGS != 0 {
        return Err(single(
            limits,
            diagnostic(
                Family::Container,
                Code::BadDirectory,
                offset,
                "reserved directory bits are non-zero",
            ),
        ));
    }
    if format::is_global(entry.kind) && entry.scope != 0
        || format::is_module(entry.kind) && entry.scope == 0
    {
        return Err(single(
            limits,
            diagnostic(
                Family::Section,
                Code::BadSection,
                offset,
                "section has the wrong scope",
            ),
        ));
    }
    if known {
        let expected_flags = if entry.kind == format::DEBUG {
            0
        } else {
            format::KNOWN_FLAGS
        };
        if entry.flags != expected_flags {
            return Err(single(
                limits,
                diagnostic(
                    Family::Section,
                    Code::BadSection,
                    offset,
                    "known section has non-canonical flags",
                ),
            ));
        }
    } else if entry.kind < format::OPTIONAL_EXTENSION_START || entry.flags != 0 {
        return Err(single(
            limits,
            diagnostic(
                Family::Section,
                Code::BadSection,
                offset,
                "unknown required or semantic section",
            ),
        ));
    }
    if entry.element_count as usize > limits.records_per_section {
        return Err(single(
            limits,
            diagnostic(
                Family::Limit,
                Code::LimitExceeded,
                offset + 24,
                "section record limit exceeded",
            ),
        ));
    }
    Ok(())
}

fn check_zero(
    bytes: &[u8],
    start: usize,
    end: usize,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    if bytes
        .get(start..end)
        .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
    {
        return Err(single(
            limits,
            diagnostic(
                Family::Container,
                Code::BadDirectory,
                start,
                "alignment gap is missing or non-zero",
            ),
        ));
    }
    Ok(())
}

fn align8(value: usize) -> Option<usize> {
    value.checked_add(7).map(|value| value & !7)
}

fn relocate(mut diagnostic: Diagnostic, base: usize) -> Diagnostic {
    diagnostic.location.offset = diagnostic
        .location
        .offset
        .and_then(|offset| offset.checked_add(base as u64));
    diagnostic
}

fn diagnostic(family: Family, code: Code, offset: usize, detail: &'static str) -> Diagnostic {
    Diagnostic::at_offset(family, code, offset, detail)
}

fn single(limits: &ArtifactLimits, diagnostic: Diagnostic) -> DiagnosticSet {
    let mut diagnostics = DiagnosticSet::new(limits.diagnostics);
    diagnostics.push(diagnostic);
    diagnostics
}
