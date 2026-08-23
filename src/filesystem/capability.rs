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

use std::ops::{BitOr, BitOrAssign};

use super::{FileSystemError, VirtualPath};

/// A validated set of filesystem operations.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileRights(u16);

impl FileRights {
    pub const INSPECT: Self = Self(1 << 0);
    pub const LIST: Self = Self(1 << 1);
    pub const READ: Self = Self(1 << 2);
    pub const CREATE: Self = Self(1 << 3);
    pub const WRITE: Self = Self(1 << 4);
    pub const DELETE: Self = Self(1 << 5);
    pub const RENAME: Self = Self(1 << 6);
    pub const EXECUTE: Self = Self(1 << 7);
    pub const OWNER: Self = Self((1 << 8) - 1);

    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }
}

impl BitOr for FileRights {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for FileRights {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Unforgeable-by-guest authority over one exact virtual subtree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileCapability {
    root: VirtualPath,
    rights: FileRights,
    logical_byte_limit: u64,
    operation_limit: u64,
    handle_limit: u32,
}

impl FileCapability {
    pub fn new(root: VirtualPath, rights: FileRights) -> Self {
        Self {
            root,
            rights,
            logical_byte_limit: u64::MAX,
            operation_limit: u64::MAX,
            handle_limit: u32::MAX,
        }
    }

    pub fn permits(&self, path: &VirtualPath, required: FileRights) -> bool {
        path.is_within(&self.root) && self.rights.contains(required)
    }

    pub fn delegate(&self, root: VirtualPath, rights: FileRights) -> Result<Self, FileSystemError> {
        if !root.is_within(&self.root) || !self.rights.contains(rights) {
            return Err(FileSystemError::PermissionDenied);
        }
        Ok(Self {
            root,
            rights,
            logical_byte_limit: self.logical_byte_limit,
            operation_limit: self.operation_limit,
            handle_limit: self.handle_limit,
        })
    }

    pub(crate) fn handle_limit(&self) -> u32 {
        self.handle_limit
    }
}
