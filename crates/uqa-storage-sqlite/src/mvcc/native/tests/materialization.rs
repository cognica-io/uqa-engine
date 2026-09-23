//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use rusqlite::params;
use uqa_storage::{
    mvcc::{
        CommitFailure, CommitSequence, CommitStatus, PreparedRecordCommit, RecordWrite,
        VersionedKeyValueStore, VersionedPersistence, VersionedSessionOptions,
    },
    KeyValueStore,
};

use super::persistence::connection;
use super::*;
use crate::{mvcc::PhysicalResult, SQLiteRecordStore};

pub(super) fn with<T>(
    connection: &ManagedConnection,
    operation: impl FnOnce(&rusqlite::Connection) -> PhysicalResult<T>,
) -> T {
    connection
        .with_physical(|connection| Ok(operation(connection)))
        .unwrap()
        .unwrap()
}

pub(super) fn initialize(connection: &ManagedConnection) {
    Catalog::open(connection.clone()).unwrap();
    with(connection, |connection| {
        connection.execute_batch("INSERT INTO _documents(table_name, doc_id, body) VALUES ('public.docs', 1, '{\"n\":1}'), ('public.docs', 2, '{\"n\":2}');")?;
        Ok(())
    });
}

pub(super) fn records(
    connection: &ManagedConnection,
    store: &SQLiteRecordStore,
    family: NativeRecordFamily,
    control: &StorageReadControl,
) -> Vec<NativeRecord> {
    with(connection, |connection| {
        let mut records = Vec::new();
        physical::visit(connection, family.layout(), control, |values| {
            let owner = owners::for_row(
                connection,
                store.native_namespace().unwrap(),
                family,
                values,
                control,
            )?;
            records.push(NativeRecord::encode(family, owner, values, control)?);
            Ok(())
        })?;
        Ok(records)
    })
}

pub(super) fn replace(
    record: &NativeRecord,
    column: usize,
    value: ValueRef<'_>,
    control: &StorageReadControl,
) -> NativeRecord {
    let (identity, values) = decode_record(record.key(), record.row(), control).unwrap();
    let mut values = values.to_vec();
    values[column] = value;
    NativeRecord::encode(identity.family(), identity.owner(), &values, control).unwrap()
}

pub(super) fn delete(record: &NativeRecord) -> RecordWrite<'_> {
    RecordWrite {
        key: record.key(),
        expected: Some(CommitSequence::from_u64(1)),
        value: None,
    }
}

fn assert_materialized(
    connection: &ManagedConnection,
    store: &SQLiteRecordStore,
    record: &NativeRecord,
    control: &StorageReadControl,
) {
    let snapshot = store.snapshot(control).unwrap();
    let found = snapshot.get(record.key(), control).unwrap().unwrap();
    assert_eq!(&***found.value().unwrap(), record.row());
    with(connection, |connection| {
        let (identity, values) = decode_record(record.key(), record.row(), control)?;
        let key = physical::physical_key(identity.family().layout(), &values, control)?;
        assert_eq!(
            physical::get(connection, identity.family().layout(), &key, control)?.as_deref(),
            Some(record.row())
        );
        let pending: i64 = connection.query_row("SELECT (SELECT count(*) FROM _uqa_mvcc_native_changes) + (SELECT count(*) FROM _uqa_mvcc_native_expected)", [], |row| row.get(0))?;
        assert_eq!(pending, 0);
        Ok(())
    });
}

