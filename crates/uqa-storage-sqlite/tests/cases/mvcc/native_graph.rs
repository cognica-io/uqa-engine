//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public native catalog mutations retain graph snapshots and derived cache publication.

use super::{open, MODES};
use uqa_storage::{
    mvcc::VersionedSessionOptions, EdgeRow, GraphEntityFilter, GraphEntityKind, GraphSnapshot,
    GraphVertexRow,
};
use uqa_storage_sqlite::{Catalog, ManagedConnection};

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}
fn build(catalog: &Catalog, index: &str, graph: &str) {
    catalog.save_path_index(index, "[]").unwrap();
    catalog.finish_path_index_data(index, graph, "[]").unwrap();
    assert!(catalog.path_index_data_is_current(index, "[]").unwrap());
}

#[test]
fn independent_native_label_allocations_commit_and_reopen() {
    use std::sync::Arc;
    use uqa_core::Vertex;
    use uqa_graph::{GraphStore, PersistentGraphStore};
    use uqa_storage_sqlite::SQLiteStorageBackend;

    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph-allocations.db");
        let a = open(mode, &path);
        Catalog::open(a.clone()).unwrap();
        bind(&a);
        let graph = |connection: &ManagedConnection| {
            PersistentGraphStore::from_catalog(
                Arc::new(Catalog::open(connection.clone()).unwrap()),
                Arc::new(SQLiteStorageBackend::new(connection.clone())),
            )
        };
        let mut left = graph(&a);
        left.create_graph("g").unwrap();
        let baseline = left.allocate_vertex_id("item", "g").unwrap();
        left.add_vertex(Vertex::new(baseline, "item"), "g").unwrap();
        let b = open(mode, &path);
        bind(&b);
        let mut right = graph(&b);
        a.begin_transaction().unwrap();
        let first = left.allocate_vertex_id("item", "g").unwrap();
        left.add_vertex(Vertex::new(first, "item"), "g").unwrap();
        b.begin_transaction().unwrap();
        let second = right.allocate_vertex_id("item", "g").unwrap();
        assert_ne!(
            first, second,
            "independent sessions reused a graph identity"
        );
        right.add_vertex(Vertex::new(second, "item"), "g").unwrap();
        b.commit_transaction().unwrap();
        assert!(a.in_transaction());
        assert!(left.get_vertex(second).unwrap().is_none());
        a.commit_transaction().unwrap();
        assert!(left.get_vertex(second).unwrap().is_some());
        assert!(right.get_vertex(first).unwrap().is_some());
        drop((left, right, a, b));
        let connection = open(mode, &path);
        bind(&connection);
        let mut reopened = graph(&connection);
        assert_eq!(reopened.vertices_in_graph("g").unwrap().len(), 3);
        assert!(reopened.allocate_vertex_id("item", "g").unwrap() > first.max(second));
    }
}

