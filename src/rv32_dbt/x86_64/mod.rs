/*
 * The Compukter Kraft Developers
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
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

pub(crate) mod cold_exit;
pub(crate) mod emitter;
pub(crate) mod lower;
#[cfg(feature = "dbt-tier1-prototype")]
pub(crate) mod region_alloc;
#[cfg(feature = "dbt-tier1-prototype")]
pub(crate) mod region_copy;
#[cfg(feature = "dbt-tier1-prototype")]
pub(crate) mod region_lower;
pub(crate) mod register_cache;
