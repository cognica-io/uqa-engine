//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standalone graph namespaces join native logical sessions without losing their distinct row format.

use super::{open, MODES};
use uqa_core::{Value, Vertex};
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteGraphStore};

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

fn vertex(id: u64, value: i64) -> Vertex {
    Vertex {
        vertex_id: id,
        label: "item".into(),
        properties: [("value".into(), Value::Int(value))].into(),
    }
}

#[test]
fn independent_standalone_label_allocations_commit_and_reopen() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("standalone-allocations.db");
        let a = open(mode, &path);
        let mut left = SQLiteGraphStore::open(a.clone(), Some("direct")).unwrap();
        left.create_graph("g").unwrap();
        let baseline = left.allocate_vertex_id("item", "g").unwrap();
        left.add_vertex(vertex(baseline, 0), "g").unwrap();
        bind(&a);
        let b = open(mode, &path);
        bind(&b);
        let mut right = SQLiteGraphStore::open(b.clone(), Some("direct")).unwrap();
        a.begin_transaction().unwrap();
        let first = left.allocate_vertex_id("item", "g").unwrap();
        left.add_vertex(vertex(first, 10), "g").unwrap();
        b.begin_transaction().unwrap();
        let second = right.allocate_vertex_id("item", "g").unwrap();
        assert_ne!(
            first, second,
            "independent sessions reused a graph identity"
        );
        right.add_vertex(vertex(second, 20), "g").unwrap();
        b.commit_transaction().unwrap();
        assert!(a.in_transaction());
        assert!(left.get_vertex(second).unwrap().is_none());
        a.commit_transaction().unwrap();
        assert_eq!(left.get_vertex(second).unwrap(), Some(vertex(second, 20)));
        assert_eq!(right.get_vertex(first).unwrap(), Some(vertex(first, 10)));
        drop((left, right, a, b));
        let connection = open(mode, &path);
        bind(&connection);
        let mut reopened = SQLiteGraphStore::open(connection, Some("direct")).unwrap();
        assert_eq!(reopened.vertices_in_graph("g").unwrap().len(), 3);
        assert!(reopened.allocate_vertex_id("item", "g").unwrap() > first.max(second));
    }
}

#[test]
fn standalone_graph_namespaces_merge_independent_vertices_in_native_sessions() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("native-graphs.db");
        let a = open(mode, &path);
        Catalog::open(a.clone()).unwrap();
        bind(&a);
        let mut left = SQLiteGraphStore::open(a.clone(), Some("direct")).unwrap();
        left.create_graph("g").unwrap();
        left.add_vertex(vertex(1, 0), "g").unwrap();
        left.add_vertex(vertex(2, 0), "g").unwrap();
        let b = open(mode, &path);
        bind(&b);
        let mut right = SQLiteGraphStore::open(b.clone(), Some("direct")).unwrap();
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        left.add_vertex(vertex(1, 10), "g").unwrap();
        right.add_vertex(vertex(2, 20), "g").unwrap();
        b.commit_transaction().unwrap();
        assert!(a.in_transaction());
        assert_eq!(left.get_vertex(2).unwrap(), Some(vertex(2, 0)));
        a.commit_transaction().unwrap();
        assert_eq!(left.get_vertex(2).unwrap(), Some(vertex(2, 20)));
        assert_eq!(right.get_vertex(1).unwrap(), Some(vertex(1, 10)));
        drop((left, right, a, b));
        let reopened = open(mode, &path);
        bind(&reopened);
        let graph = SQLiteGraphStore::open(reopened, Some("direct")).unwrap();
        assert_eq!(graph.get_vertex(1).unwrap(), Some(vertex(1, 10)));
        assert_eq!(graph.get_vertex(2).unwrap(), Some(vertex(2, 20)));
    }
}

#[test]
fn standalone_graph_files_and_preexisting_handles_join_native_record_binding() {
    for suffix in [None, Some("direct")] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let mut graph = SQLiteGraphStore::open(connection.clone(), suffix).unwrap();
        graph.create_graph("g").unwrap();
        graph.add_vertex(vertex(1, 10), "g").unwrap();
        bind(&connection);
        assert_eq!(graph.get_vertex(1).unwrap(), Some(vertex(1, 10)));
        connection.begin_transaction().unwrap();
        graph.add_vertex(vertex(1, 20), "g").unwrap();
        connection.rollback_transaction().unwrap();
        assert_eq!(graph.get_vertex(1).unwrap(), Some(vertex(1, 10)));
        let reopened = SQLiteGraphStore::open(connection.clone(), suffix).unwrap();
        assert_eq!(reopened.get_vertex(1).unwrap(), Some(vertex(1, 10)));
    }
}

