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

use std::sync::Arc;

use compukter_vm::{
    recover, Checkpoint, CheckpointNode, ComputerId, FileSystemLimits, JournalOperation,
    JournalRecord, PersistenceCodecError, RecoveryCheckpoint, RecoveryError, RecoveryInput,
    RecoveryJournalRecord, VirtualPath,
};
use sha2::{Digest, Sha256};

fn id(byte: u8) -> ComputerId {
    ComputerId::from_bytes([byte; 16])
}

fn path(value: &str) -> VirtualPath {
    VirtualPath::parse_utf8(value, &FileSystemLimits::testing()).unwrap()
}

fn checkpoint(computer: ComputerId, generation: u64) -> Checkpoint {
    Checkpoint::new(
        computer,
        generation,
        vec![CheckpointNode::directory(path("/home/project"), generation)],
    )
    .unwrap()
}

fn record(
    computer: ComputerId,
    sequence: u64,
    previous: u64,
    operation: JournalOperation,
) -> JournalRecord {
    JournalRecord::new(computer, sequence, previous, operation).unwrap()
}

#[test]
fn journal_and_checkpoint_codecs_are_exact_and_deterministic() {
    let limits = FileSystemLimits::testing();
    let computer = id(7);
    let object: [u8; 32] = Sha256::digest(b"artifact").into();
    let journal = record(
        computer,
        5,
        4,
        JournalOperation::put_file(path("/home/project/app"), 5, 8, object, true),
    );
    let encoded = journal.encode(&limits).unwrap();
    assert_eq!(
        JournalRecord::decode(encoded.clone(), &limits).unwrap(),
        journal
    );
    assert_eq!(journal.encode(&limits).unwrap(), encoded);

    let operations = [
        record(
            computer,
            1,
            0,
            JournalOperation::create_directory(path("/home/new"), 1),
        ),
        record(
            computer,
            2,
            1,
            JournalOperation::put_file(path("/home/new/app"), 2, 8, object, false),
        ),
        record(
            computer,
            3,
            2,
            JournalOperation::remove(path("/home/new/app")),
        ),
        record(
            computer,
            4,
            3,
            JournalOperation::rename(path("/home/new"), path("/home/renamed"), true),
        ),
    ];
    for operation in operations {
        let encoded = operation.encode(&limits).unwrap();
        assert_eq!(JournalRecord::decode(encoded, &limits).unwrap(), operation);
    }

    let checkpoint = Checkpoint::new(
        computer,
        4,
        vec![
            CheckpointNode::directory(path("/home/project"), 2),
            CheckpointNode::file(path("/home/project/old"), 4, 3, [3; 32], false),
        ],
    )
    .unwrap();
    let encoded = checkpoint.encode(&limits).unwrap();
    assert_eq!(
        Checkpoint::decode(encoded.clone(), &limits).unwrap(),
        checkpoint
    );
    assert_eq!(checkpoint.encode(&limits).unwrap(), encoded);

    let mut trailing = encoded.to_vec();
    let digest_at = trailing.len() - 32;
    trailing.insert(digest_at, 0);
    let digest_at = trailing.len() - 32;
    let digest = Sha256::digest(&trailing[..digest_at]);
    trailing[digest_at..].copy_from_slice(&digest);
    assert_eq!(
        Checkpoint::decode(trailing.into(), &limits),
        Err(PersistenceCodecError::Malformed),
    );

    let mut tiny = limits;
    tiny.maximum_journal_payload_bytes = 4;
    assert_eq!(
        journal.encode(&tiny),
        Err(PersistenceCodecError::LimitExceeded),
    );
}

