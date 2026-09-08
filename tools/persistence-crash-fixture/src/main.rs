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

use std::path::PathBuf;

use persistence_crash_fixture::{parse_mutation_point, run_mutation};

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let root = PathBuf::from(arguments.next().expect("missing store root"));
    let point = arguments.next().expect("missing crash point");
    assert!(arguments.next().is_none(), "unexpected argument");
    let point = point
        .to_str()
        .and_then(parse_mutation_point)
        .expect("invalid crash point");
    run_mutation(&root, point);
}
