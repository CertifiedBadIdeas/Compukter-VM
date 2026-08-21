use crate::{
    bytes::Cursor,
    decode::container::{Container, DirectoryEntry},
    diagnostic::{Code, Diagnostic, Family},
    limits::ArtifactLimits,
};

#[derive(Debug)]
pub(crate) struct IndexedSection<'a> {
    pub kind: u16,
    offsets: Vec<u32>,
    records: &'a [u8],
    records_offset: usize,
}

impl<'a> IndexedSection<'a> {
    #[cfg(test)]
    pub(crate) fn from_test_records(kind: u16, records: &'a [u8], offsets: Vec<u32>) -> Self {
        Self {
            kind,
            offsets,
            records,
            records_offset: 0,
        }
    }

    pub(crate) fn decode(
        container: &Container<'a>,
        entry: &DirectoryEntry,
        limits: &ArtifactLimits,
    ) -> Result<Self, Diagnostic> {
        let section_offset = usize::try_from(entry.offset).map_err(|_| {
            error(
                entry,
                0,
                Code::BadRecord,
                "section offset does not fit usize",
            )
        })?;
        let section_length = usize::try_from(entry.length).map_err(|_| {
            error(
                entry,
                0,
                Code::BadRecord,
                "section length does not fit usize",
            )
        })?;
        let section_end = section_offset
            .checked_add(section_length)
            .ok_or_else(|| error(entry, 0, Code::BadRecord, "section range overflows usize"))?;
        let payload = container
            .bytes
            .get(section_offset..section_end)
            .ok_or_else(|| {
                error(
                    entry,
                    0,
                    Code::BadRecord,
                    "section range is outside artifact",
                )
            })?;

        let mut cursor = Cursor::new(payload);
        let count = read(entry, section_offset, cursor.read_u32())?;
        let reserved = read(entry, section_offset, cursor.read_u32())?;
        let record_bytes_u64 = read(entry, section_offset, cursor.read_u64())?;
        if reserved != 0 || count != entry.element_count {
            return Err(error(
                entry,
                0,
                Code::BadRecord,
                "indexed header disagrees with directory or has reserved bits",
            ));
        }
        let count = usize::try_from(count).map_err(|_| {
            error(
                entry,
                0,
                Code::LimitExceeded,
                "record count does not fit usize",
            )
        })?;
        if count > limits.records_per_section {
            return Err(error(
                entry,
                0,
                Code::LimitExceeded,
                "record count limit exceeded",
            ));
        }
        let offset_count = count
            .checked_add(1)
            .ok_or_else(|| error(entry, 0, Code::BadRecord, "offset count overflows usize"))?;
        let mut offsets = Vec::new();
        offsets.try_reserve_exact(offset_count).map_err(|_| {
            error(
                entry,
                0,
                Code::LimitExceeded,
                "cannot reserve bounded offset table",
            )
        })?;
        for _ in 0..offset_count {
            offsets.push(read(entry, section_offset, cursor.read_u32())?);
        }

        let records_offset_in_section = align8(cursor.position()).ok_or_else(|| {
            error(
                entry,
                cursor.position(),
                Code::BadRecord,
                "indexed prefix alignment overflows",
            )
        })?;
        let padding = read(
            entry,
            section_offset,
            cursor.take(records_offset_in_section - cursor.position()),
        )?;
        if padding.iter().any(|byte| *byte != 0) {
            return Err(error(
                entry,
                cursor.position() - padding.len(),
                Code::BadRecord,
                "indexed prefix padding is non-zero",
            ));
        }

        let record_bytes = usize::try_from(record_bytes_u64).map_err(|_| {
            error(
                entry,
                8,
                Code::BadRecord,
                "record byte length does not fit usize",
            )
        })?;
        let expected_payload = records_offset_in_section
            .checked_add(record_bytes)
            .ok_or_else(|| error(entry, 8, Code::BadRecord, "record byte range overflows"))?;
        if expected_payload != payload.len()
            || offsets.first() != Some(&0)
            || offsets.last().copied() != u32::try_from(record_bytes).ok()
            || offsets.windows(2).any(|pair| pair[0] > pair[1])
        {
            return Err(error(
                entry,
                0,
                Code::BadRecord,
                "indexed offsets are not canonical",
            ));
        }

        Ok(Self {
            kind: entry.kind,
            offsets,
            records: &payload[records_offset_in_section..],
            records_offset: section_offset + records_offset_in_section,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    pub(crate) fn record(&self, id: u32) -> Result<&'a [u8], Diagnostic> {
        let id = usize::try_from(id)
            .map_err(|_| self.record_error(0, Code::BadRecord, "record id does not fit usize"))?;
        let start = *self
            .offsets
            .get(id)
            .ok_or_else(|| self.record_error(0, Code::BadRecord, "record id is out of range"))?
            as usize;
        let end =
            *self.offsets.get(id + 1).ok_or_else(|| {
                self.record_error(0, Code::BadRecord, "record end is out of range")
            })? as usize;
        self.records.get(start..end).ok_or_else(|| {
            self.record_error(start, Code::BadRecord, "record range is outside section")
        })
    }

    pub(crate) fn record_range(&self, id: u32) -> Result<std::ops::Range<usize>, Diagnostic> {
        let id = usize::try_from(id)
            .map_err(|_| self.record_error(0, Code::BadRecord, "record id does not fit usize"))?;
        let start = *self
            .offsets
            .get(id)
            .ok_or_else(|| self.record_error(0, Code::BadRecord, "record id is out of range"))?
            as usize;
        let end =
            *self.offsets.get(id + 1).ok_or_else(|| {
                self.record_error(0, Code::BadRecord, "record end is out of range")
            })? as usize;
        Ok(self.records_offset + start..self.records_offset + end)
    }

    pub(crate) fn record_bytes_len(&self) -> usize {
        self.records.len()
    }

    fn record_error(&self, relative: usize, code: Code, detail: &'static str) -> Diagnostic {
        let mut diagnostic = Diagnostic::at_offset(
            Family::Section,
            code,
            self.records_offset.saturating_add(relative),
            detail,
        );
        diagnostic.location.section = Some(self.kind);
        diagnostic
    }
}

pub(crate) fn decode_string_table<'a>(
    container: &Container<'a>,
    entry: &DirectoryEntry,
    limits: &ArtifactLimits,
) -> Result<Vec<&'a str>, Diagnostic> {
    let section = IndexedSection::decode(container, entry, limits)?;
    if section.records.len() > limits.strings_bytes {
        return Err(section.record_error(0, Code::LimitExceeded, "string byte limit exceeded"));
    }

