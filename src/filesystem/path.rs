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

use std::fmt;

use super::{FileSystemError, FileSystemLimits};

/// An exact, absolute guest path represented as validated Unicode scalars.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VirtualPath(Box<[Box<str>]>);

impl VirtualPath {
    pub fn root() -> Self {
        Self(Box::new([]))
    }

    pub fn parse_utf16(units: &[u16], limits: &FileSystemLimits) -> Result<Self, FileSystemError> {
        let mut decoded = String::new();
        for scalar in char::decode_utf16(units.iter().copied()) {
            let scalar = scalar.map_err(|_| FileSystemError::InvalidPath)?;
            if scalar == '\0' {
                return Err(FileSystemError::InvalidPath);
            }
            decoded.push(scalar);
            if decoded.len() > limits.maximum_path_bytes {
                return Err(FileSystemError::InvalidPath);
            }
        }

        Self::parse_utf8(&decoded, limits)
    }

    pub fn parse_utf8(decoded: &str, limits: &FileSystemLimits) -> Result<Self, FileSystemError> {
        if decoded.len() > limits.maximum_path_bytes
            || !decoded.starts_with('/')
            || decoded.is_empty()
            || decoded.contains('\0')
        {
            return Err(FileSystemError::InvalidPath);
        }
        if decoded == "/" {
            return Ok(Self(Box::new([])));
        }
        if decoded.ends_with('/') {
            return Err(FileSystemError::InvalidPath);
        }

        let components = decoded[1..]
            .split('/')
            .map(|component| {
                if component.is_empty()
                    || component == "."
                    || component == ".."
                    || component.len() > limits.maximum_component_bytes
                {
                    return Err(FileSystemError::InvalidPath);
                }
                Ok(Box::<str>::from(component))
            })
            .collect::<Result<Box<[_]>, _>>()?;
        if components.len() > limits.maximum_components {
            return Err(FileSystemError::InvalidPath);
        }
        Ok(Self(components))
    }

    pub fn components(&self) -> impl ExactSizeIterator<Item = &str> {
        self.0.iter().map(Box::as_ref)
    }

    pub fn is_within(&self, root: &Self) -> bool {
        self.0.starts_with(&root.0)
    }

    pub fn parent(&self) -> Option<Self> {
        (!self.0.is_empty()).then(|| Self(self.0[..self.0.len() - 1].into()))
    }

    pub fn file_name(&self) -> Option<&str> {
        self.0.last().map(Box::as_ref)
    }

    pub(crate) fn component_slice(&self) -> &[Box<str>] {
        &self.0
    }
}

impl fmt::Display for VirtualPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("/")?;
        let mut components = self.components();
        if let Some(first) = components.next() {
            formatter.write_str(first)?;
            for component in components {
                formatter.write_str("/")?;
                formatter.write_str(component)?;
            }
        }
        Ok(())
    }
}
