//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph selectors retain the same boundaries as their source records without loading entity properties.

mod migration;
mod path_migration;
mod reads;

use std::sync::Arc;

use rusqlite::params;
use uqa_storage::{
    mvcc::{
        CommitFailure, CommitSequence, CommitStatus, CommittedRecordSnapshot, PreparedRecordCommit,
        VersionedKeyValueStore, VersionedPersistence, VersionedSessionOptions,
    },
    KeyValueStore,
};

use super::{
    materialization::{delete, initialize, records, replace, with},
    persistence::connection,
    *,
};
use crate::SQLiteRecordStore;

type Family = NativeRecordFamily;

fn seed(connection: &ManagedConnection) {
    initialize(connection);
    with(connection, |connection| {
        connection.execute_batch("INSERT INTO _named_graphs VALUES ('g'); INSERT INTO _graph_vertices VALUES (1, 'node', '{}'), (2, 'node', '{}'); INSERT INTO _graph_edges VALUES (9, 1, 2, 'rel', '{}'), (10, 1, 2, 'rel', '{}'); INSERT INTO _graph_membership VALUES ('vertex', 1, 'g'), ('edge', 9, 'g');")?;
        Ok(())
    });
}

fn record(
    store: &SQLiteRecordStore,
    family: Family,
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> NativeRecord {
    NativeRecord::encode(
        family,
        NativeRecordOwner::Database(store.database_id()),
        values,
        control,
    )
    .unwrap()
}

fn lookup(
    store: &SQLiteRecordStore,
    parts: (&str, &str, i64, &str, i64),
    control: &StorageReadControl,
) -> NativeRecord {
    record(
        store,
        Family::GraphLookups,
        &[
            ValueRef::Text(parts.0.as_bytes()),
            ValueRef::Text(parts.1.as_bytes()),
            ValueRef::Integer(parts.2),
            ValueRef::Text(parts.3.as_bytes()),
            ValueRef::Integer(parts.4),
        ],
        control,
    )
}

fn assert_live(
    snapshot: &dyn CommittedRecordSnapshot,
    row: &NativeRecord,
    live: bool,
    control: &StorageReadControl,
) {
    let found = snapshot.get(row.key(), control).unwrap();
    assert_eq!(
        found
            .as_ref()
            .and_then(|found| found.value())
            .map(|value| &***value),
        live.then_some(row.row())
    );
}

#[test]
fn native_graph_lookup_baseline_is_complete_and_selective_without_entity_payloads() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory.path().join(format!("graph-lookup-{mode}.db"));
        let connection = connection(&path, mode);
        seed(&connection);
        with(&connection, |connection| {
            connection.execute(
                "UPDATE _graph_vertices SET properties_json = ?1",
                ["x".repeat(1 << 20)],
            )?;
            connection.execute(
                "INSERT INTO _graph_membership VALUES ('extension', -9, ?1)",
                ["g\0日本語"],
            )?;
            connection.execute(
                "UPDATE _graph_edges SET label = ?1 WHERE edge_id = 10",
                ["r\0él"],
            )?;
            Ok(())
        });
        let control = StorageReadControl::with_limit(1 << 25);
        let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        let expected = [
            ("label", "node", 0, "vertex", 1),
            ("label", "node", 0, "vertex", 2),
            ("label", "rel", 0, "edge", 9),
            ("label", "r\0él", 0, "edge", 10),
            ("source", "", 1, "edge", 9),
            ("source", "", 1, "edge", 10),
            ("target", "", 2, "edge", 9),
            ("target", "", 2, "edge", 10),
            ("member", "g", 0, "vertex", 1),
            ("member", "g", 0, "edge", 9),
            ("member", "g\0日本語", 0, "extension", -9),
        ];
        let physical = records(&connection, &store, Family::GraphLookups, &control);
        let snapshot = store.snapshot(&control).unwrap();
        assert_eq!(physical.len(), expected.len());
        for parts in expected {
            let row = lookup(&store, parts, &control);
            assert!(physical
                .iter()
                .any(|found| found.key() == row.key() && found.row() == row.row()));
            assert_live(snapshot.as_ref(), &row, true, &control);
        }
        let small = StorageReadControl::with_limit(16 << 10);
        let prefix = NativeRecordIdentity::new(
            Family::GraphLookups,
            NativeRecordOwner::Database(store.database_id()),
        )
        .unwrap()
        .encode_prefix(
            &[
                ValueRef::Text(b"label"),
                ValueRef::Text(b"node"),
                ValueRef::Integer(0),
                ValueRef::Text(b"vertex"),
            ],
            &small,
        )
        .unwrap();
        let page = snapshot.scan(&prefix, None, 1, &small).unwrap();
        assert_eq!(page.len(), 1);
        let mut ids = Vec::new();
        snapshot
            .visit_prefix(&prefix, None, 8, &small, &mut |key, found| {
                let (_, values) = decode_record(key, found.value.unwrap(), &small)?;
                ids.push(values[4].as_i64().unwrap());
                Ok(true)
            })
            .unwrap();
        assert_eq!(ids, [1, 2]);
        drop(page);
        drop(prefix);
        assert_eq!(small.memory().used(), 0);
        let reopened = SQLiteRecordStore::for_native(&connection, &small).unwrap();
        assert_eq!(reopened.database_id(), store.database_id());
    }
}

