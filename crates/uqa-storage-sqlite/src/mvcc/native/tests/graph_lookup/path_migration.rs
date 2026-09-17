//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Path directory upgrades preserve existing selectors and every source commit boundary.

use super::migration::{commit, dump, preserved, restore_format_one};
use super::*;
use crate::mvcc::schema;

type LookupHistory = Vec<(Vec<u8>, Vec<u8>, Option<Vec<u8>>)>;

fn prior_lookup_history(
    connection: &ManagedConnection,
    database: DatabaseId,
    control: &StorageReadControl,
) -> LookupHistory {
    let family = NativeRecordIdentity::family_prefix(Family::GraphLookups, control).unwrap();
    let paths =
        NativeRecordIdentity::new(Family::GraphLookups, NativeRecordOwner::Database(database))
            .unwrap()
            .encode_prefix(&[ValueRef::Text(b"path")], control)
            .unwrap();
    with(connection, |connection| {
        Ok(connection.prepare("SELECT key,sequence,value FROM _uqa_mvcc_versions WHERE substr(key,1,?1) = ?2 AND substr(key,1,?3) != ?4 ORDER BY key,sequence")?.query_map(params![i64::try_from(family.len()).unwrap(),&family[..],i64::try_from(paths.len()).unwrap(),&paths[..]],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?.collect::<Result<_,_>>()?)
    })
}

fn restore_format_two(
    connection: &ManagedConnection,
    database: DatabaseId,
    control: &StorageReadControl,
) {
    with(connection, |connection| {
        let _permit = schema::WritePermit::acquire(connection)?;
        let transaction = schema::begin(connection)?;
        for (name, _) in
            super::super::super::graph_lookup::source_triggers(&[Family::GraphPathIndexState])
        {
            transaction.execute_batch(&format!("DROP TRIGGER {name}"))?;
        }
        let prefix =
            NativeRecordIdentity::new(Family::GraphLookups, NativeRecordOwner::Database(database))?
                .encode_prefix(&[ValueRef::Text(b"path")], control)?;
        for table in ["_uqa_mvcc_heads", "_uqa_mvcc_versions"] {
            transaction.execute(
                &format!("DELETE FROM {table} WHERE substr(key, 1, ?1) = ?2"),
                params![i64::try_from(prefix.len()).unwrap(), &prefix[..]],
            )?;
        }
        let (capture_name, capture_sql) =
            crate::mvcc::native::capture::trigger(Family::GraphLookups, "DELETE");
        transaction.execute_batch(&format!("DROP TRIGGER {capture_name}"))?;
        transaction.execute(
            "DELETE FROM _uqa_mvcc_native_graph_lookup WHERE kind = 'path'",
            [],
        )?;
        transaction.execute_batch(&capture_sql)?;
        transaction.execute_batch("DROP TABLE _uqa_mvcc_native_ivf_guards; DROP TABLE _uqa_mvcc_native_occurrence_guards; DROP TABLE _occurrence_skips; DROP TABLE _occurrence_block_max; DROP TABLE _uqa_mvcc_native_format; CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 2), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49)); INSERT INTO _uqa_mvcc_native_format VALUES (1, 2, 49);")?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger("_uqa_mvcc_native_format", action).1)?;
        }
        transaction.commit()?;
        Ok(())
    });
}

