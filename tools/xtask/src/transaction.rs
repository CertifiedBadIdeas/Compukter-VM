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

use std::fs;
use std::path::{Path, PathBuf};

pub struct FileTransaction {
    snapshots: Vec<(PathBuf, Vec<u8>)>,
    committed: bool,
}

impl FileTransaction {
    pub fn begin(root: &Path, paths: &[&Path]) -> Result<Self, String> {
        let snapshots = paths
            .iter()
            .map(|path| {
                let path = root.join(path);
                let bytes = fs::read(&path)
                    .map_err(|error| format!("cannot snapshot {}: {error}", path.display()))?;
                Ok((path, bytes))
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            snapshots,
            committed: false,
        })
    }

    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for FileTransaction {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for (path, bytes) in &self.snapshots {
            let _ = fs::write(path, bytes);
        }
    }
}
