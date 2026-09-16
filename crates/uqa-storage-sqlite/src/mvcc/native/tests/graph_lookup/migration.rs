//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Upgrade the old native format without replacing source histories or their original commit boundaries.

use rusqlite::types::Value;
use uqa_storage::mvcc::{CommitReceipt, RecordWrite};

use super::*;
use crate::mvcc::schema;

const OLD_FORMAT: &str = "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 1), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))";

// Source/history encodings are unchanged. Remove only the new derived records/table from a populated file and pin the predecessor's exact marker and guards.
pub(super) fn restore_format_one(connection: &ManagedConnection, control: &StorageReadControl) {
    with(connection, |connection| {
        let _permit = schema::WritePermit::acquire(connection)?;
        let transaction = schema::begin(connection)?;
        let names: Vec<String> = transaction.prepare("SELECT name FROM sqlite_schema WHERE type = 'trigger' AND name GLOB '_uqa_mvcc_graph_lookup_*'")?.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?;
        for name in names {
            transaction.execute_batch(&format!("DROP TRIGGER {name}"))?;
        }
        let prefix = NativeRecordIdentity::family_prefix(Family::GraphLookups, control)?;
        for table in ["_uqa_mvcc_heads", "_uqa_mvcc_versions"] {
            transaction.execute(
                &format!("DELETE FROM {table} WHERE substr(key, 1, ?1) = ?2"),
                params![i64::try_from(prefix.len()).unwrap(), &prefix[..]],
            )?;
        }
        transaction.execute_batch(
            "DROP TABLE _uqa_mvcc_native_graph_lookup; DROP TABLE _uqa_mvcc_native_format; DROP TABLE _uqa_mvcc_native_occurrence_guards; DROP TABLE _occurrence_skips; DROP TABLE _occurrence_block_max;",
        )?;
        transaction.execute_batch(OLD_FORMAT)?;
        transaction.execute("INSERT INTO _uqa_mvcc_native_format VALUES (1, 1, 49)", [])?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger("_uqa_mvcc_native_format", action).1)?;
        }
        transaction.commit()?;
        Ok(())
    });
}