#[test]
fn standalone_graph_scopes_preserve_sqlite_case_identity_and_transaction_rollback() {
    use uqa_graph::{Direction, GraphStore};
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("scopes.db");
        let connection = open(mode, &path);
        let mut original = SQLiteGraphStore::open(connection.clone(), Some("Direct")).unwrap();
        original.create_graph("g").unwrap();
        for id in 1..=3 {
            original.add_vertex(vertex(id, 0), "g").unwrap();
        }
        original
            .add_edge(
                uqa_core::Edge {
                    edge_id: 1,
                    source_id: 1,
                    target_id: 2,
                    label: "rel".into(),
                    properties: std::collections::BTreeMap::new(),
                },
                "g",
            )
            .unwrap();
        bind(&connection);
        let mut same = SQLiteGraphStore::open(connection.clone(), Some("DIRECT")).unwrap();
        let mut other = SQLiteGraphStore::open(connection.clone(), None).unwrap();
        other.create_graph("g").unwrap();
        other.add_vertex(vertex(1, 99), "g").unwrap();
        connection.begin_transaction().unwrap();
        connection.savepoint("graph_change").unwrap();
        same.add_vertex(vertex(1, 10), "g").unwrap();
        same.add_edge(
            uqa_core::Edge {
                edge_id: 1,
                source_id: 3,
                target_id: 1,
                label: "reversed".into(),
                properties: std::collections::BTreeMap::new(),
            },
            "g",
        )
        .unwrap();
        assert!(original
            .neighbors(1, Some("rel"), Direction::Out, "g")
            .unwrap()
            .is_empty());
        assert_eq!(
            original
                .neighbors(3, Some("reversed"), Direction::Out, "g")
                .unwrap(),
            vec![1]
        );
        assert_eq!(other.get_vertex(1).unwrap(), Some(vertex(1, 99)));
        connection.rollback_to_savepoint("graph_change").unwrap();
        connection.release_savepoint("graph_change").unwrap();
        assert_eq!(
            original
                .neighbors(1, Some("rel"), Direction::Out, "g")
                .unwrap(),
            vec![2]
        );
        assert_eq!(original.get_vertex(1).unwrap(), Some(vertex(1, 0)));
        connection.commit_transaction().unwrap();
        original.copy_graph("g", "copy").unwrap();
        original.drop_graph("g").unwrap();
        assert_eq!(
            original.vertex_id_page("copy", Some(1), 1).unwrap(),
            vec![2]
        );
        assert_eq!(original.out_edge_ids(1, "copy").unwrap(), [1].into());
        drop((original, same, other, connection));
        let reopened = open(mode, &path);
        bind(&reopened);
        let copy = SQLiteGraphStore::open(reopened, Some("direct")).unwrap();
        assert_eq!(copy.graph_names().unwrap(), vec!["copy"]);
        assert_eq!(
            copy.neighbors(1, None, Direction::Out, "copy").unwrap(),
            vec![2]
        );
    }
}

#[test]
fn standalone_same_entity_conflicts_keep_committed_selectors_and_discard_private_changes() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    let mut a = SQLiteGraphStore::open(connection.clone(), None).unwrap();
    a.create_graph("g").unwrap();
    a.add_vertex(vertex(1, 0), "g").unwrap();
    let sibling = connection.new_session();
    let mut b = SQLiteGraphStore::open(sibling.clone(), None).unwrap();
    connection.begin_transaction().unwrap();
    sibling.begin_transaction().unwrap();
    let mut left = vertex(1, 10);
    left.label = "left".into();
    a.add_vertex(left, "g").unwrap();
    let mut right = vertex(1, 20);
    right.label = "right".into();
    b.add_vertex(right.clone(), "g").unwrap();
    sibling.commit_transaction().unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(a.get_vertex(1).unwrap(), Some(right));
    assert_eq!(a.vertex_ids_by_label("right", "g").unwrap(), vec![1]);
    assert!(a.vertex_ids_by_label("left", "g").unwrap().is_empty());
}

#[test]
fn standalone_selectors_skip_unselected_large_properties_under_a_small_session_allowance() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bounded.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let mut graph = SQLiteGraphStore::open(connection.clone(), Some("direct")).unwrap();
    graph.create_graph("g").unwrap();
    graph.add_vertex(vertex(1, 10), "g").unwrap();
    let mut large = vertex(2, 20);
    large.label = "large".into();
    large
        .properties
        .insert("large".into(), Value::Str("x".repeat(1 << 20)));
    graph.add_vertex(large, "g").unwrap();
    connection
        .with(|sqlite| {
            sqlite.execute(
                "UPDATE _graph_catalog_direct SET registry_json=?1 WHERE name='g'",
                [format!("{{\"unused\":\"{}\"}}", "x".repeat(1 << 20))],
            )?;
            Ok(())
        })
        .unwrap();
    bind(&connection);
    drop((graph, connection));
    let limited = ManagedConnection::open(&path).unwrap();
    limited
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 1 << 16,
        })
        .unwrap();
    let graph = SQLiteGraphStore::open(limited, Some("DIRECT")).unwrap();
    assert_eq!(graph.graph_names().unwrap(), vec!["g"]);
    assert_eq!(graph.vertex_ids_by_label("item", "g").unwrap(), vec![1]);
    assert_eq!(graph.vertex_ids_by_label("large", "g").unwrap(), vec![2]);
    assert_eq!(graph.get_vertex(1).unwrap(), Some(vertex(1, 10)));
    assert!(graph.get_vertex(2).is_err());
}