    let mut strings = Vec::new();
    strings.try_reserve_exact(section.len()).map_err(|_| {
        section.record_error(
            0,
            Code::LimitExceeded,
            "cannot reserve bounded string table",
        )
    })?;
    for id in 0..section.len() {
        let record = section.record(id as u32)?;
        let value = std::str::from_utf8(record).map_err(|_| {
            section.record_error(
                record_offset(&section, id),
                Code::InvalidUtf8,
                "string is not UTF-8",
            )
        })?;
        if id > 0 && value.is_empty() {
            return Err(section.record_error(
                record_offset(&section, id),
                Code::BadRecord,
                "only string zero may be empty",
            ));
        }
        if strings.last().is_some_and(|previous| *previous >= value) {
            return Err(section.record_error(
                record_offset(&section, id),
                Code::BadRecord,
                "strings are not strictly ordered and unique",
            ));
        }
        strings.push(value);
    }
    Ok(strings)
}

fn record_offset(section: &IndexedSection<'_>, id: usize) -> usize {
    section.offsets[id] as usize
}

fn read<T>(
    entry: &DirectoryEntry,
    section_offset: usize,
    result: Result<T, Diagnostic>,
) -> Result<T, Diagnostic> {
    result.map_err(|mut diagnostic| {
        diagnostic.family = Family::Section;
        diagnostic.location.section = Some(entry.kind);
        diagnostic.location.offset = diagnostic
            .location
            .offset
            .and_then(|offset| offset.checked_add(section_offset as u64));
        diagnostic
    })
}

fn error(
    entry: &DirectoryEntry,
    relative_offset: usize,
    code: Code,
    detail: &'static str,
) -> Diagnostic {
    let absolute = usize::try_from(entry.offset)
        .ok()
        .and_then(|offset| offset.checked_add(relative_offset))
        .unwrap_or(relative_offset);
    let family = if code == Code::LimitExceeded {
        Family::Limit
    } else {
        Family::Section
    };
    let mut diagnostic = Diagnostic::at_offset(family, code, absolute, detail);
    diagnostic.location.section = Some(entry.kind);
    diagnostic
}

fn align8(value: usize) -> Option<usize> {
    value.checked_add(7).map(|value| value & !7)
}
