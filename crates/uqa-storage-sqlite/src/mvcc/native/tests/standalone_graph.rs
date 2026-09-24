//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standalone graph conversion preserves source bytes and guards the scoped record format.

use super::*;

pub(in crate::mvcc::native) fn remove_empty_tables(
    connection: &rusqlite::Connection,
) -> crate::Result<()> {
    for &(family, _) in super::super::standalone_graph::schema::TABLES.iter().rev() {
        let table = family.layout().table;
        assert_eq!(
            connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))?,
            0
        );
        connection.execute_batch(&format!("DROP TABLE {table}"))?;
    }
    Ok(())
}

use crate::{SQLiteGraphStore, SQLiteRecordStore};
use materialization::{records, replace, with};
use uqa_storage::mvcc::{CommitFailure, PreparedRecordCommit, RecordWrite, VersionedPersistence};

fn legacy(connection: &ManagedConnection) {
    connection.with(|sqlite| {
        sqlite.execute_batch(r#"
            CREATE TABLE _graph_vertices (vertex_id INTEGER PRIMARY KEY, label TEXT NOT NULL, properties_json TEXT NOT NULL);
            CREATE TABLE _graph_edges (edge_id INTEGER PRIMARY KEY, source_id INTEGER NOT NULL, target_id INTEGER NOT NULL, label TEXT NOT NULL, properties_json TEXT NOT NULL);
            CREATE TABLE _graph_membership (graph TEXT NOT NULL, entity_kind TEXT NOT NULL, entity_id INTEGER NOT NULL, PRIMARY KEY(graph,entity_kind,entity_id));
            CREATE TABLE _graph_catalog (name TEXT PRIMARY KEY);
            INSERT INTO _graph_catalog VALUES ('g');
            INSERT INTO _graph_vertices VALUES (1,'item','{"bytes":[1,2],"empty":[],"list":[256]}'), (2,'item','{}');
            INSERT INTO _graph_edges VALUES (1,1,2,'rel','{"bytes":[3]}');
            INSERT INTO _graph_membership VALUES ('g','v',1),('g','v',2),('g','e',1);
        "#)?;
        Ok(())
    }).unwrap();
}

#[test]
fn standalone_legacy_bytes_and_missing_columns_survive_native_conversion() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    legacy(&connection);
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    let graph = SQLiteGraphStore::open(connection.clone(), None).unwrap();
    assert_eq!(
        graph.get_vertex(1).unwrap().unwrap().properties,
        [
            ("bytes".into(), uqa_core::Value::Bytes(vec![1, 2])),
            ("empty".into(), uqa_core::Value::Bytes(vec![])),
            (
                "list".into(),
                uqa_core::Value::List(vec![uqa_core::Value::Int(256)])
            ),
        ]
        .into()
    );
    assert_eq!(
        graph.get_edge(1).unwrap().unwrap().properties["bytes"],
        uqa_core::Value::Bytes(vec![3])
    );
    with(&connection, |sqlite| {
        assert_eq!(sqlite.query_row("SELECT properties_json,properties_format FROM _uqa_mvcc_native_standalone_graph_vertices WHERE scope='' AND vertex_id=1", [], |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?)))?, (r#"{"bytes":[1,2],"empty":[],"list":[256]}"#.into(),1));
        assert_eq!(
            sqlite.query_row("SELECT count(*) FROM _graph_vertices", [], |row| row
                .get::<_, i64>(0))?,
            0
        );
        assert!(sqlite
            .execute("INSERT INTO _graph_catalog VALUES ('old','{}')", [])
            .is_err());
        Ok(())
    });
}