pub(super) fn dump(connection: &ManagedConnection, sql: &str) -> Vec<Vec<Value>> {
    with(connection, |connection| {
        let mut statement = connection.prepare(sql)?;
        let count = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..count)
                    .map(|slot| row.get(slot))
                    .collect::<Result<Vec<Value>, _>>()
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
}

pub(super) fn preserved(connection: &ManagedConnection) -> Vec<Vec<Vec<Value>>> {
    [
        "SELECT * FROM _uqa_mvcc_metadata",
        "SELECT * FROM _uqa_mvcc_transactions ORDER BY allocation",
        "SELECT * FROM _uqa_mvcc_heads WHERE substr(key, 1, 21) != x'007571612d6e61746976652d7265636f726401002d' ORDER BY key",
        "SELECT * FROM _uqa_mvcc_versions WHERE substr(key, 1, 21) != x'007571612d6e61746976652d7265636f726401002d' ORDER BY key, sequence",
    ].map(|sql| dump(connection, sql)).into()
}

pub(super) fn commit(
    store: &SQLiteRecordStore,
    deletes: &[&NativeRecord],
    puts: &[&NativeRecord],
    control: &StorageReadControl,
) -> CommitReceipt {
    let snapshot = store.snapshot(control).unwrap();
    let mut changes: Vec<_> = deletes
        .iter()
        .map(|row| RecordWrite {
            key: row.key(),
            expected: snapshot
                .metadata(row.key(), control)
                .unwrap()
                .and_then(|found| found.revision),
            value: None,
        })
        .collect();
    changes.extend(puts.iter().map(|row| {
        row.write(
            snapshot
                .metadata(row.key(), control)
                .unwrap()
                .and_then(|found| found.revision),
        )
    }));
    let batch = PreparedRecordCommit::new(&changes, control).unwrap();
    store
        .commit(
            store.allocate_transaction(control).unwrap(),
            &batch,
            control,
        )
        .unwrap()
}

struct GraphHistory {
    snapshots: Vec<Arc<dyn CommittedRecordSnapshot>>,
    receipts: [CommitReceipt; 3],
    original: [NativeRecord; 3],
    replaced: [NativeRecord; 3],
    node: NativeRecord,
    membership: NativeRecord,
    edges: [NativeRecord; 3],
}

fn graph_history(
    connection: &ManagedConnection,
    store: &SQLiteRecordStore,
    control: &StorageReadControl,
) -> GraphHistory {
    let mut edges = records(connection, store, Family::GraphEdges, control);
    let vertices = records(connection, store, Family::GraphVertices, control);
    let member = record(
        store,
        Family::GraphMembership,
        &[
            ValueRef::Text(b"edge"),
            ValueRef::Integer(9),
            ValueRef::Text(b"g"),
        ],
        control,
    );
    let original = [
        lookup(store, ("label", "rel", 0, "edge", 9), control),
        lookup(store, ("source", "", 1, "edge", 9), control),
        lookup(store, ("target", "", 2, "edge", 9), control),
    ];
    let replaced = [
        lookup(store, ("label", "new\0rel", 0, "edge", 9), control),
        lookup(store, ("source", "", 2, "edge", 9), control),
        lookup(store, ("target", "", 1, "edge", 9), control),
    ];
    let node = lookup(store, ("label", "node", 0, "vertex", 2), control);
    let membership = lookup(store, ("member", "g", 0, "edge", 9), control);
    let changed = record(
        store,
        Family::GraphEdges,
        &[
            ValueRef::Integer(9),
            ValueRef::Integer(2),
            ValueRef::Integer(1),
            ValueRef::Text(b"new\0rel"),
            ValueRef::Text(b"{}"),
        ],
        control,
    );
    let mut snapshots = vec![store.snapshot(control).unwrap()];
    let mut deletes: Vec<_> = original.iter().collect();
    deletes.extend([&vertices[1], &node, &member, &membership]);
    let mut puts: Vec<_> = replaced.iter().collect();
    puts.push(&changed);
    let first = commit(store, &deletes, &puts, control);
    snapshots.push(store.snapshot(control).unwrap());
    let properties = replace(&changed, 4, ValueRef::Text(b"{\"n\":3}"), control);
    let second = commit(
        store,
        &[],
        &[&properties, &vertices[1], &node, &member, &membership],
        control,
    );
    snapshots.push(store.snapshot(control).unwrap());
    let mut deletes: Vec<_> = replaced.iter().collect();
    deletes.extend([&properties, &member, &membership]);
    let third = commit(store, &deletes, &[], control);
    snapshots.push(store.snapshot(control).unwrap());
    GraphHistory {
        snapshots,
        receipts: [first, second, third],
        original,
        replaced,
        node,
        membership,
        edges: [edges.remove(0), changed, properties],
    }
}

fn assert_history(history: &GraphHistory, control: &StorageReadControl) {
    for (slot, snapshot) in history.snapshots.iter().enumerate() {
        for row in &history.original {
            assert_live(snapshot.as_ref(), row, slot == 0, control);
        }
        for row in &history.replaced {
            assert_live(snapshot.as_ref(), row, slot == 1 || slot == 2, control);
        }
        assert_live(snapshot.as_ref(), &history.node, slot != 1, control);
        assert_live(
            snapshot.as_ref(),
            &history.membership,
            slot == 0 || slot == 2,
            control,
        );
        assert_live(
            snapshot.as_ref(),
            &history.edges[slot.min(2)],
            slot != 3,
            control,
        );
    }
    // Property-only changes do not fabricate selector revisions.
    assert_eq!(
        history.snapshots[2]
            .get(history.replaced[0].key(), control)
            .unwrap()
            .unwrap()
            .sequence(),
        history.receipts[0].sequence
    );
}

#[test]
fn native_graph_lookup_upgrade_preserves_every_boundary_and_receipt_in_all_file_modes() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory.path().join(format!("lookup-upgrade-{mode}.db"));
        let connection = connection(&path, mode);
        seed(&connection);
        let control = StorageReadControl::with_limit(1 << 24);
        let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        let history = graph_history(&connection, &store, &control);
        let pending = store.allocate_transaction(&control).unwrap();
        let aborted = store.allocate_transaction(&control).unwrap();
        store.abort(aborted, &control).unwrap();
        restore_format_one(&connection, &control);
        let before = preserved(&connection);
        let upgraded = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        assert_eq!(upgraded.database_id(), store.database_id());
        assert_eq!(preserved(&connection), before);
        assert_eq!(
            upgraded.snapshot(&control).unwrap().sequence(),
            history.receipts[2].sequence
        );
        assert_eq!(
            upgraded.commit_status(pending, &control).unwrap(),
            CommitStatus::Pending
        );
        assert_eq!(
            upgraded.commit_status(aborted, &control).unwrap(),
            CommitStatus::Aborted
        );
        for receipt in history.receipts {
            assert_eq!(
                upgraded
                    .commit_status(receipt.transaction, &control)
                    .unwrap(),
                CommitStatus::Committed(receipt)
            );
        }
        assert_history(&history, &control);
        let full = dump(
            &connection,
            "SELECT * FROM _uqa_mvcc_versions ORDER BY key, sequence",
        );
        let new_connection = super::connection(&path, mode);
        let reopened = SQLiteRecordStore::for_native(&new_connection, &control).unwrap();
        assert_eq!(reopened.database_id(), upgraded.database_id());
        assert_eq!(
            reopened.snapshot(&control).unwrap().sequence(),
            history.receipts[2].sequence
        );
        assert_eq!(
            dump(
                &connection,
                "SELECT * FROM _uqa_mvcc_versions ORDER BY key, sequence"
            ),
            full
        );
        with(&connection, |connection| {
            let matches_old: bool = connection.query_row(
                "SELECT format = 1 FROM _uqa_mvcc_native_format",
                [],
                |row| row.get(0),
            )?;
            assert!(!matches_old);
            assert!(!schema::definition_matches(
                connection,
                "_uqa_mvcc_native_format",
                OLD_FORMAT
            )?
            .unwrap());
            Ok(())
        });
    }
}

