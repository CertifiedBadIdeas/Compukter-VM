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

/// Independent bounds applied before accepting guest-controlled paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileSystemLimits {
    pub maximum_path_bytes: usize,
    pub maximum_component_bytes: usize,
    pub maximum_components: usize,
    pub maximum_logical_bytes: u64,
    pub maximum_file_bytes: u64,
    pub maximum_nodes: u32,
    pub maximum_directory_entries: u32,
    pub maximum_open_handles: u32,
    pub maximum_io_bytes: usize,
    pub maximum_rom_bytes: usize,
    pub maximum_journal_record_bytes: usize,
    pub maximum_journal_payload_bytes: usize,
    pub maximum_checkpoint_bytes: usize,
    pub maximum_recovery_records: usize,
    pub maximum_recovery_bytes: usize,
    pub maximum_persistence_queue_records: usize,
    pub maximum_persistence_queue_bytes: usize,
}

impl FileSystemLimits {
    #[doc(hidden)]
    pub const fn testing() -> Self {
        Self {
            maximum_path_bytes: 256,
            maximum_component_bytes: 64,
            maximum_components: 16,
            maximum_logical_bytes: 1 << 20,
            maximum_file_bytes: 1 << 16,
            maximum_nodes: 1_024,
            maximum_directory_entries: 256,
            maximum_open_handles: 32,
            maximum_io_bytes: 4_096,
            maximum_rom_bytes: 1 << 20,
            maximum_journal_record_bytes: 1 << 16,
            maximum_journal_payload_bytes: 1 << 15,
            maximum_checkpoint_bytes: 1 << 20,
            maximum_recovery_records: 1_024,
            maximum_recovery_bytes: 4 << 20,
            maximum_persistence_queue_records: 64,
            maximum_persistence_queue_bytes: 1 << 20,
        }
    }
}

impl Default for FileSystemLimits {
    fn default() -> Self {
        Self {
            maximum_path_bytes: 4_096,
            maximum_component_bytes: 255,
            maximum_components: 64,
            maximum_logical_bytes: 64 << 20,
            maximum_file_bytes: 8 << 20,
            maximum_nodes: 65_536,
            maximum_directory_entries: 4_096,
            maximum_open_handles: 256,
            maximum_io_bytes: 64 << 10,
            maximum_rom_bytes: 16 << 20,
            maximum_journal_record_bytes: 1 << 20,
            maximum_journal_payload_bytes: 512 << 10,
            maximum_checkpoint_bytes: 128 << 20,
            maximum_recovery_records: 65_536,
            maximum_recovery_bytes: 512 << 20,
            maximum_persistence_queue_records: 4_096,
            maximum_persistence_queue_bytes: 64 << 20,
        }
    }
}
