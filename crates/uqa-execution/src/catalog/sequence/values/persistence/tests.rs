//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{cell::Cell, sync::Arc};
use uqa_storage::{
    KeyValueCatalog, KeyValueStorageBackend, KeyValueStore, MemoryKeyValueStore, RelationIdentity,
    SequenceOptions, SequenceReservationResult, SequenceRow,
};

fn session() -> PersistentStorageSession {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let catalog = Arc::new(KeyValueCatalog::new(store.clone()));
    catalog.save_schema("public").unwrap();
    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "ids"),
            security: uqa_storage::SequenceSecurityRow::bootstrap(),
            object_id: [1; 16],
            definition_generation: [2; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            owner: None,
            options: SequenceOptions {
                cache_size: 3,
                ..SequenceOptions::default()
            },
        })
        .unwrap();
    PersistentStorageSession::new(catalog, Arc::new(KeyValueStorageBackend::new(store)))
}

fn reserve(catalog: &dyn CatalogFacade) -> StorageBackendResult<i64> {
    let SequenceReservationResult::Reserved(value) =
        catalog.reserve_sequence_values("ids", [1; 16], [2; 16])?
    else {
        panic!("expected a sequence reservation");
    };
    Ok(value.first_value)
}

fn conflict() -> StorageBackendError {
    uqa_storage::mvcc::VersionError::WriteConflict {
        mutation: 0,
        expected: None,
        actual: Some(uqa_storage::mvcc::CommitSequence::from_u64(1)),
    }
    .into_storage_error()
}

#[test]
fn rejected_autonomous_reservation_undoes_only_its_failed_attempt_before_repeating() {
    let session = session();
    let calls = Cell::new(0);
    let value = autonomous_value(&session, &uqa_core::CancellationToken::new(), &|catalog| {
        calls.set(calls.get() + 1);
        assert!(!session.backend.in_transaction());
        if calls.get() == 1 {
            session.backend.begin_transaction()?;
            assert_eq!(reserve(catalog)?, 1);
            return Err(conflict());
        }
        reserve(catalog)
    })
    .unwrap();
    assert_eq!(value, 1);
    assert_eq!(calls.get(), 2);
    assert!(!session.backend.in_transaction());
    assert_eq!(reserve(session.catalog.as_ref()).unwrap(), 4);
}

#[test]
fn uncertain_autonomous_reservation_is_not_replayed_or_rolled_back() {
    for published in [false, true] {
        let session = session();
        let calls = Cell::new(0);
        let error = autonomous_value(&session, &uqa_core::CancellationToken::new(), &|catalog| {
            calls.set(calls.get() + 1);
            session.backend.begin_transaction()?;
            assert_eq!(reserve(catalog)?, 1);
            if published {
                session.backend.commit_transaction()?;
            }
            Err::<i64, _>(StorageBackendError::backend(
                "MVCC",
                uqa_storage::mvcc::CommitFailure::Indeterminate {
                    transaction: uqa_storage::mvcc::StorageTransactionId::new(
                        uqa_storage::mvcc::DatabaseId::from_bytes([3; 16]),
                        1,
                    )
                    .unwrap(),
                    source: StorageBackendError::Other("lost sequence commit reply".into()),
                },
            ))
        })
        .unwrap_err();
        assert!(error.commit_outcome().is_some());
        assert_eq!(calls.get(), 1);
        assert_eq!(session.backend.in_transaction(), !published);
        if !published {
            session.backend.rollback_transaction().unwrap();
        }
        assert_eq!(
            reserve(session.catalog.as_ref()).unwrap(),
            if published { 4 } else { 1 }
        );
    }
}

#[test]
fn cancellation_after_rejection_stops_before_allocating_again() {
    let session = session();
    let cancel = uqa_core::CancellationToken::new();
    let calls = Cell::new(0);
    let error = autonomous_value(&session, &cancel, &|catalog| {
        calls.set(calls.get() + 1);
        session.backend.begin_transaction()?;
        assert_eq!(reserve(catalog)?, 1);
        cancel.cancel();
        Err::<i64, _>(conflict())
    })
    .unwrap_err();
    assert!(matches!(error, StorageBackendError::Cancelled(_)));
    assert_eq!(calls.get(), 1);
    assert!(!session.backend.in_transaction());
    assert_eq!(reserve(session.catalog.as_ref()).unwrap(), 1);
}

#[test]
fn autonomous_values_reject_borrowed_transactions_and_do_not_parse_error_messages() {
    let session = session();
    session.backend.begin_transaction().unwrap();
    assert_eq!(reserve(session.catalog.as_ref()).unwrap(), 1);
    let error = autonomous_value(
        &session,
        &uqa_core::CancellationToken::new(),
        &|_| -> StorageBackendResult<i64> {
            panic!("must not evaluate inside a caller's transaction");
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("idle independent session"));
    assert!(session.backend.in_transaction());
    assert_eq!(session.catalog.load_sequence_rows().unwrap()[0].current, 3);
    session.backend.rollback_transaction().unwrap();
    let calls = Cell::new(0);
    let error = autonomous_value(&session, &uqa_core::CancellationToken::new(), &|_| {
        calls.set(calls.get() + 1);
        Err::<i64, _>(StorageBackendError::Other("record write conflict".into()))
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "record write conflict");
    assert_eq!(calls.get(), 1);
}

#[test]
fn sequence_storage_resource_failures_preserve_the_sql_error_category() {
    let error = sequence_storage_error(
        "reserve sequence values",
        StorageBackendError::Memory(uqa_core::memory::MemoryError::SizeOverflow),
    )
    .into_sql_error();
    assert_eq!(error.sqlstate(), Some("53200"));
    let error = sequence_storage_error(
        "reserve sequence values",
        StorageBackendError::Cancelled(uqa_core::QueryCancelled),
    )
    .into_sql_error();
    assert_eq!(error.sqlstate(), Some("57014"));
}