#[test]
fn native_graph_lookup_changes_must_include_both_source_and_all_selectors() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    seed(&connection);
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let vertices = records(&connection, &store, Family::GraphVertices, &control);
    let changed = replace(&vertices[0], 1, ValueRef::Text(b"renamed"), &control);
    let old = lookup(&store, ("label", "node", 0, "vertex", 1), &control);
    let new = lookup(&store, ("label", "renamed", 0, "vertex", 1), &control);
    let boundary = CommitSequence::from_u64(1);
    let id = store.allocate_transaction(&control).unwrap();
    for writes in [
        vec![changed.write(Some(boundary))],
        vec![changed.write(Some(boundary)), new.write(None)],
        vec![changed.write(Some(boundary)), delete(&old)],
        vec![delete(&old)],
        vec![new.write(None)],
    ] {
        let batch = PreparedRecordCommit::new(&writes, &control).unwrap();
        assert!(matches!(
            store.commit(id, &batch, &control),
            Err(CommitFailure::Rejected(_))
        ));
        assert_eq!(
            store.commit_status(id, &control).unwrap(),
            CommitStatus::Pending
        );
        let snapshot = store.snapshot(&control).unwrap();
        assert_eq!(snapshot.sequence(), boundary);
        assert_live(snapshot.as_ref(), &vertices[0], true, &control);
        assert_live(snapshot.as_ref(), &old, true, &control);
        assert_live(snapshot.as_ref(), &new, false, &control);
    }
    let before = store.snapshot(&control).unwrap();
    let complete = PreparedRecordCommit::new(
        &[changed.write(Some(boundary)), delete(&old), new.write(None)],
        &control,
    )
    .unwrap();
    store.commit(id, &complete, &control).unwrap();
    assert_live(before.as_ref(), &old, true, &control);
    assert_live(before.as_ref(), &new, false, &control);
    let after = store.snapshot(&control).unwrap();
    assert_live(after.as_ref(), &old, false, &control);
    assert_live(after.as_ref(), &new, true, &control);
}

#[test]
fn native_graph_lookup_independent_adjacency_writers_commit_before_private_writer_finishes() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        for rollback in [false, true] {
            let path = directory
                .path()
                .join(format!("adjacency-{mode}-{rollback}.db"));
            let connection_a = connection(&path, mode);
            seed(&connection_a);
            let control = StorageReadControl::with_limit(1 << 24);
            let a = SQLiteRecordStore::for_native(&connection_a, &control).unwrap();
            let connection_b = connection(&path, mode);
            let b = SQLiteRecordStore::for_native(&connection_b, &control).unwrap();
            let edges = records(&connection_a, &a, Family::GraphEdges, &control);
            let before = a.snapshot(&control).unwrap();
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
            let changed = [
                replace(&edges[0], 1, ValueRef::Integer(2), &control),
                replace(&edges[1], 1, ValueRef::Integer(2), &control),
            ];
            for (slot, session) in [&session_a, &session_b].into_iter().enumerate() {
                let edge_id = 9 + slot as i64;
                let old = lookup(&a, ("source", "", 1, "edge", edge_id), &control);
                let new = lookup(&a, ("source", "", 2, "edge", edge_id), &control);
                session.begin_transaction().unwrap();
                session
                    .put(changed[slot].key(), changed[slot].row())
                    .unwrap();
                session.delete(old.key()).unwrap();
                session.put(new.key(), new.row()).unwrap();
            }
            session_b.commit_transaction().unwrap();
            assert!(session_a.in_transaction());
            assert_live(
                b.snapshot(&control).unwrap().as_ref(),
                &changed[1],
                true,
                &control,
            );
            if rollback {
                session_a.rollback_transaction().unwrap();
            } else {
                session_a.commit_transaction().unwrap();
            }
            let reopened = SQLiteRecordStore::for_native(&connection_b, &control).unwrap();
            let after = reopened.snapshot(&control).unwrap();
            for (slot, original) in edges.iter().enumerate() {
                let committed = slot == 1 || !rollback;
                assert_live(before.as_ref(), original, true, &control);
                assert_live(
                    after.as_ref(),
                    if committed { &changed[slot] } else { original },
                    true,
                    &control,
                );
                for source in [1, 2] {
                    let key = lookup(
                        &a,
                        ("source", "", source, "edge", 9 + slot as i64),
                        &control,
                    );
                    assert_live(before.as_ref(), &key, source == 1, &control);
                    assert_live(
                        after.as_ref(),
                        &key,
                        if committed { source == 2 } else { source == 1 },
                        &control,
                    );
                }
            }
        }
    }
}

