/*
 * The Compukters Developers
 *
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::collections::BTreeSet;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::{FileSystemLimits, VirtualPath};

const MAGIC: &[u8; 8] = b"CPKTSYS\0";
const DIGEST_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemImageError {
    Malformed,
    UnsupportedVersion,
    LimitExceeded,
    DigestMismatch,
    NonCanonical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemEntryKind {
    Directory,
    File,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SystemEntry {
    pub path: VirtualPath,
    pub kind: SystemEntryKind,
    pub executable: bool,
    pub content: Arc<[u8]>,
}

/// A fully bounded and canonical installable system image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemImage {
    identity: [u8; DIGEST_BYTES],
    entries: Arc<[SystemEntry]>,
}

impl SystemImage {
    pub fn admit(bytes: Arc<[u8]>, limits: &FileSystemLimits) -> Result<Self, SystemImageError> {
        if bytes.len() > limits.maximum_system_image_bytes {
            return Err(SystemImageError::LimitExceeded);
        }
        if bytes.len() < 16 + DIGEST_BYTES {
            return Err(SystemImageError::Malformed);
        }
        let payload_end = bytes.len() - DIGEST_BYTES;
        let identity: [u8; DIGEST_BYTES] = bytes[payload_end..]
            .try_into()
            .map_err(|_| SystemImageError::Malformed)?;
        if Sha256::digest(&bytes[..payload_end]).as_slice() != identity {
            return Err(SystemImageError::DigestMismatch);
        }

        let mut cursor = Cursor::new(&bytes[..payload_end]);
        if cursor.read_exact(8)? != MAGIC {
            return Err(SystemImageError::Malformed);
        }
        if cursor.read_u16()? != 1 || cursor.read_u16()? != 0 {
            return Err(SystemImageError::UnsupportedVersion);
        }
        let count = cursor.read_u32()?;
        if count > limits.maximum_nodes {
            return Err(SystemImageError::LimitExceeded);
        }

        let mut entries = Vec::new();
        entries
            .try_reserve_exact(count as usize)
            .map_err(|_| SystemImageError::LimitExceeded)?;
        let mut directories = BTreeSet::new();
        let root =
            VirtualPath::parse_utf8("/", limits).map_err(|_| SystemImageError::LimitExceeded)?;
        directories.insert(root.clone());
        // The image contract seeds these ordinary directories even for an empty distribution.
        for path in ["/bin", "/boot"] {
            directories.insert(
                VirtualPath::parse_utf8(path, limits)
                    .map_err(|_| SystemImageError::LimitExceeded)?,
            );
        }
        let mut previous: Option<VirtualPath> = None;

        for _ in 0..count {
            let path_length = cursor.read_u32()? as usize;
            if path_length > limits.maximum_path_bytes {
                return Err(SystemImageError::LimitExceeded);
            }
            let path_bytes = cursor.read_exact(path_length)?;
            let path_text =
                std::str::from_utf8(path_bytes).map_err(|_| SystemImageError::Malformed)?;
            let path = VirtualPath::parse_utf8(path_text, limits)
                .map_err(|_| SystemImageError::NonCanonical)?;
            if path == root || path.components().next() == Some("home") {
                return Err(SystemImageError::NonCanonical);
            }
            if previous.as_ref().is_some_and(|previous| previous >= &path) {
                return Err(SystemImageError::NonCanonical);
            }
            let parent = path.parent().ok_or(SystemImageError::NonCanonical)?;
            if !directories.contains(&parent) {
                return Err(SystemImageError::NonCanonical);
            }

            let kind = cursor.read_u8()?;
            let flags = cursor.read_u8()?;
            if cursor.read_u16()? != 0 {
                return Err(SystemImageError::Malformed);
            }
            let content_length = cursor.read_u64()?;
            let (kind, executable, content) = match kind {
                1 if flags == 0 && content_length == 0 => {
                    directories.insert(path.clone());
                    (SystemEntryKind::Directory, false, Arc::from([]))
                }
                2 if flags & !1 == 0 => {
                    if content_length > limits.maximum_file_bytes {
                        return Err(SystemImageError::LimitExceeded);
                    }
                    let length = usize::try_from(content_length)
                        .map_err(|_| SystemImageError::LimitExceeded)?;
                    let content: Arc<[u8]> = Arc::from(cursor.read_exact(length)?);
                    (SystemEntryKind::File, flags & 1 != 0, content)
                }
                _ => return Err(SystemImageError::Malformed),
            };
            previous = Some(path.clone());
            entries.push(SystemEntry {
                path,
                kind,
                executable,
                content,
            });
        }
        if !cursor.is_finished() {
            return Err(SystemImageError::Malformed);
        }
        Ok(Self {
            identity,
            entries: entries.into(),
        })
    }

    pub fn identity(&self) -> [u8; DIGEST_BYTES] {
        self.identity
    }

    pub(crate) fn entries(&self) -> &[SystemEntry] {
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

    fn read_exact(&mut self, length: usize) -> Result<&'a [u8], SystemImageError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(SystemImageError::Malformed)?;
        let result = self
            .bytes
            .get(self.offset..end)
            .ok_or(SystemImageError::Malformed)?;
        self.offset = end;
        Ok(result)
    }

    fn read_u8(&mut self) -> Result<u8, SystemImageError> {
        Ok(self.read_exact(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16, SystemImageError> {
        Ok(u16::from_le_bytes(
            self.read_exact(2)?.try_into().expect("exact width"),
        ))
    }

    fn read_u32(&mut self) -> Result<u32, SystemImageError> {
        Ok(u32::from_le_bytes(
            self.read_exact(4)?.try_into().expect("exact width"),
        ))
    }

    fn read_u64(&mut self) -> Result<u64, SystemImageError> {
        Ok(u64::from_le_bytes(
            self.read_exact(8)?.try_into().expect("exact width"),
        ))
    }

    fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}
