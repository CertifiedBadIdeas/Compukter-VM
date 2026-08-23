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

/// Stable result categories exposed by the guest filesystem boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSystemError {
    InvalidPath,
    NotFound,
    AlreadyExists,
    NotDirectory,
    IsDirectory,
    NotEmpty,
    ReadOnly,
    PermissionDenied,
    StaleHandle,
    QuotaExceeded,
    Busy,
    StorageFaulted,
    Closed,
}

/// Observable lifecycle of the persistent store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreHealth {
    Active,
    Draining,
    Faulted,
    Closed,
}
