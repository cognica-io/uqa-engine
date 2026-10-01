//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed graph reads preserve the selected history and callback boundary.

use super::*;

#[test]
fn borrowed_vertices_share_admission_and_keep_request_order_and_absence() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let connection = connection(128);
    let snapshot = connection.native_snapshot().unwrap().unwrap();
    let transactions = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&transactions);
    connection
        .with_physical(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: AuthContext<'_>| {
                if matches!(context.action, AuthAction::Transaction { .. }) {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    let ids = [128, 1, 400, 1]
        .into_iter()
        .chain(1..=128)
        .collect::<Vec<_>>();
    let retained = snapshot.control.memory().used();
    let mut seen = Vec::new();
    assert_eq!(
        for_each_vertex_borrowed(&snapshot, &ids, &mut |id, row| {
            seen.push((
                id,
                row.map(|row| {
                    (
                        row.vertex_id,
                        row.label.clone(),
                        row.properties_json.clone(),
                    )
                }),
            ));
            true
        })
        .unwrap(),
        ids.len()
    );
    assert_eq!(
        seen,
        ids.iter()
            .map(|id| (*id, (*id <= 128).then(|| (*id, "node".into(), "{}".into()))))
            .collect::<Vec<_>>()
    );
    assert!(
        transactions.load(Ordering::Relaxed) <= 4,
        "one vertex stream must not reopen a transaction per entity"
    );
    assert_eq!(snapshot.control.memory().used(), retained);
    connection
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn borrowed_vertices_retain_private_values_and_tombstones_after_rollback() {
    let connection = connection(3);
    let catalog = Catalog::open(connection.clone()).unwrap();
    let original = connection.native_snapshot().unwrap().unwrap();
    connection.begin_transaction().unwrap();
    catalog.save_vertex(1, "changed", "{\"n\":7}").unwrap();
    catalog.delete_vertex(2).unwrap();
    let private = connection.native_snapshot().unwrap().unwrap();
    connection.rollback_transaction().unwrap();
    catalog.save_vertex(1, "later", "{\"n\":8}").unwrap();
    let current = connection.native_snapshot().unwrap().unwrap();
    for (snapshot, label, has_second) in [
        (&original, "node", true),
        (&private, "changed", false),
        (&current, "later", true),
    ] {
        let mut seen = Vec::new();
        for_each_vertex_borrowed(snapshot, &[2, 1, 2, 3], &mut |id, row| {
            seen.push((id, row.map(|row| row.label.clone())));
            true
        })
        .unwrap();
        assert_eq!(
            seen,
            vec![
                (2, has_second.then(|| "node".into())),
                (1, Some(label.into())),
                (2, has_second.then(|| "node".into())),
                (3, Some("node".into()))
            ]
        );
    }
}

#[test]
fn borrowed_vertices_stop_before_future_ids_and_preserve_cancellation_and_budget() {
    let connection = connection(2);
    let snapshot = connection.native_snapshot().unwrap().unwrap();
    let mut count = 0;
    assert_eq!(
        for_each_vertex_borrowed(&snapshot, &[1, u64::MAX], &mut |id, row| {
            assert_eq!(id, 1);
            assert!(row.is_some());
            count += 1;
            false
        })
        .unwrap(),
        1
    );
    assert_eq!(count, 1);
    let result = for_each_vertex_borrowed(&snapshot, &[1, 2], &mut |_, _| {
        snapshot.control.cancellation().cancel();
        false
    });
    assert!(matches!(
        result,
        Err(crate::SQLiteError::Cancelled(_) | crate::SQLiteError::StorageSource(_))
    ));
    assert!(for_each_vertex_borrowed(&snapshot, &[], &mut |_, _| unreachable!()).is_err());
    snapshot.control.cancellation().reset();
    let snapshot = NativeSnapshot {
        view: snapshot.view.try_clone().unwrap(),
        control: uqa_storage::read_control::StorageReadControl::with_limit(1),
        database: snapshot.database,
        history: snapshot.history,
    };
    assert!(for_each_vertex_borrowed(&snapshot, &[1], &mut |_, _| unreachable!()).is_err());
    assert_eq!(snapshot.control.memory().used(), 0);
}

#[test]
fn native_graph_handle_borrowing_preserves_cypher_errors_and_overlay_values() {
    use std::sync::Arc;
    use uqa_graph::{
        cypher::{parse_cypher, CypherError, CypherExecutor},
        GraphStore, GraphStoreHandle, PersistentGraphStore,
    };

    let connection = connection(3);
    let catalog = Arc::new(Catalog::open(connection.clone()).unwrap());
    catalog.save_vertex(1, "node", "{\"n\":1}").unwrap();
    catalog.save_vertex(2, "node", "invalid JSON").unwrap();
    let graph = PersistentGraphStore::from_catalog(
        catalog.clone(),
        Arc::new(crate::SQLiteStorageBackend::new(connection.clone())),
    );
    let handle = GraphStoreHandle::Persistent(graph.clone());
    let query = parse_cypher("MATCH (n:node {n: $absent}) RETURN n").unwrap();
    assert_eq!(
        CypherExecutor::new(&handle, "g")
            .execute(&query)
            .unwrap_err(),
        CypherError::UndefinedParameter("absent".into())
    );
    catalog.save_vertex(2, "node", "{\"n\":2}").unwrap();
    let mut overlay = graph.with_read_snapshot(&graph);
    assert_eq!(
        overlay
            .for_each_vertex_borrowed(&[1], &mut |_, row| {
                assert_eq!(row.unwrap().properties["n"], uqa_core::Value::Int(1));
                true
            })
            .unwrap(),
        Some(1)
    );
    connection.begin_transaction().unwrap();
    let mut replacement = uqa_core::Vertex::new(1, "node");
    replacement
        .properties
        .insert("n".into(), uqa_core::Value::Int(9));
    overlay.add_vertex(replacement, "g").unwrap();
    assert_eq!(
        overlay
            .for_each_vertex_borrowed(&[1], &mut |_, _| panic!(
                "changed overlay must use per-entity selection"
            ))
            .unwrap(),
        None
    );
    let query = parse_cypher("MATCH (n:node {n: 9}) RETURN n.n").unwrap();
    // Private deletion must keep its per-entity overlay selection.
    overlay.remove_vertex(2, "g").unwrap();
    let (_, rows) = CypherExecutor::new(&overlay, "g").execute(&query).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["n.n"], uqa_core::Value::Int(9));
    connection.rollback_transaction().unwrap();
}

type Read = Vec<(u64, Option<(u64, String, String)>)>;

/// Read `ids` with `limit` visits, recording which physical tables the read used.
fn read_vertices(
    connection: &ManagedConnection,
    snapshot: &NativeSnapshot,
    ids: &[u64],
    limit: usize,
) -> (Read, std::collections::BTreeSet<String>) {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{Arc, Mutex};
    let tables = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    let seen_tables = Arc::clone(&tables);
    connection
        .with_physical(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: AuthContext<'_>| {
                if let AuthAction::Read { table_name, .. } = context.action {
                    seen_tables.lock().unwrap().insert(table_name.to_owned());
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    let mut seen = Vec::new();
    let count = for_each_vertex_borrowed(snapshot, ids, &mut |id, row| {
        seen.push((
            id,
            row.map(|row| {
                (
                    row.vertex_id,
                    row.label.clone(),
                    row.properties_json.clone(),
                )
            }),
        ));
        seen.len() < limit
    })
    .unwrap();
    assert_eq!(count, seen.len());
    connection
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            sqlite.set_prepared_statement_cache_capacity(16);
            Ok(())
        })
        .unwrap();
    let tables = tables.lock().unwrap().clone();
    (seen, tables)
}

#[test]
fn latest_ascending_batches_read_the_vertex_projection_and_match_the_records() {
    let connection = connection(300);
    let catalog = Catalog::open(connection.clone()).unwrap();
    // Properties above the inline limit are admitted and read by themselves.
    let wide = format!("{{\"text\":\"{}\"}}", "x".repeat(20_000));
    catalog.save_vertex(150, "wide", &wide).unwrap();
    let snapshot = connection.native_snapshot().unwrap().unwrap();
    let dense = (1..=300).chain([400]).collect::<Vec<_>>();
    let sparse = [5, 150, 290, 1_000, 5_000];
    let batches = [
        (&dense[..], usize::MAX),
        (&sparse[..], usize::MAX),
        (&dense[..], 3),
    ];
    let mut latest = Vec::new();
    for (ids, limit) in batches {
        let (seen, tables) = read_vertices(&connection, &snapshot, ids, limit);
        assert!(tables.contains("_graph_vertices"), "{tables:?}");
        latest.push(seen);
    }
    // A later commit leaves the snapshot behind, so the same reads resolve its records.
    catalog.save_vertex(301, "later", "{}").unwrap();
    for ((ids, limit), expected) in batches.into_iter().zip(latest) {
        let (seen, tables) = read_vertices(&connection, &snapshot, ids, limit);
        assert!(!tables.contains("_graph_vertices"), "{tables:?}");
        assert_eq!(seen, expected);
    }
    let (seen, _) = read_vertices(&connection, &snapshot, &[150], usize::MAX);
    assert_eq!(seen, [(150, Some((150, "wide".into(), wide)))]);
}