#[test]
fn independently_opened_native_writers_publish_both_rows_and_merge_cache_generations() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory.path().join(format!("writers-{mode}.db"));
        let connection_a = connection(&path, mode);
        initialize(&connection_a);
        let control = StorageReadControl::with_limit(1 << 24);
        let a = SQLiteRecordStore::for_native(&connection_a, &control).unwrap();
        let connection_b = connection(&path, mode);
        let b = SQLiteRecordStore::for_native(&connection_b, &control).unwrap();
        let original = records(&connection_a, &a, NativeRecordFamily::Documents, &control);
        let first = replace(&original[0], 2, ValueRef::Text(b"{\"n\":10}"), &control);
        let second = replace(&original[1], 2, ValueRef::Text(b"{\"n\":20}"), &control);
        let old = a.snapshot(&control).unwrap();
        let session_a = VersionedKeyValueStore::new(
            Arc::new(a.clone()),
            None,
            VersionedSessionOptions::default(),
        );
        let session_b = VersionedKeyValueStore::new(
            Arc::new(b.clone()),
            None,
            VersionedSessionOptions::default(),
        );
        session_a.begin_transaction().unwrap();
        session_a.put(first.key(), first.row()).unwrap();
        session_b.begin_transaction().unwrap();
        session_b.put(second.key(), second.row()).unwrap();
        session_b.commit_transaction().unwrap();
        assert!(session_a.in_transaction());
        assert_materialized(&connection_b, &b, &second, &control);
        assert_eq!(
            session_a.get(second.key()).unwrap().unwrap(),
            original[1].row()
        );
        session_a.commit_transaction().unwrap();
        assert_materialized(&connection_a, &a, &first, &control);
        assert_materialized(&connection_b, &b, &second, &control);
        for record in &original {
            assert_eq!(
                &***old
                    .get(record.key(), &control)
                    .unwrap()
                    .unwrap()
                    .value()
                    .unwrap(),
                record.row()
            );
        }
        let caches = records(
            &connection_a,
            &a,
            NativeRecordFamily::CacheRevisions,
            &control,
        );
        for cache in &caches {
            assert_materialized(&connection_a, &a, cache, &control);
        }
        session_a.begin_transaction().unwrap();
        session_a.put(first.key(), original[0].row()).unwrap();
        session_a.rollback_transaction().unwrap();
        assert_materialized(&connection_a, &a, &first, &control);
        let retry =
            PreparedRecordCommit::new(&[first.write(Some(CommitSequence::from_u64(1)))], &control)
                .unwrap();
        let id = a.allocate_transaction(&control).unwrap();
        assert!(matches!(
            a.commit(id, &retry, &control),
            Err(CommitFailure::Rejected(_))
        ));
        assert_materialized(&connection_b, &b, &second, &control);
    }
}

#[test]
fn rejected_history_write_rolls_back_native_rows_caches_and_receipt_before_retry() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory.path().join(format!("atomic-{mode}.db"));
        let connection = connection(&path, mode);
        initialize(&connection);
        let control = StorageReadControl::with_limit(1 << 24);
        let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        let original = records(&connection, &store, NativeRecordFamily::Documents, &control);
        let changed = replace(&original[0], 2, ValueRef::Text(b"{\"n\":99}"), &control);
        let before_cache = records(
            &connection,
            &store,
            NativeRecordFamily::CacheRevisions,
            &control,
        );
        let batch = PreparedRecordCommit::new(
            &[changed.write(Some(CommitSequence::from_u64(1)))],
            &control,
        )
        .unwrap();
        let id = store.allocate_transaction(&control).unwrap();
        with(&connection, |connection| {
            connection.execute_batch("CREATE TRIGGER reject_native_history BEFORE INSERT ON _uqa_mvcc_versions WHEN NEW.sequence > x'0000000000000001' BEGIN SELECT RAISE(ABORT, 'injected history failure'); END")?;
            Ok(())
        });
        assert!(matches!(
            store.commit(id, &batch, &control),
            Err(CommitFailure::Rejected(_))
        ));
        assert_eq!(
            store.commit_status(id, &control).unwrap(),
            CommitStatus::Pending
        );
        assert_eq!(
            store.snapshot(&control).unwrap().sequence(),
            CommitSequence::from_u64(1)
        );
        assert_materialized(&connection, &store, &original[0], &control);
        for cache in before_cache {
            assert_materialized(&connection, &store, &cache, &control);
        }
        with(&connection, |connection| {
            connection.execute_batch("DROP TRIGGER reject_native_history")?;
            Ok(())
        });
        let receipt = store.commit(id, &batch, &control).unwrap();
        let caches = records(
            &connection,
            &store,
            NativeRecordFamily::CacheRevisions,
            &control,
        );
        assert_eq!(store.commit(id, &batch, &control).unwrap(), receipt);
        for cache in caches {
            assert_materialized(&connection, &store, &cache, &control);
        }
        assert_materialized(&connection, &store, &changed, &control);
        let reopened = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        assert_eq!(
            reopened.commit_status(id, &control).unwrap(),
            CommitStatus::Committed(receipt)
        );
    }
}

