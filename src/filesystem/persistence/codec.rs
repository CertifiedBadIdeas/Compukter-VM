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

use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::PersistenceCodecError;
use crate::filesystem::{FileSystemLimits, VirtualPath};

pub(crate) const DIGEST_BYTES: usize = 32;

pub(crate) struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub fn with_capacity(capacity: usize) -> Result<Self, PersistenceCodecError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| PersistenceCodecError::LimitExceeded)?;
        Ok(Self { bytes })
    }

    pub fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    pub fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    pub fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    pub fn path(&mut self, path: &VirtualPath) -> Result<(), PersistenceCodecError> {
        let length = path.encoded_len();
        self.u32(u32::try_from(length).map_err(|_| PersistenceCodecError::LimitExceeded)?);
        self.bytes(b"/");
        let mut components = path.components();
        if let Some(first) = components.next() {
            self.bytes(first.as_bytes());
            for component in components {
                self.bytes(b"/");
                self.bytes(component.as_bytes());
            }
        }
        Ok(())
    }

    pub fn finish_checked(self, maximum: usize) -> Result<Arc<[u8]>, PersistenceCodecError> {
        let length = self
            .bytes
            .len()
            .checked_add(DIGEST_BYTES)
            .ok_or(PersistenceCodecError::LimitExceeded)?;
        if length > maximum {
            return Err(PersistenceCodecError::LimitExceeded);
        }
        let mut bytes = self.bytes;
        let digest = Sha256::digest(&bytes);
        bytes.extend_from_slice(&digest);
        Ok(bytes.into())
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

pub(crate) fn path_record_len(path: &VirtualPath) -> Result<usize, PersistenceCodecError> {
    4_usize
        .checked_add(path.encoded_len())
        .ok_or(PersistenceCodecError::LimitExceeded)
}

pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    pub fn verified(bytes: &'a [u8], maximum: usize) -> Result<Self, PersistenceCodecError> {
        if bytes.len() > maximum {
            return Err(PersistenceCodecError::LimitExceeded);
        }
        if bytes.len() < DIGEST_BYTES {
            return Err(PersistenceCodecError::Malformed);
        }
        let payload_end = bytes.len() - DIGEST_BYTES;
        let expected = &bytes[payload_end..];
        if Sha256::digest(&bytes[..payload_end]).as_slice() != expected {
            return Err(PersistenceCodecError::DigestMismatch);
        }
        Ok(Self {
            bytes: &bytes[..payload_end],
            offset: 0,
        })
    }

    pub fn plain(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub fn exact(&mut self, length: usize) -> Result<&'a [u8], PersistenceCodecError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(PersistenceCodecError::Malformed)?;
        let result = self
            .bytes
            .get(self.offset..end)
            .ok_or(PersistenceCodecError::Malformed)?;
        self.offset = end;
        Ok(result)
    }

    pub fn u8(&mut self) -> Result<u8, PersistenceCodecError> {
        Ok(self.exact(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, PersistenceCodecError> {
        Ok(u16::from_le_bytes(
            self.exact(2)?.try_into().expect("exact width"),
        ))
    }

    pub fn u32(&mut self) -> Result<u32, PersistenceCodecError> {
        Ok(u32::from_le_bytes(
            self.exact(4)?.try_into().expect("exact width"),
        ))
    }

    pub fn u64(&mut self) -> Result<u64, PersistenceCodecError> {
        Ok(u64::from_le_bytes(
            self.exact(8)?.try_into().expect("exact width"),
        ))
    }

    pub fn path(
        &mut self,
        limits: &FileSystemLimits,
    ) -> Result<VirtualPath, PersistenceCodecError> {
        let length = self.u32()? as usize;
        if length > limits.maximum_path_bytes {
            return Err(PersistenceCodecError::LimitExceeded);
        }
        let text = std::str::from_utf8(self.exact(length)?)
            .map_err(|_| PersistenceCodecError::Malformed)?;
        VirtualPath::parse_utf8(text, limits).map_err(|_| PersistenceCodecError::NonCanonical)
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    pub fn finish(self) -> Result<(), PersistenceCodecError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(PersistenceCodecError::Malformed)
        }
    }
}

pub(crate) fn home_descendant(path: &VirtualPath) -> bool {
    let mut components = path.components();
    components.next() == Some("home") && components.next().is_some()
}
