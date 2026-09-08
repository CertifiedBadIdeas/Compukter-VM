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

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use compukter_vm::{FileSystemLimits, StoreOpenError, WorldFileSystemStore};

const LOCK_OWNER_ROOT: &str = "COMPUKTERS_TEST_LOCK_OWNER_ROOT";
const LOCK_OWNER_MODE: &str = "COMPUKTERS_TEST_LOCK_OWNER_MODE";
const LOCK_OWNER_READY: &str = "COMPUKTERS_TEST_LOCK_OWNER_READY";

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .join("compukters-vfs-lock-tests")
            .join(std::process::id().to_string());
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        std::fs::create_dir(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let parent_is_safe = self
            .0
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "compukters-vfs-lock-tests");
        assert!(parent_is_safe);
        if self.0.exists() {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

#[test]
fn lock_owner_process() {
    let Some(root) = std::env::var_os(LOCK_OWNER_ROOT) else {
        return;
    };
    let mode = std::env::var(LOCK_OWNER_MODE).unwrap();
    let store = WorldFileSystemStore::open(Path::new(&root), FileSystemLimits::testing()).unwrap();
    println!("{LOCK_OWNER_READY}");
    std::io::stdout().flush().unwrap();
    match mode.as_str() {
        "hold" => {
            std::io::stdin().read_exact(&mut [0_u8]).unwrap();
            store.close().unwrap();
        }
        "crash" => std::process::exit(0),
        _ => panic!("unknown lock owner mode: {mode}"),
    }
}

fn spawn_lock_owner(root: &Path, mode: &str) -> Child {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "lock_owner_process",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(LOCK_OWNER_ROOT, root)
        .env(LOCK_OWNER_MODE, mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.as_mut().unwrap());
    loop {
        let mut line = String::new();
        let read = output.read_line(&mut line).unwrap();
        assert_ne!(read, 0, "lock owner exited before acquiring the store");
        if line.contains(LOCK_OWNER_READY) {
            break;
        }
    }
    child
}

#[test]
fn process_lock_is_exclusive_and_recovers_after_exit() {
    let root = TestRoot::new();
    let lock = root.path().join("lock");

    std::fs::create_dir(&lock).unwrap();
    assert!(matches!(
        WorldFileSystemStore::open(root.path(), FileSystemLimits::testing()),
        Err(StoreOpenError::Io)
    ));
    std::fs::remove_dir(&lock).unwrap();

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.path().join("foreign-lock"), &lock).unwrap();
        assert!(matches!(
            WorldFileSystemStore::open(root.path(), FileSystemLimits::testing()),
            Err(StoreOpenError::Io)
        ));
        std::fs::remove_file(&lock).unwrap();
    }

    std::fs::write(&lock, b"legacy marker").unwrap();

    let store = WorldFileSystemStore::open(root.path(), FileSystemLimits::testing()).unwrap();
    assert!(matches!(
        WorldFileSystemStore::open(root.path(), FileSystemLimits::testing()),
        Err(StoreOpenError::Locked)
    ));
    store.close().unwrap();
    assert_eq!(std::fs::read(&lock).unwrap(), b"legacy marker");

    let mut child = spawn_lock_owner(root.path(), "hold");
    assert!(matches!(
        WorldFileSystemStore::open(root.path(), FileSystemLimits::testing()),
        Err(StoreOpenError::Locked)
    ));
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    assert!(child.wait().unwrap().success());
    WorldFileSystemStore::open(root.path(), FileSystemLimits::testing())
        .unwrap()
        .close()
        .unwrap();

    let mut child = spawn_lock_owner(root.path(), "crash");
    assert!(child.wait().unwrap().success());
    assert!(lock.is_file());
    WorldFileSystemStore::open(root.path(), FileSystemLimits::testing())
        .unwrap()
        .close()
        .unwrap();
}