#[test]
fn btree_cascades_must_be_in_the_evaluated_batch_and_named_blob_keys_survive() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    with(&connection, |connection| {
        connection.execute_batch("INSERT INTO _btree_indexes VALUES ('public.docs', 'named'); INSERT INTO _btree_index_entries VALUES ('public.docs', 'named', 1, '10');")?;
        connection.execute(
            "INSERT INTO _btree_indexes VALUES ('public.docs', ?1)",
            params![b"named".as_slice()],
        )?;
        connection.execute(
            "INSERT INTO _btree_index_entries VALUES ('public.docs', ?1, 1, '10')",
            params![b"named".as_slice()],
        )?;
        Ok(())
    });
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let documents = records(&connection, &store, NativeRecordFamily::Documents, &control);
    let entries = records(
        &connection,
        &store,
        NativeRecordFamily::BtreeIndexEntries,
        &control,
    );
    let marker = records(
        &connection,
        &store,
        NativeRecordFamily::BtreeIndexes,
        &control,
    );
    assert_eq!(entries.len(), 2);
    assert_ne!(entries[0].key(), entries[1].key());
    for (entry, expected) in entries
        .iter()
        .zip([ValueRef::Text(b"named"), ValueRef::Blob(b"named")])
    {
        let (_, values) = decode_record(entry.key(), entry.row(), &control).unwrap();
        assert_eq!(values[1], expected);
    }
    let partial = PreparedRecordCommit::new(&[delete(&documents[0])], &control).unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    assert!(matches!(
        store.commit(id, &partial, &control),
        Err(CommitFailure::Rejected(VersionError::InvalidEncoding(
            "native trigger or cascade changed an unprepared record"
        )))
    ));
    assert_materialized(&connection, &store, &documents[0], &control);
    for entry in &entries {
        assert_materialized(&connection, &store, entry, &control);
    }
    let mut writes = vec![delete(&documents[0])];
    writes.extend(entries.iter().map(delete));
    let complete = PreparedRecordCommit::new(&writes, &control).unwrap();
    store.commit(id, &complete, &control).unwrap();
    with(&connection, |connection| {
        assert_eq!(
            connection.query_row("SELECT count(*) FROM _btree_index_entries", [], |row| row
                .get::<_, i64>(
                0
            ))?,
            0
        );
        Ok(())
    });
    for marker in marker {
        assert_materialized(&connection, &store, &marker, &control);
    }
    for record in std::iter::once(&documents[0]).chain(entries.iter()) {
        assert!(store
            .snapshot(&control)
            .unwrap()
            .get(record.key(), &control)
            .unwrap()
            .unwrap()
            .value()
            .is_none());
    }
}