#[test]
fn torn_unconfirmed_tail_is_discarded_but_confirmed_corruption_faults() {
    let limits = FileSystemLimits::testing();
    let computer = id(1);
    let checkpoint_bytes = checkpoint(computer, 4).encode(&limits).unwrap();
    let fifth = record(
        computer,
        5,
        4,
        JournalOperation::put_file(path("/home/project/app"), 5, 3, [9; 32], true),
    )
    .encode(&limits)
    .unwrap();
    let sixth = record(
        computer,
        6,
        5,
        JournalOperation::remove(path("/home/project/app")),
    )
    .encode(&limits)
    .unwrap();
    let torn: Arc<[u8]> = sixth[..sixth.len() - 7].into();
    let input = RecoveryInput::new(computer, 5)
        .with_checkpoint(RecoveryCheckpoint::new(4, checkpoint_bytes.clone()))
        .with_journal(RecoveryJournalRecord::new(5, fifth))
        .with_journal(RecoveryJournalRecord::new(6, torn));

    let recovered = recover(&input, &limits).unwrap();
    assert_eq!(recovered.generation(), 5);
    assert!(recovered
        .node(&path("/home/project/app"))
        .unwrap()
        .executable());

    let unconfirmed_valid = RecoveryInput::new(computer, 4)
        .with_checkpoint(RecoveryCheckpoint::new(4, checkpoint_bytes.clone()))
        .with_journal(RecoveryJournalRecord::new(
            5,
            record(
                computer,
                5,
                4,
                JournalOperation::put_file(path("/home/project/app"), 5, 3, [9; 32], true),
            )
            .encode(&limits)
            .unwrap(),
        ));
    assert_eq!(
        recover(&unconfirmed_valid, &limits).unwrap().generation(),
        5
    );

    let mut corrupt = checkpoint_bytes.to_vec();
    corrupt[20] ^= 1;
    let corrupt =
        RecoveryInput::new(computer, 4).with_checkpoint(RecoveryCheckpoint::new(4, corrupt.into()));
    assert_eq!(
        recover(&corrupt, &limits),
        Err(RecoveryError::ConfirmedCorruption),
    );

    let foreign_checkpoint = RecoveryInput::new(computer, 4).with_checkpoint(
        RecoveryCheckpoint::new(4, checkpoint(id(8), 4).encode(&limits).unwrap()),
    );
    assert_eq!(
        recover(&foreign_checkpoint, &limits),
        Err(RecoveryError::ConfirmedCorruption),
    );
}

#[test]
fn confirmed_gaps_identity_mismatches_and_corruption_never_get_guessed_past() {
    let limits = FileSystemLimits::testing();
    let computer = id(2);
    let checkpoint = checkpoint(computer, 4).encode(&limits).unwrap();
    let sixth = record(
        computer,
        6,
        5,
        JournalOperation::remove(path("/home/project")),
    )
    .encode(&limits)
    .unwrap();
    let gap = RecoveryInput::new(computer, 6)
        .with_checkpoint(RecoveryCheckpoint::new(4, checkpoint.clone()))
        .with_journal(RecoveryJournalRecord::new(6, sixth));
    assert_eq!(
        recover(&gap, &limits),
        Err(RecoveryError::ConfirmedCorruption)
    );

    let foreign = record(id(3), 5, 4, JournalOperation::remove(path("/home/project")))
        .encode(&limits)
        .unwrap();
    let mismatch = RecoveryInput::new(computer, 5)
        .with_checkpoint(RecoveryCheckpoint::new(4, checkpoint.clone()))
        .with_journal(RecoveryJournalRecord::new(5, foreign));
    assert_eq!(
        recover(&mismatch, &limits),
        Err(RecoveryError::ConfirmedCorruption),
    );

    let valid = record(
        computer,
        5,
        4,
        JournalOperation::remove(path("/home/project")),
    )
    .encode(&limits)
    .unwrap();
    let mut corrupt = valid.to_vec();
    corrupt[18] ^= 1;
    let corrupt = RecoveryInput::new(computer, 5)
        .with_checkpoint(RecoveryCheckpoint::new(4, checkpoint))
        .with_journal(RecoveryJournalRecord::new(5, corrupt.into()));
    assert_eq!(
        recover(&corrupt, &limits),
        Err(RecoveryError::ConfirmedCorruption),
    );
}

#[test]
fn recovery_work_is_bounded_before_decoding_untrusted_records() {
    let computer = id(4);
    let normal = FileSystemLimits::testing();
    let checkpoint = checkpoint(computer, 1).encode(&normal).unwrap();
    let input =
        RecoveryInput::new(computer, 1).with_checkpoint(RecoveryCheckpoint::new(1, checkpoint));

    let mut limits = normal;
    limits.maximum_recovery_bytes = 8;
    assert_eq!(recover(&input, &limits), Err(RecoveryError::LimitExceeded));

    let mut limits = normal;
    limits.maximum_recovery_records = 0;
    assert_eq!(recover(&input, &limits), Err(RecoveryError::LimitExceeded));
}