#[test]
fn native_path_lookup_upgrade_preserves_sources_and_old_selectors_in_every_file_mode() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        for previous_format in [1, 2] {
            let path = directory
                .path()
                .join(format!("paths-{mode}-{previous_format}.db"));
            let connection = connection(&path, mode);
            seed(&connection);
            with(&connection, |connection| {
                connection.execute_batch("INSERT INTO _path_indexes VALUES ('paths', '[]'); INSERT INTO _graph_path_index_state VALUES ('paths','g','[]',1)")?;
                Ok(())
            });
            let control = StorageReadControl::with_limit(1 << 24);
            let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
            let baseline = store.snapshot(&control).unwrap();
            let original =
                records(&connection, &store, Family::GraphPathIndexState, &control).remove(0);
            let from = lookup(&store, ("path", "g", 0, "paths", 0), &control);
            let moved = replace(
                &original,
                1,
                ValueRef::Text("other\0日本語".as_bytes()),
                &control,
            );
            let to = lookup(&store, ("path", "other\0日本語", 0, "paths", 0), &control);
            let first = commit(&store, &[&from], &[&moved, &to], &control);
            let moved_view = store.snapshot(&control).unwrap();
            let invalid = replace(&moved, 3, ValueRef::Integer(0), &control);
            commit(&store, &[], &[&invalid], &control);
            let invalid_view = store.snapshot(&control).unwrap();
            let last = commit(&store, &[&invalid, &to], &[], &control);
            let removed_view = store.snapshot(&control).unwrap();
            if previous_format == 1 {
                restore_format_one(&connection, &control);
            } else {
                restore_format_two(&connection, store.database_id(), &control);
            }
            let before = preserved(&connection);
            let prior = if previous_format == 2 {
                prior_lookup_history(&connection, store.database_id(), &control)
            } else {
                Vec::new()
            };
            let old_lookups = if previous_format == 2 {
                dump(&connection,"SELECT * FROM _uqa_mvcc_native_graph_lookup ORDER BY kind,text_key,integer_key,entity_type,entity_id")
            } else {
                Vec::new()
            };
            let upgraded = SQLiteRecordStore::for_native(&connection, &control).unwrap();
            assert_eq!(preserved(&connection), before);
            assert_eq!(upgraded.database_id(), store.database_id());
            assert_eq!(
                upgraded.snapshot(&control).unwrap().sequence(),
                last.sequence
            );
            assert_eq!(
                upgraded.commit_status(first.transaction, &control).unwrap(),
                CommitStatus::Committed(first)
            );
            assert_live(baseline.as_ref(), &from, true, &control);
            assert_live(baseline.as_ref(), &to, false, &control);
            for view in [&moved_view, &invalid_view] {
                assert_live(view.as_ref(), &from, false, &control);
                assert_live(view.as_ref(), &to, true, &control);
                assert_eq!(
                    view.get(to.key(), &control).unwrap().unwrap().sequence(),
                    first.sequence
                );
            }
            assert_live(removed_view.as_ref(), &to, false, &control);
            if previous_format == 2 {
                assert_eq!(
                    prior_lookup_history(&connection, store.database_id(), &control),
                    prior
                );
                assert_eq!(dump(&connection,"SELECT * FROM _uqa_mvcc_native_graph_lookup ORDER BY kind,text_key,integer_key,entity_type,entity_id"),old_lookups);
            }
            assert_eq!(
                dump(&connection, "SELECT format FROM _uqa_mvcc_native_format"),
                vec![vec![rusqlite::types::Value::Integer(6)]]
            );
            assert_eq!(
                SQLiteRecordStore::for_native(&connection, &control)
                    .unwrap()
                    .database_id(),
                store.database_id()
            );
        }
    }
}

#[test]
fn native_path_lookup_upgrade_failure_preserves_predecessor_schema_and_history() {
    for failure in ["quota", "history", "missing-head", "changed-source"] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        seed(&connection);
        with(&connection, |connection| {
            connection
                .execute_batch("INSERT INTO _graph_path_index_state VALUES ('paths','g','[]',1)")?;
            Ok(())
        });
        let control = StorageReadControl::with_limit(1 << 24);
        let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        restore_format_two(&connection, store.database_id(), &control);
        if matches!(failure, "missing-head" | "changed-source") {
            with(&connection, |connection| {
                let _permit = schema::WritePermit::acquire(connection)?;
                if failure == "changed-source" {
                    let (name, sql) = crate::mvcc::native::capture::trigger(
                        Family::GraphPathIndexState,
                        "UPDATE",
                    );
                    connection.execute_batch(&format!("DROP TRIGGER {name}"))?;
                    connection.execute(
                        "UPDATE _graph_path_index_state SET graph_name = 'broken'",
                        [],
                    )?;
                    connection.execute_batch(&sql)?;
                } else {
                    let row = record(
                        &store,
                        Family::GraphPathIndexState,
                        &[
                            ValueRef::Text(b"paths"),
                            ValueRef::Text(b"g"),
                            ValueRef::Text(b"[]"),
                            ValueRef::Integer(1),
                        ],
                        &control,
                    );
                    connection
                        .execute("DELETE FROM _uqa_mvcc_heads WHERE key = ?1", [row.key()])?;
                }
                Ok(())
            });
        }
        if failure == "history" {
            with(&connection, |connection| {
                connection.execute_batch("CREATE TRIGGER reject_path_backfill BEFORE INSERT ON _uqa_mvcc_versions BEGIN SELECT RAISE(ABORT, 'injected path history failure'); END")?;
                Ok(())
            });
        }
        let before = preserved(&connection);
        let schema_before = dump(
            &connection,
            "SELECT name,sql FROM sqlite_schema ORDER BY name",
        );
        let small = StorageReadControl::with_limit(1);
        assert!(SQLiteRecordStore::for_native(
            &connection,
            if failure == "quota" { &small } else { &control }
        )
        .is_err());
        assert_eq!(preserved(&connection), before);
        assert_eq!(
            dump(
                &connection,
                "SELECT name,sql FROM sqlite_schema ORDER BY name"
            ),
            schema_before
        );
    }
}