#[test]
fn standalone_failed_conversion_restores_source_schema_and_data_before_retry() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    legacy(&connection);
    connection
        .with(|sqlite| {
            sqlite.execute(
                "UPDATE _graph_vertices SET properties_json=?1 WHERE vertex_id=2",
                [format!("{{\"large\":\"{}\"}}", "x".repeat(1 << 18))],
            )?;
            Ok(())
        })
        .unwrap();
    let before = with(&connection, |sqlite| {
        Ok(sqlite
            .prepare("SELECT type,name,sql FROM sqlite_schema ORDER BY type,name")?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    });
    assert!(
        SQLiteRecordStore::for_native(&connection, &StorageReadControl::with_limit(1 << 15))
            .is_err()
    );
    with(&connection, |sqlite| {
        let after = sqlite
            .prepare("SELECT type,name,sql FROM sqlite_schema ORDER BY type,name")?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(after, before);
        assert_eq!(
            sqlite.query_row(
                "SELECT length(properties_json) FROM _graph_vertices WHERE vertex_id=2",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            (1 << 18) + 12
        );
        Ok(())
    });
    SQLiteRecordStore::for_native(&connection, &StorageReadControl::with_limit(1 << 24)).unwrap();
}

#[test]
fn standalone_scopes_and_selectors_reject_incomplete_record_commits() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let mut graph = SQLiteGraphStore::open(connection.clone(), Some("Direct")).unwrap();
    graph.create_graph("g").unwrap();
    graph
        .add_vertex(
            uqa_core::Vertex {
                vertex_id: 1,
                label: "item".into(),
                properties: std::collections::BTreeMap::new(),
            },
            "g",
        )
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let original = records(
        &connection,
        &store,
        NativeRecordFamily::StandaloneGraphVertices,
        &control,
    );
    let changed = replace(&original[0], 2, ValueRef::Text(b"changed"), &control);
    let lookups = records(
        &connection,
        &store,
        NativeRecordFamily::StandaloneGraphLookups,
        &control,
    );
    let scopes = records(
        &connection,
        &store,
        NativeRecordFamily::StandaloneGraphScopes,
        &control,
    );
    let removed_binding = replace(&scopes[0], 1, ValueRef::Null, &control);
    let snapshot = store.snapshot(&control).unwrap();
    let revision = |record: &NativeRecord| {
        snapshot
            .metadata(record.key(), &control)
            .unwrap()
            .and_then(|meta| meta.revision)
    };
    for writes in [
        vec![changed.write(revision(&original[0]))],
        vec![RecordWrite {
            key: lookups[0].key(),
            expected: revision(&lookups[0]),
            value: None,
        }],
        vec![RecordWrite {
            key: scopes[0].key(),
            expected: revision(&scopes[0]),
            value: None,
        }],
        vec![removed_binding.write(revision(&scopes[0]))],
    ] {
        let prepared = PreparedRecordCommit::new(&writes, &control).unwrap();
        assert!(matches!(
            store.commit(
                store.allocate_transaction(&control).unwrap(),
                &prepared,
                &control
            ),
            Err(CommitFailure::Rejected(_))
        ));
        assert_eq!(
            store.snapshot(&control).unwrap().sequence(),
            snapshot.sequence()
        );
    }
    with(&connection, |sqlite| {
        assert!(sqlite
            .execute("DELETE FROM _graph_vertices_Direct", [])
            .is_err());
        sqlite.execute_batch("DROP VIEW _graph_vertices_Direct")?;
        Ok(())
    });
    assert!(SQLiteRecordStore::for_native(&connection, &control).is_err());
}

#[test]
fn newly_committed_standalone_namespaces_publish_legacy_guards_atomically() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    let observer = connection.new_session();
    let guarded = || {
        with(&observer, |sqlite| {
            Ok(sqlite.query_row("SELECT count(*) FROM sqlite_schema WHERE type='view' AND name='_graph_vertices_fresh'", [], |row| row.get::<_,i64>(0))?)
        })
    };
    connection.begin_transaction().unwrap();
    drop(SQLiteGraphStore::open(connection.clone(), Some("Fresh")).unwrap());
    assert_eq!(guarded(), 0);
    connection.rollback_transaction().unwrap();
    assert_eq!(guarded(), 0);
    with(&connection, |sqlite| {
        sqlite.execute_batch("CREATE TRIGGER fail_graph_history BEFORE INSERT ON _uqa_mvcc_versions BEGIN SELECT RAISE(ABORT,'injected graph publication failure'); END")?;
        Ok(())
    });
    assert!(SQLiteGraphStore::open(connection.clone(), Some("Fresh")).is_err());
    assert_eq!(guarded(), 0);
    with(&connection, |sqlite| {
        sqlite.execute_batch("DROP TRIGGER fail_graph_history")?;
        Ok(())
    });
    drop(SQLiteGraphStore::open(connection.clone(), Some("Fresh")).unwrap());
    assert_eq!(guarded(), 1);
    with(&connection, |sqlite| {
        assert!(sqlite
            .execute("INSERT INTO _graph_catalog_fresh VALUES ('old','{}')", [])
            .is_err());
        Ok(())
    });
    SQLiteRecordStore::for_native(&connection, &StorageReadControl::with_limit(1 << 20)).unwrap();
}

#[test]
fn standalone_graph_handles_open_in_read_only_sessions_and_reject_mutation() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let mut graph = SQLiteGraphStore::open(connection.clone(), Some("direct")).unwrap();
    graph.create_graph("g").unwrap();
    graph
        .add_vertex(uqa_core::Vertex::new(1, "item"), "g")
        .unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    connection.begin_record_read().unwrap();
    let reopened = SQLiteGraphStore::open(connection.clone(), Some("direct")).unwrap();
    assert_eq!(
        reopened.get_vertex(1).unwrap(),
        Some(uqa_core::Vertex::new(1, "item"))
    );
    assert!(graph
        .add_vertex(uqa_core::Vertex::new(1, "changed"), "g")
        .is_err());
    assert_eq!(
        reopened.get_vertex(1).unwrap(),
        Some(uqa_core::Vertex::new(1, "item"))
    );
    assert!(!connection.transaction_has_written().unwrap());
    connection.rollback_transaction().unwrap();
}
