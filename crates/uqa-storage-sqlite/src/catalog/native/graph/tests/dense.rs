//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn dense_label_selection_uses_range_reads_without_per_vertex_selects() {
    use rusqlite::hooks::{AuthAction, Authorization};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let connection = connection(1024);
    let snapshot = connection.native_snapshot().unwrap().unwrap();
    let selected = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&selected);
    connection
        .with_physical(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if matches!(context.action, AuthAction::Select) {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    let mut filter = GraphEntityFilter::new(GraphEntityKind::Vertex, Some("g"));
    filter.label = Some("node");
    assert_eq!(
        ids(&snapshot, filter, None, 1024).unwrap(),
        (1..=1024).collect::<Vec<_>>()
    );
    assert!(
        selected.load(Ordering::Relaxed) <= 256,
        "selection issued {} SELECTs",
        selected.load(Ordering::Relaxed)
    );
    connection
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>)?;
            sqlite.set_prepared_statement_cache_capacity(16);
            Ok(())
        })
        .unwrap();
}

#[test]
fn high_membership_fanout_preserves_secondary_filters_and_old_snapshots() {
    let connection = connection(32);
    let catalog = Catalog::open(connection.clone()).unwrap();
    let old = connection.native_snapshot().unwrap().unwrap();
    connection.begin_transaction().unwrap();
    for graph in ["a", "b", "c", "d"] {
        catalog.save_named_graph(graph).unwrap();
        for id in 1..=32 {
            catalog.save_graph_membership("vertex", id, graph).unwrap();
        }
    }
    for id in 1..=16 {
        catalog.delete_graph_membership("vertex", id, "g").unwrap();
    }
    let mut filter = GraphEntityFilter::new(GraphEntityKind::Vertex, Some("g"));
    filter.label = Some("node");
    assert_eq!(
        catalog.graph_entity_ids(filter, None, 32).unwrap(),
        (17..=32).collect::<Vec<_>>()
    );
    assert_eq!(
        ids(&old, filter, None, 32).unwrap(),
        (1..=32).collect::<Vec<_>>()
    );
    connection.rollback_transaction().unwrap();
    assert_eq!(
        catalog.graph_entity_ids(filter, None, 32).unwrap(),
        (1..=32).collect::<Vec<_>>()
    );
}

#[test]
fn dense_presence_does_not_hide_a_dangling_label_or_fail_a_shorter_page() {
    let connection = connection(16);
    connection.begin_transaction().unwrap();
    connection
        .with_native_write(|snapshot, batch| {
            snapshot.delete_prefix(
                batch,
                Family::GraphVertices,
                owner(snapshot),
                &[ValueRef::Integer(16)],
            )?;
            Ok(())
        })
        .unwrap()
        .unwrap();
    let snapshot = connection.native_snapshot().unwrap().unwrap();
    let mut filter = GraphEntityFilter::new(GraphEntityKind::Vertex, Some("g"));
    filter.label = Some("node");
    assert_eq!(
        ids(&snapshot, filter, None, 15).unwrap(),
        (1..=15).collect::<Vec<_>>()
    );
    let error = ids(&snapshot, filter, None, 16).unwrap_err();
    assert!(
        error.to_string().contains("references missing vertex 16"),
        "{error}"
    );
    connection.rollback_transaction().unwrap();
}

#[test]
fn standalone_dense_labels_keep_scope_and_private_membership_boundaries() {
    use crate::SQLiteGraphStore;
    use uqa_core::Vertex;

    let connection = connection(16);
    let mut left = SQLiteGraphStore::open(connection.clone(), Some("left")).unwrap();
    let mut right = SQLiteGraphStore::open(connection.clone(), Some("right")).unwrap();
    connection.begin_transaction().unwrap();
    for store in [&mut left, &mut right] {
        store.create_graph("g").unwrap();
        for id in 1..=16 {
            store.add_vertex(Vertex::new(id, "node"), "g").unwrap();
        }
    }
    connection.commit_transaction().unwrap();
    let old = connection.native_snapshot().unwrap().unwrap();
    connection.begin_transaction().unwrap();
    left.remove_vertex(8, "g").unwrap();
    let mut filter = GraphEntityFilter::new(GraphEntityKind::Vertex, Some("g"));
    filter.label = Some("node");
    let select = |snapshot: &NativeSnapshot, scope| {
        let mut selected = Vec::new();
        snapshot
            .visit_graph_ids(Some(scope), filter, None, |id| {
                selected.push(id);
                Ok(true)
            })
            .unwrap();
        selected
    };
    let current = connection.native_snapshot().unwrap().unwrap();
    assert_eq!(
        select(&current, "left"),
        (1..=16).filter(|id| *id != 8).collect::<Vec<_>>()
    );
    assert_eq!(select(&current, "right"), (1..=16).collect::<Vec<_>>());
    assert_eq!(select(&old, "left"), (1..=16).collect::<Vec<_>>());
    connection.rollback_transaction().unwrap();
    assert_eq!(
        left.vertex_ids_by_label("node", "g").unwrap(),
        (1..=16).collect::<Vec<_>>()
    );
}

/// Select `filter` after `after`, recording which physical tables the selection read.
fn select_reading(
    connection: &ManagedConnection,
    snapshot: &NativeSnapshot,
    filter: GraphEntityFilter<'_>,
    after: Option<u64>,
    limit: usize,
) -> (Vec<u64>, std::collections::BTreeSet<String>) {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{Arc, Mutex};
    let tables = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    let seen = Arc::clone(&tables);
    connection
        .with_physical(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: AuthContext<'_>| {
                if let AuthAction::Read { table_name, .. } = context.action {
                    seen.lock().unwrap().insert(table_name.to_owned());
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    let selected = ids(snapshot, filter, after, limit).unwrap();
    connection
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            sqlite.set_prepared_statement_cache_capacity(16);
            Ok(())
        })
        .unwrap();
    let tables = tables.lock().unwrap().clone();
    (selected, tables)
}

#[test]
fn latest_label_selection_reads_the_label_index_and_matches_the_records() {
    let connection = connection(300);
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_vertex(7, "other", "{}").unwrap();
    catalog.save_vertex(500, "node", "{}").unwrap();
    let snapshot = connection.native_snapshot().unwrap().unwrap();
    let mut filter = GraphEntityFilter::new(GraphEntityKind::Vertex, Some("g"));
    filter.label = Some("node");
    let pages = [(None, 256), (Some(100), 50), (Some(299), 256), (None, 1)];
    let mut latest = Vec::new();
    for (after, limit) in pages {
        let (selected, tables) = select_reading(&connection, &snapshot, filter, after, limit);
        assert!(tables.contains("_graph_vertices"), "{tables:?}");
        latest.push(selected);
    }
    assert!(!latest[0].contains(&7));
    // A later commit leaves the snapshot behind, so the same selections resolve its records.
    catalog.save_vertex(301, "node", "{}").unwrap();
    for ((after, limit), expected) in pages.into_iter().zip(latest) {
        let (selected, tables) = select_reading(&connection, &snapshot, filter, after, limit);
        assert!(!tables.contains("_graph_vertices"), "{tables:?}");
        assert_eq!(selected, expected, "after {after:?}, limit {limit}");
    }
}
