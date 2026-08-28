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

use runtime_bundler::smoke::require_ffi_abi;
use std::path::PathBuf;

#[test]
#[ignore = "requires a separately built current-platform cdylib"]
fn loads_the_exported_abi_from_the_dynamic_library() {
    let library = PathBuf::from(
        std::env::var_os("COMPUKTER_FFI_SMOKE_LIBRARY")
            .expect("COMPUKTER_FFI_SMOKE_LIBRARY must point at the built cdylib"),
    );

    require_ffi_abi(&library, 5).unwrap();
}
