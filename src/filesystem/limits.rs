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

/// Independent bounds applied before accepting guest-controlled paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileSystemLimits {
    pub maximum_path_bytes: usize,
    pub maximum_component_bytes: usize,
    pub maximum_components: usize,
}

impl FileSystemLimits {
    #[doc(hidden)]
    pub const fn testing() -> Self {
        Self {
            maximum_path_bytes: 256,
            maximum_component_bytes: 64,
            maximum_components: 16,
        }
    }
}

impl Default for FileSystemLimits {
    fn default() -> Self {
        Self {
            maximum_path_bytes: 4_096,
            maximum_component_bytes: 255,
            maximum_components: 64,
        }
    }
}
