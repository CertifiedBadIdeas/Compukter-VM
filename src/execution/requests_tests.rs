use super::{
    host::{
        HostMergeEntrySource, HostMergeSchema, HostValueSlot, HostValueType, OperationSchema,
        RequestId, TaskId,
    },
    requests::{
        reduce_last_write_wins, HostMergeEntry, HostMergeGroup, HostRequestIdentity,
        PendingHostRequest, PendingRequestTable, RequestTableError, RequestTableLimits,
    },
};

#[test]
fn operation_schema_declares_bounded_merge_entry_sources() {
    let pair = OperationSchema::asynchronous_last_write_wins(
        &[HostValueType::I32, HostValueType::I32],
        HostValueType::Unit,
        HostMergeGroup::new(7),
        HostMergeEntrySource::ArgumentPair { key: 0, value: 1 },
    );
    assert_eq!(
        HostMergeSchema::LastWriteWins {
            group: HostMergeGroup::new(7),
            source: HostMergeEntrySource::ArgumentPair { key: 0, value: 1 },
        },
        pair.merge,
    );

    let packed = OperationSchema::asynchronous_last_write_wins(
        &[HostValueType::I32],
        HostValueType::Unit,
        HostMergeGroup::new(7),
        HostMergeEntrySource::PackedFields {
            argument: 0,
            width: 5,
            count: 6,
        },
    );
    assert!(matches!(
        packed.merge,
        HostMergeSchema::LastWriteWins {
            source: HostMergeEntrySource::PackedFields {
                width: 5,
                count: 6,
                ..
            },
            ..
        }
    ));
}

fn identity(task: u32, request: u64) -> HostRequestIdentity {
    HostRequestIdentity::new(TaskId::new(task).unwrap(), RequestId::new(request).unwrap())
}

fn limits() -> RequestTableLimits {
    RequestTableLimits {
        maximum_requests: 3,
        maximum_arguments_per_request: 2,
        maximum_total_arguments: 4,
        maximum_utf16_per_request: 4,
        maximum_total_utf16: 6,
        maximum_merge_entries_per_request: 3,
        maximum_total_merge_entries: 5,
    }
}

fn ordinary(task: u32, request: u64) -> PendingHostRequest {
    PendingHostRequest::ordinary(
        identity(task, request),
        2,
        3,
        vec![HostValueSlot::I32(request as i32)].into_boxed_slice(),
        Box::default(),
    )
}

fn mergeable(task: u32, request: u64, entries: &[(u32, u32)]) -> PendingHostRequest {
    PendingHostRequest::last_write_wins(
        identity(task, request),
        2,
        3,
        HostMergeGroup::new(7),
        entries
            .iter()
            .map(|&(key, value)| HostMergeEntry::new(key, value))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        Box::default(),
        Box::default(),
    )
}

#[test]
fn request_table_distinguishes_tasks_and_completes_out_of_order() {
    let mut table = PendingRequestTable::new(limits());
    let first = identity(1, 1);
    let second = identity(2, 2);

    table.insert(ordinary(1, 1)).unwrap();
    table.insert(ordinary(2, 2)).unwrap();

    assert_eq!(second, table.take(second).unwrap().identity());
    assert_eq!(first, table.take(first).unwrap().identity());
    assert!(table.is_empty());
}

#[test]
fn request_table_rejects_duplicates_cancellation_and_late_completion() {
    let mut table = PendingRequestTable::new(limits());
    let first = identity(2, 1);
    let second = identity(2, 2);

    table.insert(ordinary(2, 1)).unwrap();
    assert_eq!(
        RequestTableError::DuplicateIdentity,
        table.insert(ordinary(2, 1)).unwrap_err()
    );
    table.insert(ordinary(2, 2)).unwrap();

    let cancelled = table.cancel_task(TaskId::new(2).unwrap());
    assert_eq!(vec![first, second], cancelled.as_ref());
    assert_eq!(
        RequestTableError::UnknownIdentity,
        table.take(first).unwrap_err()
    );
    assert_eq!(
        RequestTableError::UnknownIdentity,
        table.take(second).unwrap_err()
    );
}