#[test]
fn independent_native_graph_sources_commit_with_cache_effects_and_reopen_in_every_mode() {
    let directory = tempfile::tempdir().unwrap();
    for mode in MODES {
        let path = directory.path().join(format!("native-graph-{mode:?}.db"));
        let a = open(mode, &path);
        let first = Catalog::open(a.clone()).unwrap();
        bind(&a);
        first.save_named_graph("g\0日本語").unwrap();
        for id in 1..=2 {
            first.save_vertex(id, "node", "{}").unwrap();
            first
                .save_graph_membership("vertex", id, "g\0日本語")
                .unwrap();
        }
        first.save_edge(10, 1, 2, "edge", "{}").unwrap();
        first
            .save_graph_membership("edge", 10, "g\0日本語")
            .unwrap();
        build(&first, "paths\0日本語", "g\0日本語");
        first
            .save_path_index_pairs("paths\0日本語", "[]", &[(1, 2)])
            .unwrap();
        let b = open(mode, &path);
        bind(&b);
        let second = Catalog::open(b.clone()).unwrap();
        a.begin_transaction().unwrap();
        first.save_vertex(1, "changed", "{\"value\":1}").unwrap();
        b.begin_transaction().unwrap();
        second
            .save_edge(10, 2, 1, "reverse", "{\"value\":2}")
            .unwrap();
        b.commit_transaction().unwrap();
        assert!(a.in_transaction());
        assert_eq!(first.graph_edge(10).unwrap().unwrap().source_id, 1);
        a.commit_transaction().unwrap();
        assert!(!second
            .path_index_data_is_current("paths\0日本語", "[]")
            .unwrap());
        assert_eq!(first.graph_vertex(1).unwrap().unwrap().label, "changed");
        assert_eq!(first.graph_edge(10).unwrap().unwrap().source_id, 2);
        assert_eq!(
            first
                .graph_entity_ids(
                    GraphEntityFilter {
                        source: Some(2),
                        ..GraphEntityFilter::new(GraphEntityKind::Edge, Some("g\0日本語"))
                    },
                    None,
                    4
                )
                .unwrap(),
            vec![10]
        );
        assert_eq!(
            first
                .path_index_pairs("paths\0日本語", "[]", None, 4)
                .unwrap(),
            vec![(1, 2)]
        );
        drop((first, second, a, b));
        let connection = open(mode, &path);
        bind(&connection);
        let reopened = Catalog::open(connection).unwrap();
        assert!(!reopened
            .path_index_data_is_current("paths\0日本語", "[]")
            .unwrap());
        assert_eq!(
            reopened.graph_vertex(1).unwrap().unwrap().properties_json,
            "{\"value\":1}"
        );
        assert_eq!(reopened.graph_edge(10).unwrap().unwrap().target_id, 1);
    }
}

#[test]
fn native_graph_dependencies_follow_late_membership_reassigned_paths_and_savepoints() {
    let a = ManagedConnection::open_in_memory().unwrap();
    let first = Catalog::open(a.clone()).unwrap();
    bind(&a);
    let b = a.new_session();
    let second = Catalog::open(b.clone()).unwrap();
    first.save_named_graph("g").unwrap();
    first.save_vertex(1, "node", "{}").unwrap();
    first.save_graph_membership("vertex", 1, "g").unwrap();
    build(&first, "moved", "g");
    a.begin_transaction().unwrap();
    first.save_vertex(1, "node", "{\"value\":1}").unwrap();
    second.save_named_graph("late").unwrap();
    second.save_graph_membership("vertex", 1, "late").unwrap();
    build(&second, "late", "late");
    second.save_named_graph("other").unwrap();
    second
        .finish_path_index_data("moved", "other", "[]")
        .unwrap();
    a.commit_transaction().unwrap();
    assert!(!first.path_index_data_is_current("late", "[]").unwrap());
    assert!(first.path_index_data_is_current("moved", "[]").unwrap());
    a.begin_transaction().unwrap();
    first.finish_path_index_data("late", "late", "[]").unwrap();
    second.save_vertex(1, "node", "{\"value\":2}").unwrap();
    a.commit_transaction().unwrap();
    assert!(!first.path_index_data_is_current("late", "[]").unwrap());
    a.begin_transaction().unwrap();
    first.save_vertex(1, "node", "{\"value\":3}").unwrap();
    first.finish_path_index_data("late", "late", "[]").unwrap();
    a.savepoint("rebuilt").unwrap();
    first.delete_graph_membership("vertex", 1, "late").unwrap();
    assert!(!first.path_index_data_is_current("late", "[]").unwrap());
    a.rollback_to_savepoint("rebuilt").unwrap();
    second.save_vertex(2, "outside", "{}").unwrap();
    a.commit_transaction().unwrap();
    assert!(first.path_index_data_is_current("late", "[]").unwrap());
}

