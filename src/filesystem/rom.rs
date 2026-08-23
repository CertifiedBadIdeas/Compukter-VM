/*
 * The Compukters Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 */

use std::collections::BTreeSet;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::{FileSystemLimits, VirtualPath};

const MAGIC: &[u8; 8] = b"CPKTROM\0";
const DIGEST_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RomImageError {
    Malformed,
    UnsupportedVersion,
    LimitExceeded,
    DigestMismatch,
    NonCanonical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RomEntryKind {
    Directory,
    File,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RomEntry {
    pub path: VirtualPath,
    pub kind: RomEntryKind,
    pub executable: bool,
    pub content: Arc<[u8]>,
}

/// A fully bounded and canonical immutable ROM image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RomImage {
    identity: [u8; DIGEST_BYTES],
    entries: Arc<[RomEntry]>,
}

impl RomImage {
    pub fn admit(bytes: Arc<[u8]>, limits: &FileSystemLimits) -> Result<Self, RomImageError> {
        if bytes.len() > limits.maximum_rom_bytes {
            return Err(RomImageError::LimitExceeded);
        }
        if bytes.len() < 16 + DIGEST_BYTES {
            return Err(RomImageError::Malformed);
        }
        let payload_end = bytes.len() - DIGEST_BYTES;
        let identity: [u8; DIGEST_BYTES] = bytes[payload_end..]
            .try_into()
            .map_err(|_| RomImageError::Malformed)?;
        if Sha256::digest(&bytes[..payload_end]).as_slice() != identity {
            return Err(RomImageError::DigestMismatch);
        }

        let mut cursor = Cursor::new(&bytes[..payload_end]);
        if cursor.read_exact(8)? != MAGIC {
            return Err(RomImageError::Malformed);
        }
        if cursor.read_u16()? != 1 || cursor.read_u16()? != 0 {
            return Err(RomImageError::UnsupportedVersion);
        }
        let count = cursor.read_u32()?;
        if count > limits.maximum_nodes.saturating_sub(2) {
            return Err(RomImageError::LimitExceeded);
        }

        let mut entries = Vec::new();
        entries
            .try_reserve_exact(count as usize)
            .map_err(|_| RomImageError::LimitExceeded)?;
        let mut directories = BTreeSet::new();
        let rom_root =
            VirtualPath::parse_utf8("/rom", limits).map_err(|_| RomImageError::LimitExceeded)?;
        directories.insert(rom_root.clone());
        let mut previous: Option<VirtualPath> = None;

        for _ in 0..count {
            let path_length = cursor.read_u32()? as usize;
            if path_length > limits.maximum_path_bytes {
                return Err(RomImageError::LimitExceeded);
            }
            let path_bytes = cursor.read_exact(path_length)?;
            let path_text =
                std::str::from_utf8(path_bytes).map_err(|_| RomImageError::Malformed)?;
            let path = VirtualPath::parse_utf8(path_text, limits)
                .map_err(|_| RomImageError::NonCanonical)?;
            if !path.is_within(&rom_root) || path == rom_root {
                return Err(RomImageError::NonCanonical);
            }
            if previous.as_ref().is_some_and(|previous| previous >= &path) {
                return Err(RomImageError::NonCanonical);
            }
            let parent = path.parent().ok_or(RomImageError::NonCanonical)?;
            if !directories.contains(&parent) {
                return Err(RomImageError::NonCanonical);
            }

            let kind = cursor.read_u8()?;
            let flags = cursor.read_u8()?;
            if cursor.read_u16()? != 0 {
                return Err(RomImageError::Malformed);
            }
            let content_length = cursor.read_u64()?;
            let (kind, executable, content) = match kind {
                1 if flags == 0 && content_length == 0 => {
                    directories.insert(path.clone());
                    (RomEntryKind::Directory, false, Arc::from([]))
                }
                2 if flags & !1 == 0 => {
                    if content_length > limits.maximum_file_bytes {
                        return Err(RomImageError::LimitExceeded);
                    }
                    let length = usize::try_from(content_length)
                        .map_err(|_| RomImageError::LimitExceeded)?;
                    let content: Arc<[u8]> = Arc::from(cursor.read_exact(length)?);
                    (RomEntryKind::File, flags & 1 != 0, content)
                }
                _ => return Err(RomImageError::Malformed),
            };
            previous = Some(path.clone());
            entries.push(RomEntry {
                path,
                kind,
                executable,
                content,
            });
        }
        if !cursor.is_finished() {
            return Err(RomImageError::Malformed);
        }
        Ok(Self {
            identity,
            entries: entries.into(),
        })
    }

    pub fn identity(&self) -> [u8; DIGEST_BYTES] {
        self.identity
    }

    pub(crate) fn entries(&self) -> &[RomEntry] {
        &self.entries
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_exact(&mut self, length: usize) -> Result<&'a [u8], RomImageError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(RomImageError::Malformed)?;
        let result = self
            .bytes
            .get(self.offset..end)
            .ok_or(RomImageError::Malformed)?;
        self.offset = end;
        Ok(result)
    }

    fn read_u8(&mut self) -> Result<u8, RomImageError> {
        Ok(self.read_exact(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16, RomImageError> {
        Ok(u16::from_le_bytes(
            self.read_exact(2)?.try_into().expect("exact width"),
        ))
    }

    fn read_u32(&mut self) -> Result<u32, RomImageError> {
        Ok(u32::from_le_bytes(
            self.read_exact(4)?.try_into().expect("exact width"),
        ))
    }

    fn read_u64(&mut self) -> Result<u64, RomImageError> {
        Ok(u64::from_le_bytes(
            self.read_exact(8)?.try_into().expect("exact width"),
        ))
    }

    fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}