#[test]
fn request_table_enforces_each_storage_bound() {
    let mut count = PendingRequestTable::new(RequestTableLimits {
        maximum_requests: 1,
        ..limits()
    });
    count.insert(ordinary(1, 1)).unwrap();
    assert_eq!(
        RequestTableError::RequestLimit,
        count.insert(ordinary(2, 2)).unwrap_err()
    );

    let too_many_arguments = PendingHostRequest::ordinary(
        identity(1, 1),
        2,
        3,
        vec![
            HostValueSlot::I32(1),
            HostValueSlot::I32(2),
            HostValueSlot::I32(3),
        ]
        .into_boxed_slice(),
        Box::default(),
    );
    assert_eq!(
        RequestTableError::ArgumentsPerRequestLimit,
        PendingRequestTable::new(limits())
            .insert(too_many_arguments)
            .unwrap_err(),
    );

    let mut aggregate_arguments = PendingRequestTable::new(RequestTableLimits {
        maximum_total_arguments: 1,
        ..limits()
    });
    aggregate_arguments.insert(ordinary(1, 1)).unwrap();
    assert_eq!(
        RequestTableError::TotalArgumentsLimit,
        aggregate_arguments.insert(ordinary(2, 2)).unwrap_err(),
    );

    let string_request = |task, request, units: usize| {
        PendingHostRequest::ordinary(
            identity(task, request),
            2,
            3,
            vec![HostValueSlot::String {
                start: 0,
                length: units as u32,
            }]
            .into_boxed_slice(),
            vec![0_u16; units].into_boxed_slice(),
        )
    };
    assert_eq!(
        RequestTableError::Utf16PerRequestLimit,
        PendingRequestTable::new(limits())
            .insert(string_request(1, 1, 5))
            .unwrap_err(),
    );
    let mut aggregate_utf16 = PendingRequestTable::new(RequestTableLimits {
        maximum_total_utf16: 3,
        ..limits()
    });
    aggregate_utf16.insert(string_request(1, 1, 2)).unwrap();
    assert_eq!(
        RequestTableError::TotalUtf16Limit,
        aggregate_utf16.insert(string_request(2, 2, 2)).unwrap_err(),
    );

    assert_eq!(
        RequestTableError::MergeEntriesPerRequestLimit,
        PendingRequestTable::new(limits())
            .insert(mergeable(1, 1, &[(0, 0), (1, 1), (2, 2), (3, 3)]))
            .unwrap_err(),
    );
    let mut aggregate_entries = PendingRequestTable::new(RequestTableLimits {
        maximum_total_merge_entries: 2,
        ..limits()
    });
    aggregate_entries
        .insert(mergeable(1, 1, &[(0, 0), (1, 1)]))
        .unwrap();
    assert_eq!(
        RequestTableError::TotalMergeEntriesLimit,
        aggregate_entries
            .insert(mergeable(2, 2, &[(2, 2)]))
            .unwrap_err(),
    );
}

#[test]
fn last_write_wins_retains_every_original_identity() {
    let requests = [
        mergeable(1, 1, &[(2, 7)]),
        mergeable(
            2,
            2,
            &[(0, 31), (1, 31), (2, 31), (3, 31), (4, 31), (5, 31)],
        ),
        mergeable(3, 3, &[(4, 0)]),
    ];

    let reduced = reduce_last_write_wins(&requests, 6).unwrap();

    assert_eq!(
        &[identity(1, 1), identity(2, 2), identity(3, 3)],
        reduced.requests(),
    );
    assert_eq!(
        &[
            HostMergeEntry::new(0, 31),
            HostMergeEntry::new(1, 31),
            HostMergeEntry::new(2, 31),
            HostMergeEntry::new(3, 31),
            HostMergeEntry::new(4, 0),
            HostMergeEntry::new(5, 31),
        ],
        reduced.entries(),
    );
}

#[test]
fn reducer_rejects_ordinary_mixed_groups_and_effective_entry_overflow() {
    let ordinary_request = ordinary(1, 1);
    assert_eq!(
        RequestTableError::NotMergeable,
        reduce_last_write_wins(&[ordinary_request], 1).unwrap_err(),
    );

    let first = mergeable(1, 1, &[(0, 1)]);
    let mut second = mergeable(2, 2, &[(1, 2)]);
    second = second.with_merge_group(HostMergeGroup::new(8));
    assert_eq!(
        RequestTableError::IncompatibleMergeGroup,
        reduce_last_write_wins(&[first, second], 2).unwrap_err(),
    );

    assert_eq!(
        RequestTableError::EffectiveMergeEntryLimit,
        reduce_last_write_wins(&[mergeable(1, 1, &[(0, 1), (1, 2)])], 1).unwrap_err(),
    );
}
