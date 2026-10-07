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

//! Internal envelope; publication and host-resource admission are separate gates.

use crate::execution::checkpoint::{Checkpoint, CheckpointError, Reader, Result, Writer};
use crate::filesystem::ComputerId;
use sha2::{Digest, Sha256};

const MAGIC: [u8; 8] = *b"CPKTHIB\0";
const FORMAT: u32 = 2;
const HEADER_BYTES: usize = 84;
const CHECKSUM_BYTES: usize = 32;
// Bump the format whenever any constituent logical codec changes its meaning.
const SCHEMA: &[u8] = b"compukters.computer.logical-checkpoint/2";

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub execution_bytes: usize,
    pub host_bytes: usize,
    pub allocation_bytes: usize,
}

impl Limits {
    pub(super) fn total_bytes(self) -> Result<usize> {
        HEADER_BYTES
            .checked_add(self.execution_bytes)
            .and_then(|value| value.checked_add(self.host_bytes))
            .and_then(|value| value.checked_add(CHECKSUM_BYTES))
            .ok_or(CheckpointError::Limit)
    }
}

pub(super) struct Envelope<'a> {
    pub generation: u64,
    pub execution: &'a [u8],
    pub host: &'a [u8],
}

fn runtime_identity() -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(SCHEMA);
    hash.update([0]);
    hash.update(env!("CARGO_PKG_VERSION").as_bytes());
    hash.finalize().into()
}

pub(super) fn encode(
    id: ComputerId,
    generation: u64,
    execution: &[u8],
    host: &[u8],
    limits: Limits,
) -> Result<Vec<u8>> {
    if execution.len() > limits.execution_bytes || host.len() > limits.host_bytes {
        return Err(CheckpointError::Limit);
    }
    let mut writer = Writer::new(limits.total_bytes()?);
    MAGIC.write(&mut writer)?;
    FORMAT.write(&mut writer)?;
    runtime_identity().write(&mut writer)?;
    writer.put(&id.into_bytes())?;
    generation.write(&mut writer)?;
    execution.len().write(&mut writer)?;
    host.len().write(&mut writer)?;
    writer.put(execution)?;
    writer.put(host)?;
    let mut bytes = writer.finish();
    let digest = Sha256::digest(&bytes);
    bytes
        .try_reserve_exact(CHECKSUM_BYTES)
        .map_err(|_| CheckpointError::Allocation)?;
    bytes.extend_from_slice(&digest);
    Ok(bytes)
}

pub(super) fn decode(bytes: &[u8], id: ComputerId, limits: Limits) -> Result<Envelope<'_>> {
    if bytes.len() > limits.total_bytes()? {
        return Err(CheckpointError::Limit);
    }
    let body_length = bytes
        .len()
        .checked_sub(CHECKSUM_BYTES)
        .filter(|length| *length >= HEADER_BYTES)
        .ok_or(CheckpointError::Truncated)?;
    let (body, checksum) = bytes.split_at(body_length);
    if Sha256::digest(body).as_slice() != checksum {
        return Err(CheckpointError::Integrity);
    }
    let mut reader = Reader::new(body, limits.total_bytes()?, HEADER_BYTES)?;
    let magic: [u8; 8] = Checkpoint::read(&mut reader)?;
    let format = u32::read(&mut reader)?;
    let runtime: [u8; 32] = Checkpoint::read(&mut reader)?;
    let computer: [u8; 16] = Checkpoint::read(&mut reader)?;
    if magic != MAGIC
        || format != FORMAT
        || runtime != runtime_identity()
        || computer != id.into_bytes()
    {
        return Err(CheckpointError::Incompatible);
    }
    let generation = u64::read(&mut reader)?;
    let execution_length = usize::read(&mut reader)?;
    let host_length = usize::read(&mut reader)?;
    if execution_length > limits.execution_bytes || host_length > limits.host_bytes {
        return Err(CheckpointError::Limit);
    }
    let execution = reader.take(execution_length)?;
    let host = reader.take(host_length)?;
    reader.finish()?;
    Ok(Envelope {
        generation,
        execution,
        host,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: ComputerId = ComputerId::from_bytes([7; 16]);
    const LIMITS: Limits = Limits {
        execution_bytes: 32,
        host_bytes: 16,
        allocation_bytes: 64,
    };

    #[test]
    fn checkpoint_envelope_preserves_generation_execution_and_host_bytes() {
        let bytes = encode(ID, 17, b"execution", b"timers", LIMITS).unwrap();
        let decoded = decode(&bytes, ID, LIMITS).unwrap();
        assert_eq!(17, decoded.generation);
        assert_eq!(b"execution", decoded.execution);
        assert_eq!(b"timers", decoded.host);
    }

    #[test]
    fn checkpoint_envelope_rejects_corruption_truncation_identity_and_sizes() {
        let bytes = encode(ID, 17, b"execution", b"timers", LIMITS).unwrap();
        for index in 0..bytes.len() {
            let mut corrupt = bytes.clone();
            corrupt[index] ^= 1;
            assert!(matches!(
                decode(&corrupt, ID, LIMITS),
                Err(CheckpointError::Integrity)
            ));
        }
        for length in 0..bytes.len() {
            assert!(decode(&bytes[..length], ID, LIMITS).is_err());
        }
        assert!(matches!(
            decode(&bytes, ComputerId::from_bytes([8; 16]), LIMITS),
            Err(CheckpointError::Incompatible)
        ));
        let mut incompatible = bytes.clone();
        incompatible[8] = FORMAT as u8 + 1;
        let end = incompatible.len() - CHECKSUM_BYTES;
        let digest = Sha256::digest(&incompatible[..end]);
        incompatible[end..].copy_from_slice(&digest);
        assert!(matches!(
            decode(&incompatible, ID, LIMITS),
            Err(CheckpointError::Incompatible)
        ));
        assert_eq!(
            Err(CheckpointError::Limit),
            encode(ID, 0, &[0; 33], b"", LIMITS)
        );
        assert!(matches!(
            decode(
                &bytes,
                ID,
                Limits {
                    execution_bytes: 8,
                    ..LIMITS
                }
            ),
            Err(CheckpointError::Limit)
        ));
        let mut oversized = bytes.clone();
        oversized[68..76].copy_from_slice(&u64::MAX.to_le_bytes());
        let end = oversized.len() - CHECKSUM_BYTES;
        let digest = Sha256::digest(&oversized[..end]);
        oversized[end..].copy_from_slice(&digest);
        assert!(matches!(
            decode(&oversized, ID, LIMITS),
            Err(CheckpointError::Limit)
        ));
    }
}