#[test]
fn graph_trigger_invalidation_is_atomic_with_the_evaluated_graph_batch() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    with(&connection, |connection| {
        connection.execute_batch("INSERT INTO _named_graphs VALUES ('g'); INSERT INTO _graph_vertices VALUES (1, 'node', '{}'); INSERT INTO _graph_membership VALUES ('vertex', 1, 'g'); INSERT INTO _graph_path_index_state VALUES ('g', 'g', '{}', 1);")?;
        Ok(())
    });
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let vertices = records(
        &connection,
        &store,
        NativeRecordFamily::GraphVertices,
        &control,
    );
    let paths = records(
        &connection,
        &store,
        NativeRecordFamily::GraphPathIndexState,
        &control,
    );
    let changed_vertex = replace(&vertices[0], 2, ValueRef::Text(b"{\"n\":1}"), &control);
    let changed_path = replace(&paths[0], 3, ValueRef::Integer(0), &control);
    let partial = PreparedRecordCommit::new(
        &[changed_vertex.write(Some(CommitSequence::from_u64(1)))],
        &control,
    )
    .unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    assert!(matches!(
        store.commit(id, &partial, &control),
        Err(CommitFailure::Rejected(VersionError::InvalidEncoding(
            "native trigger or cascade changed an unprepared record"
        )))
    ));
    assert_materialized(&connection, &store, &vertices[0], &control);
    assert_materialized(&connection, &store, &paths[0], &control);
    let old = store.snapshot(&control).unwrap();
    let complete = PreparedRecordCommit::new(
        &[
            changed_vertex.write(Some(old.sequence())),
            changed_path.write(Some(old.sequence())),
        ],
        &control,
    )
    .unwrap();
    store.commit(id, &complete, &control).unwrap();
    assert_materialized(&connection, &store, &changed_vertex, &control);
    assert_materialized(&connection, &store, &changed_path, &control);
    assert_eq!(
        &***old
            .get(paths[0].key(), &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        paths[0].row()
    );
}

#[test]
fn native_commits_reject_malformed_absent_tombstones_and_private_counter_writes() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let original = records(&connection, &store, NativeRecordFamily::Documents, &control);
    let mut trailing = original[0].key().to_vec();
    trailing.push(0);
    let id = store.allocate_transaction(&control).unwrap();
    for key in [
        b"unknown".as_slice(),
        &original[0].key()[..original[0].key().len() - 1],
        &trailing,
    ] {
        let prepared = PreparedRecordCommit::new(
            &[RecordWrite {
                key,
                expected: None,
                value: None,
            }],
            &control,
        )
        .unwrap();
        assert!(matches!(
            store.commit(id, &prepared, &control),
            Err(CommitFailure::Rejected(VersionError::InvalidEncoding(_)))
        ));
    }
    let foreign = NativeRecord::encode(
        NativeRecordFamily::Metadata,
        NativeRecordOwner::Database(DatabaseId::from_bytes([91; 16])),
        &[ValueRef::Text(b"outside"), ValueRef::Text(b"value")],
        &control,
    )
    .unwrap();
    let prepared = PreparedRecordCommit::new(&[foreign.write(None)], &control).unwrap();
    assert!(matches!(
        store.commit(id, &prepared, &control),
        Err(CommitFailure::Rejected(VersionError::WrongDatabase))
    ));
    let caches = records(
        &connection,
        &store,
        NativeRecordFamily::CacheRevisions,
        &control,
    );
    let prepared = PreparedRecordCommit::new(
        &[caches[0].write(Some(CommitSequence::from_u64(1)))],
        &control,
    )
    .unwrap();
    assert!(matches!(
        store.commit(id, &prepared, &control),
        Err(CommitFailure::Rejected(VersionError::InvalidEncoding(
            "native cache generations are provider-owned commit effects"
        )))
    ));
    let metadata = records(&connection, &store, NativeRecordFamily::Metadata, &control);
    let schema = metadata
        .iter()
        .find(|record| {
            decode_record(record.key(), record.row(), &control)
                .unwrap()
                .1[0]
                == ValueRef::Text(b"schema_version")
        })
        .unwrap();
    let prepared = PreparedRecordCommit::new(&[delete(schema)], &control).unwrap();
    assert!(store.commit(id, &prepared, &control).is_err());
    assert_eq!(
        store.snapshot(&control).unwrap().sequence(),
        CommitSequence::from_u64(1)
    );
    assert_materialized(&connection, &store, &original[0], &control);
}