#[test]
fn native_graph_lookup_upgrade_failure_restores_original_format_and_all_history() {
    for failure in ["history", "format", "memory"] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        seed(&connection);
        with(&connection, |connection| {
            connection.execute(
                "UPDATE _graph_vertices SET properties_json = ?1 WHERE vertex_id = 2",
                ["x".repeat(1 << 20)],
            )?;
            Ok(())
        });
        let control = StorageReadControl::with_limit(1 << 25);
        SQLiteRecordStore::for_native(&connection, &control).unwrap();
        restore_format_one(&connection, &control);
        with(&connection, |connection| {
            if failure == "history" {
                connection.execute_batch("CREATE TRIGGER reject_lookup_backfill BEFORE INSERT ON _uqa_mvcc_versions WHEN (SELECT count(*) FROM _uqa_mvcc_versions WHERE substr(key, 1, 21) = x'007571612d6e61746976652d7265636f726401002d') >= 2 BEGIN SELECT RAISE(ABORT, 'injected backfill failure'); END")?;
            } else if failure == "format" {
                connection
                    .execute_batch("CREATE VIEW _uqa_mvcc_native_graph_lookup AS SELECT 1")?;
            }
            Ok(())
        });
        let before = preserved(&connection);
        let definitions = dump(
            &connection,
            "SELECT type, name, sql FROM sqlite_schema ORDER BY name",
        );
        let small = StorageReadControl::with_limit(32 << 10);
        let result = SQLiteRecordStore::for_native(
            &connection,
            if failure == "memory" {
                &small
            } else {
                &control
            },
        );
        assert!(result.is_err(), "{failure}");
        assert_eq!(small.memory().used(), 0);
        assert_eq!(preserved(&connection), before, "{failure}");
        assert_eq!(
            dump(
                &connection,
                "SELECT type, name, sql FROM sqlite_schema ORDER BY name"
            ),
            definitions,
            "{failure}"
        );
        with(&connection, |connection| {
            connection.execute_batch("DROP TRIGGER IF EXISTS reject_lookup_backfill; DROP VIEW IF EXISTS _uqa_mvcc_native_graph_lookup;")?;
            Ok(())
        });
        SQLiteRecordStore::for_native(&connection, &control).unwrap();
        assert_eq!(preserved(&connection), before);
    }
}