fn replacement() -> GraphSnapshot {
    GraphSnapshot {
        vertices: vec![
            GraphVertexRow {
                vertex_id: 1,
                label: "shared".into(),
                properties_json: "{}".into(),
            },
            GraphVertexRow {
                vertex_id: 3,
                label: "discarded".into(),
                properties_json: "{}".into(),
            },
            GraphVertexRow {
                vertex_id: 3,
                label: "last".into(),
                properties_json: "{}".into(),
            },
        ],
        edges: vec![EdgeRow {
            edge_id: 11,
            source_id: 1,
            target_id: 3,
            label: "new".into(),
            properties_json: "{}".into(),
        }],
        label_registry_json: "{\"changed\":true}".into(),
    }
}

#[test]
fn native_graph_replacement_and_removal_match_legacy_and_restore_savepoints() {
    for native in [false, true] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        if native {
            bind(&connection);
        }
        for graph in ["g", "other"] {
            catalog.save_named_graph(graph).unwrap();
        }
        for id in [1, 2, 99] {
            catalog.save_vertex(id, "old", "{}").unwrap();
        }
        for id in [1, 2] {
            catalog.save_graph_membership("vertex", id, "g").unwrap();
        }
        catalog.save_graph_membership("vertex", 1, "other").unwrap();
        catalog.save_edge(10, 1, 2, "old", "{}").unwrap();
        catalog.save_graph_membership("edge", 10, "g").unwrap();
        build(&catalog, "g::paths", "g");
        build(&catalog, "other::paths", "other");
        catalog
            .save_path_index_pairs("g::paths", "[]", &[(1, 2)])
            .unwrap();
        let original = catalog.load_named_graph_snapshot("g").unwrap().unwrap();
        let replacement = replacement();
        connection.begin_transaction().unwrap();
        connection.savepoint("original").unwrap();
        catalog.replace_named_graph("g", &replacement).unwrap();
        let updated = catalog.load_named_graph_snapshot("g").unwrap().unwrap();
        assert_eq!(updated.vertices.len(), 2);
        assert_eq!(updated.vertices[1].label, "last");
        assert!(catalog.graph_vertex(2).unwrap().is_none());
        assert!(catalog.graph_vertex(99).unwrap().is_none());
        assert!(catalog.graph_edge(10).unwrap().is_none());
        assert!(!catalog
            .path_index_data_is_current("other::paths", "[]")
            .unwrap());
        assert!(catalog
            .path_index_pairs("g::paths", "[]", None, 4)
            .unwrap()
            .is_empty());
        connection.rollback_to_savepoint("original").unwrap();
        let restored = catalog.load_named_graph_snapshot("g").unwrap().unwrap();
        let rows = |snapshot: &GraphSnapshot| {
            snapshot
                .vertices
                .iter()
                .map(|row| {
                    (
                        row.vertex_id,
                        row.label.clone(),
                        row.properties_json.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(rows(&restored), rows(&original));
        assert!(catalog
            .path_index_data_is_current("g::paths", "[]")
            .unwrap());
        catalog.replace_named_graph("g", &replacement).unwrap();
        connection.commit_transaction().unwrap();
        assert_eq!(
            catalog
                .graph_entity_ids(
                    GraphEntityFilter {
                        label: Some("discarded"),
                        ..GraphEntityFilter::new(GraphEntityKind::Vertex, None)
                    },
                    None,
                    4
                )
                .unwrap(),
            Vec::<u64>::new()
        );
        catalog.drop_named_graph_data("g").unwrap();
        assert!(!catalog.named_graph_exists("g").unwrap());
        assert!(catalog.graph_vertex(1).unwrap().is_some());
        assert!(catalog.graph_vertex(3).unwrap().is_none());
        assert!(catalog.graph_edge(11).unwrap().is_none());
        catalog.delete_graph_membership_for_graph("other").unwrap();
        catalog.purge_orphan_graph_entities().unwrap();
        assert!(catalog.load_vertices().unwrap().is_empty());
    }
}

#[test]
fn native_graph_failed_replacement_does_not_publish_partial_entities_or_selectors() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    catalog.save_named_graph("g").unwrap();
    catalog.save_vertex(1, "kept", "{}").unwrap();
    catalog.save_graph_membership("vertex", 1, "g").unwrap();
    build(&catalog, "g::paths", "g");
    let replacement = GraphSnapshot {
        vertices: vec![GraphVertexRow {
            vertex_id: 2,
            label: "partial".into(),
            properties_json: "{}".into(),
        }],
        edges: vec![EdgeRow {
            edge_id: 10,
            source_id: 2,
            target_id: u64::MAX,
            label: "invalid".into(),
            properties_json: "{}".into(),
        }],
        label_registry_json: String::new(),
    };
    for explicit in [false, true] {
        if explicit {
            connection.begin_transaction().unwrap();
            catalog.save_vertex(3, "prior", "{}").unwrap();
        }
        assert!(catalog.replace_named_graph("g", &replacement).is_err());
        assert!(catalog.graph_vertex(2).unwrap().is_none());
        assert_eq!(catalog.graph_vertex(1).unwrap().unwrap().label, "kept");
        assert!(catalog
            .path_index_data_is_current("g::paths", "[]")
            .unwrap());
        assert_eq!(connection.in_transaction(), explicit);
        if explicit {
            assert!(catalog.graph_vertex(3).unwrap().is_some());
            connection.rollback_transaction().unwrap();
        }
    }
}

#[test]
fn a_private_native_path_binding_survives_its_later_invalidation() {
    for existing in [false, true] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        bind(&connection);
        catalog.save_named_graph("g").unwrap();
        catalog.save_vertex(1, "node", "{}").unwrap();
        catalog.save_graph_membership("vertex", 1, "g").unwrap();
        if existing {
            catalog.save_named_graph("other").unwrap();
            build(&catalog, "paths", "other");
        }
        connection.begin_transaction().unwrap();
        if !existing {
            catalog.save_path_index("paths", "[]").unwrap();
        }
        catalog.finish_path_index_data("paths", "g", "[]").unwrap();
        catalog.save_vertex(1, "changed", "{}").unwrap();
        assert!(!catalog.path_index_data_is_current("paths", "[]").unwrap());
        connection.commit_transaction().unwrap();
        assert!(!catalog.path_index_data_is_current("paths", "[]").unwrap());
        catalog.finish_path_index_data("paths", "g", "[]").unwrap();
        assert!(catalog.path_index_data_is_current("paths", "[]").unwrap());
        catalog.save_vertex(1, "again", "{}").unwrap();
        assert!(!catalog.path_index_data_is_current("paths", "[]").unwrap());
    }
}

#[test]
fn native_graph_mutations_select_dependencies_before_loading_unrelated_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bounded-graph.db");
    {
        let connection = ManagedConnection::open(&path).unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        for graph in ["g", "other"] {
            catalog.save_named_graph(graph).unwrap();
        }
        catalog.save_vertex(1, "node", "{}").unwrap();
        catalog.save_graph_membership("vertex", 1, "g").unwrap();
        catalog
            .save_vertex(2, "large", &"x".repeat(1 << 20))
            .unwrap();
        build(&catalog, "paths", "g");
        let large = "x".repeat(1 << 20);
        catalog.save_path_index("unrelated", &large).unwrap();
        catalog
            .finish_path_index_data("unrelated", "other", &large)
            .unwrap();
        bind(&connection);
    }
    let connection = ManagedConnection::open(&path).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 128 << 10,
        })
        .unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_vertex(1, "changed", "{}").unwrap();
    assert!(!catalog.path_index_data_is_current("paths", "[]").unwrap());
    assert!(catalog
        .save_path_index_pairs("paths", "[]", &[(1, 1), (1, u64::MAX)])
        .is_err());
    assert!(catalog
        .path_index_pairs("paths", "[]", None, 4)
        .unwrap()
        .is_empty());
    catalog.clear_path_index_data("paths").unwrap();
    catalog.finish_path_index_data("paths", "g", "[]").unwrap();
    assert!(catalog.path_index_data_is_current("paths", "[]").unwrap());
}