#[test]
fn a_closed_format_two_file_upgrades_live_paths_without_commit_capture_state() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory
            .path()
            .join(format!("live-path-upgrade-{mode}.db"));
        let control = StorageReadControl::with_limit(1 << 24);
        let (database, before) = {
            let connection = connection(&path, mode);
            seed(&connection);
            with(&connection, |connection| {
                connection.execute_batch(
                    "INSERT INTO _graph_path_index_state VALUES ('paths','g','[]',1)",
                )?;
                Ok(())
            });
            let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
            restore_format_two(&connection, store.database_id(), &control);
            (store.database_id(), preserved(&connection))
        };
        let reopened = connection(&path, mode);
        let store = SQLiteRecordStore::for_native(&reopened, &control).unwrap();
        assert_eq!(store.database_id(), database);
        assert_eq!(preserved(&reopened), before);
        let row = lookup(&store, ("path", "g", 0, "paths", 0), &control);
        assert_live(
            store.snapshot(&control).unwrap().as_ref(),
            &row,
            true,
            &control,
        );
        assert_eq!(
            dump(&reopened, "SELECT count(*) FROM _uqa_mvcc_native_changes"),
            vec![vec![rusqlite::types::Value::Integer(0)]]
        );
        assert_eq!(
            dump(&reopened, "SELECT count(*) FROM _uqa_mvcc_native_expected"),
            vec![vec![rusqlite::types::Value::Integer(0)]]
        );
    }
}

#[test]
fn native_path_ownership_requires_its_complete_directory_change() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    seed(&connection);
    with(&connection, |connection| {
        connection
            .execute_batch("INSERT INTO _graph_path_index_state VALUES ('paths','g','[]',1)")?;
        Ok(())
    });
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let current = store.snapshot(&control).unwrap();
    let original = records(&connection, &store, Family::GraphPathIndexState, &control).remove(0);
    let moved = replace(&original, 1, ValueRef::Text(b"other"), &control);
    let old_lookup = lookup(&store, ("path", "g", 0, "paths", 0), &control);
    let new_lookup = lookup(&store, ("path", "other", 0, "paths", 0), &control);
    for omitted in ["old", "new", "source", "source-and-new"] {
        let mut writes = Vec::new();
        if !matches!(omitted, "source" | "source-and-new") {
            writes.push(moved.write(Some(current.sequence())));
        }
        if omitted != "old" {
            writes.push(delete(&old_lookup));
        }
        if !matches!(omitted, "new" | "source-and-new") {
            writes.push(new_lookup.write(None));
        }
        let prepared = PreparedRecordCommit::new(&writes, &control).unwrap();
        let result = store.commit(
            store.allocate_transaction(&control).unwrap(),
            &prepared,
            &control,
        );
        assert!(result.is_err(), "{omitted}");
        let view = store.snapshot(&control).unwrap();
        assert_eq!(view.sequence(), current.sequence());
        assert_live(view.as_ref(), &original, true, &control);
        assert_live(view.as_ref(), &old_lookup, true, &control);
        assert_live(view.as_ref(), &new_lookup, false, &control);
    }
}