#[test]
fn native_graph_lookup_upgrade_rejects_source_and_history_disagreement_in_both_directions() {
    for corruption in [
        "missing_history",
        "missing_head",
        "changed_history",
        "missing_source",
        "future_history",
        "orphaned_deleted_source",
    ] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        seed(&connection);
        let control = StorageReadControl::with_limit(1 << 24);
        let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        let vertices = records(&connection, &store, Family::GraphVertices, &control);
        restore_format_one(&connection, &control);
        with(&connection, |connection| {
            let _permit = schema::WritePermit::acquire(connection)?;
            match corruption {
                "missing_history" => {
                    connection.execute(
                        "DELETE FROM _uqa_mvcc_versions WHERE key = ?1",
                        [vertices[0].key()],
                    )?;
                }
                "changed_history" => {
                    let changed = replace(&vertices[0], 1, ValueRef::Text(b"different"), &control);
                    connection.execute(
                        "UPDATE _uqa_mvcc_versions SET value = ?2 WHERE key = ?1",
                        params![changed.key(), changed.row()],
                    )?;
                }
                "missing_head" => {
                    connection.execute(
                        "DELETE FROM _uqa_mvcc_heads WHERE key = ?1",
                        [vertices[0].key()],
                    )?;
                }
                "missing_source" => {
                    let missing = replace(&vertices[0], 0, ValueRef::Integer(3), &control);
                    crate::mvcc::write::stage_record(
                        connection,
                        missing.key(),
                        Some(missing.row()),
                        CommitSequence::from_u64(1),
                        &control,
                    )?;
                }
                "orphaned_deleted_source" => {
                    let missing = replace(&vertices[0], 0, ValueRef::Integer(3), &control);
                    crate::mvcc::write::stage_record(
                        connection,
                        missing.key(),
                        Some(missing.row()),
                        CommitSequence::from_u64(1),
                        &control,
                    )?;
                    crate::mvcc::write::stage_record(
                        connection,
                        missing.key(),
                        None,
                        CommitSequence::from_u64(2),
                        &control,
                    )?;
                    connection.execute(
                        "UPDATE _uqa_mvcc_metadata SET sequence = x'0000000000000002'",
                        [],
                    )?;
                    connection.execute(
                        "DELETE FROM _uqa_mvcc_heads WHERE key = ?1",
                        [missing.key()],
                    )?;
                }
                _ => {
                    connection.execute("UPDATE _uqa_mvcc_versions SET sequence = x'0000000000000002' WHERE key = ?1", [vertices[0].key()])?;
                    connection.execute(
                        "UPDATE _uqa_mvcc_heads SET sequence = x'0000000000000002' WHERE key = ?1",
                        [vertices[0].key()],
                    )?;
                }
            }
            Ok(())
        });
        let before = preserved(&connection);
        assert!(
            SQLiteRecordStore::for_native(&connection, &control).is_err(),
            "{corruption}"
        );
        assert_eq!(preserved(&connection), before);
        assert_eq!(
            dump(&connection, "SELECT format FROM _uqa_mvcc_native_format"),
            vec![vec![Value::Integer(1)]]
        );
    }
}
