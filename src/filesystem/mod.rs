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

mod capability;
mod error;
mod handle;
mod limits;
mod path;
mod quota;
mod rom;
mod tree;

pub use capability::{FileCapability, FileRights};
pub use error::{FileSystemError, StoreHealth};
pub use handle::{FileHandle, HandleTable, OpenFile, OpenMode};
pub use limits::FileSystemLimits;
pub use path::VirtualPath;
pub use rom::{RomImage, RomImageError};
pub use tree::{ComputerFileSystem, FileSystemSnapshot, NodeKind, NodeMetadata};