#[test]
fn native_graph_lookup_failure_rolls_back_materialization_and_retries_exact_batch() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    seed(&connection);
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let members = records(&connection, &store, Family::GraphMembership, &control);
    let old = lookup(&store, ("member", "g", 0, "edge", 9), &control);
    let member = members
        .iter()
        .find(|member| {
            decode_record(member.key(), member.row(), &control)
                .unwrap()
                .1[0]
                == ValueRef::Text(b"edge")
        })
        .unwrap();
    let batch = PreparedRecordCommit::new(&[delete(member), delete(&old)], &control).unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    with(&connection, |connection| {
        connection.execute_batch("CREATE TRIGGER reject_graph_history BEFORE INSERT ON _uqa_mvcc_versions WHEN NEW.sequence > x'0000000000000001' BEGIN SELECT RAISE(ABORT, 'injected history failure'); END")?;
        Ok(())
    });
    assert!(matches!(
        store.commit(id, &batch, &control),
        Err(CommitFailure::Rejected(_))
    ));
    assert_live(
        store.snapshot(&control).unwrap().as_ref(),
        &old,
        true,
        &control,
    );
    with(&connection, |connection| {
        let count: i64 = connection.query_row("SELECT count(*) FROM _uqa_mvcc_native_graph_lookup WHERE kind = 'member' AND entity_type = 'edge'", [], |row| row.get(0))?;
        assert_eq!(count, 1);
        connection.execute_batch("DROP TRIGGER reject_graph_history")?;
        Ok(())
    });
    let receipt = store.commit(id, &batch, &control).unwrap();
    assert_eq!(store.commit(id, &batch, &control).unwrap(), receipt);
    assert_live(
        store.snapshot(&control).unwrap().as_ref(),
        &old,
        false,
        &control,
    );
    assert_eq!(
        SQLiteRecordStore::for_native(&connection, &control)
            .unwrap()
            .commit_status(id, &control)
            .unwrap(),
        CommitStatus::Committed(receipt)
    );
}

#[test]
fn native_graph_lookup_guards_are_required_and_reject_direct_writes() {
    for change in [
        "DROP TRIGGER _uqa_mvcc_graph_lookup_18_0_UPDATE",
        "DROP TRIGGER _uqa_mvcc_native_graph_lookup_INSERT_guard",
        "DROP TRIGGER _uqa_mvcc_native_capture_45_DELETE",
        "DROP TRIGGER _uqa_mvcc_graph_lookup_14_1_INSERT; CREATE TRIGGER _uqa_mvcc_graph_lookup_14_1_INSERT AFTER INSERT ON _graph_edges BEGIN SELECT 1; END",
    ] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        seed(&connection);
        let control = StorageReadControl::with_limit(1 << 24);
        SQLiteRecordStore::for_native(&connection, &control).unwrap();
        with(&connection, |connection| {
            assert!(connection.execute("DELETE FROM _uqa_mvcc_native_graph_lookup", []).is_err());
            connection.execute_batch(change)?;
            Ok(())
        });
        assert!(SQLiteRecordStore::for_native(&connection, &control).is_err(), "{change}");
    }
}
