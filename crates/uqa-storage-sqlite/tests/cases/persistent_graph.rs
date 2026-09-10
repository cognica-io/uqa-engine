//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable graph parity, bounded reads, and storage rollback coverage.

#[test]
fn versioned_overwrites_restore_global_records_and_exact_memberships() {
    let mut stores = vec![uqa_graph::GraphStoreHandle::default()];
    stores.extend(
        fixtures()
            .into_iter()
            .map(|(catalog, backend)| uqa_graph::GraphStoreHandle::from_catalog(catalog, backend)),
    );
    for mut store in stores {
        store.create_graph("primary").unwrap();
        store.create_graph("target").unwrap();
        for id in 1..=3 {
            store.add_vertex(vertex(id, "Before"), "primary").unwrap();
        }
        store.add_edge(edge(1, 1, 2, "before"), "primary").unwrap();
        store.add_edge(edge(2, 2, 3, "before"), "primary").unwrap();
        store.copy_graph("primary", "secondary").unwrap();
        let vertices = store.vertices().unwrap();
        let edges = store.edges().unwrap();
        {
            let mut versioned = uqa_graph::VersionedGraphStore::new(&mut store, "target");
            let mut delta = uqa_graph::GraphDelta::new();
            delta.add_vertex(vertex(1, "After"));
            delta.add_vertex(vertex(2, "Before"));
            delta.add_edge(edge(1, 2, 1, "after"));
            // Existing global entities outside target are not removals.
            delta.remove_vertex(3);
            delta.remove_edge(2);
            versioned.apply(delta).unwrap();
            for graph in ["primary", "secondary"] {
                assert_eq!(
                    versioned
                        .base()
                        .vertex_ids_by_label("Before", graph)
                        .unwrap(),
                    vec![2, 3]
                );
                assert_eq!(
                    versioned
                        .base()
                        .vertex_ids_by_label("After", graph)
                        .unwrap(),
                    vec![1]
                );
                assert_eq!(
                    versioned
                        .base()
                        .neighbors(2, Some("after"), Direction::Out, graph)
                        .unwrap(),
                    vec![1]
                );
                assert!(versioned
                    .base()
                    .neighbors(1, None, Direction::Out, graph)
                    .unwrap()
                    .is_empty());
            }
            versioned.rollback(0).unwrap();
        }
        assert_eq!(store.vertices().unwrap(), vertices);
        assert_eq!(store.edges().unwrap(), edges);
        assert!(store.vertex_ids_in_graph("target").unwrap().is_empty());
        assert!(store.edge_id_page("target", None, 256).unwrap().is_empty());
        for graph in ["primary", "secondary"] {
            assert_eq!(
                store.vertex_ids_by_label("Before", graph).unwrap(),
                vec![1, 2, 3]
            );
            assert!(store
                .vertex_ids_by_label("After", graph)
                .unwrap()
                .is_empty());
            assert_eq!(
                store
                    .neighbors(1, Some("before"), Direction::Out, graph)
                    .unwrap(),
                vec![2]
            );
        }
        store.add_vertex(vertex(4, "Before"), "primary").unwrap();
        let error = store
            .add_edge(edge(1, 1, 4, "invalid"), "primary")
            .unwrap_err();
        assert!(error.to_string().contains("secondary"), "{error}");
        assert_eq!(store.get_edge(1).unwrap(), edges.get(&1).cloned());
    }
}

#[test]
fn overlay_shared_edge_update_checks_live_endpoints_in_every_owner() {
    for ((read_catalog, read_backend), (write_catalog, write_backend)) in
        fixtures().into_iter().zip(fixtures())
    {
        let mut read = PersistentGraphStore::from_catalog(read_catalog, read_backend);
        let mut write = PersistentGraphStore::from_catalog(write_catalog, write_backend);
        for store in [&mut read, &mut write] {
            store.create_graph("first").unwrap();
            for id in 1..=3 {
                store.add_vertex(vertex(id, "Item"), "first").unwrap();
            }
            store.add_edge(edge(1, 1, 2, "link"), "first").unwrap();
            store.copy_graph("first", "second").unwrap();
        }
        write.remove_vertex(3, "second").unwrap();
        let mut overlay = write.with_read_snapshot(&read);
        let error = overlay
            .add_edge(edge(1, 1, 3, "link"), "first")
            .unwrap_err();
        assert!(
            matches!(error, GraphStoreError::SerializationFailure(_)),
            "{error}"
        );
        assert_eq!(write.get_edge(1).unwrap(), Some(edge(1, 1, 2, "link")));
        assert_eq!(overlay.get_edge(1).unwrap(), Some(edge(1, 1, 2, "link")));
    }
}

#[test]
fn overlay_pages_merge_only_own_changes_and_rollback_conflicts() {
    for ((read_catalog, read_backend), (write_catalog, write_backend)) in
        fixtures().into_iter().zip(fixtures())
    {
        let mut read = PersistentGraphStore::from_catalog(read_catalog, read_backend);
        let mut write = PersistentGraphStore::from_catalog(write_catalog, write_backend);
        for store in [&mut read, &mut write] {
            store
                .transaction(|store| {
                    store.create_graph("items")?;
                    for id in 1..=520 {
                        store.add_vertex(vertex(id, "Original"), "items")?;
                    }
                    store.add_edge(edge(1, 1, 2, "link"), "items")?;
                    Ok(())
                })
                .unwrap();
        }
        write
            .add_vertex(vertex(600, "Concurrent"), "items")
            .unwrap();
        write.add_vertex(vertex(2, "Concurrent"), "items").unwrap();
        let mut overlay = write.with_read_snapshot(&read);
        overlay.add_vertex(vertex(900, "Own"), "items").unwrap();
        overlay.add_vertex(vertex(300, "Own"), "items").unwrap();
        overlay.remove_vertex(256, "items").unwrap();
        overlay.add_edge(edge(2, 1, 900, "link"), "items").unwrap();
        let expected: BTreeSet<_> = (1..=520).filter(|id| *id != 256).chain([900]).collect();
        let mut all = BTreeSet::new();
        let mut after = None;
        loop {
            let page = overlay.vertex_id_page("items", after, 37).unwrap();
            if page.is_empty() {
                break;
            }
            assert!(page.len() <= 37);
            after = page.last().copied();
            all.extend(page);
        }
        assert_eq!(all, expected);
        assert_eq!(
            overlay.vertex_ids_by_label("Own", "items").unwrap(),
            vec![300, 900]
        );
        assert!(overlay
            .vertex_ids_by_label("Concurrent", "items")
            .unwrap()
            .is_empty());
        assert_eq!(
            overlay
                .neighbors(1, Some("link"), Direction::Out, "items")
                .unwrap(),
            vec![2, 900]
        );
        let error = overlay
            .transaction(|store| {
                store.add_vertex(vertex(950, "RolledBack"), "items")?;
                store.add_vertex(vertex(2, "Conflicting"), "items")
            })
            .unwrap_err();
        assert!(
            matches!(error, GraphStoreError::SerializationFailure(_)),
            "{error}"
        );
        assert_eq!(overlay.vertex_ids_in_graph("items").unwrap(), expected);
        assert!(write.get_vertex(950).unwrap().is_none());
    }
}

#[test]
fn durable_path_index_pages_invalidate_with_shared_graph_writes_and_rollback() {
    let sequences = vec![vec!["link".to_owned()]];
    let definition = serde_json::to_string(&sequences).unwrap();
    let sequence = serde_json::to_string(&sequences[0]).unwrap();
    for (catalog, backend) in fixtures() {
        let mut graph =
            PersistentGraphStore::from_catalog(Arc::clone(&catalog), Arc::clone(&backend));
        graph.create_graph("first").unwrap();
        for id in 1..=3 {
            graph.add_vertex(vertex(id, "Item"), "first").unwrap();
        }
        graph.add_edge(edge(1, 1, 2, "link"), "first").unwrap();
        graph.add_edge(edge(2, 2, 3, "link"), "first").unwrap();
        graph.copy_graph("first", "second").unwrap();
        let first = uqa_graph::PathIndex::build_persistent(
            Arc::clone(&catalog),
            Arc::clone(&backend),
            "first::paths",
            "first",
            &sequences,
        )
        .unwrap();
        let second = uqa_graph::PathIndex::build_persistent(
            Arc::clone(&catalog),
            Arc::clone(&backend),
            "second::paths",
            "second",
            &sequences,
        )
        .unwrap();
        assert!(catalog
            .path_index_data_is_current("first::paths", &definition)
            .unwrap());
        assert_eq!(
            catalog
                .path_index_pairs("first::paths", &sequence, None, 1)
                .unwrap(),
            vec![(1, 2)]
        );
        assert_eq!(
            catalog
                .path_index_pairs("first::paths", &sequence, Some((1, 2)), 1)
                .unwrap(),
            vec![(2, 3)]
        );
        assert!(catalog
            .path_index_pairs("first::paths", &sequence, None, 0)
            .is_err());
        assert_eq!(
            first.lookup(&sequences[0]).unwrap().unwrap(),
            BTreeSet::from([(1, 2), (2, 3)])
        );

        backend.begin_transaction().unwrap();
        catalog.save_edge(1, 1, 3, "link", "{}").unwrap();
        for key in ["first::paths", "second::paths"] {
            assert!(!catalog
                .path_index_data_is_current(key, &definition)
                .unwrap());
        }
        assert_eq!(
            first.lookup(&sequences[0]).unwrap().unwrap(),
            BTreeSet::from([(1, 3), (2, 3)])
        );
        backend.rollback_transaction().unwrap();
        assert!(catalog
            .path_index_data_is_current("first::paths", &definition)
            .unwrap());
        assert_eq!(
            second.lookup(&sequences[0]).unwrap().unwrap(),
            BTreeSet::from([(1, 2), (2, 3)])
        );

        catalog.save_edge(1, 1, 3, "link", "{}").unwrap();
        backend.begin_read_transaction().unwrap();
        let reopened = uqa_graph::PathIndex::open_persistent(
            Arc::clone(&catalog),
            Arc::clone(&backend),
            "first::paths",
            "first",
            &sequences,
        )
        .unwrap();
        assert_eq!(
            reopened.lookup(&sequences[0]).unwrap().unwrap(),
            BTreeSet::from([(1, 3), (2, 3)])
        );
        assert!(
            !backend.transaction_has_written().unwrap(),
            "querying an invalid index must not write or rebuild it"
        );
        backend.rollback_transaction().unwrap();
        catalog.drop_path_index("first::paths").unwrap();
        assert!(first.lookup(&sequences[0]).is_err());
        assert!(catalog
            .path_index_pairs("first::paths", &sequence, None, 256)
            .unwrap()
            .is_empty());
        assert!(second.lookup(&sequences[0]).unwrap().is_some());
    }
}

#[test]
fn failed_durable_path_index_rebuild_restores_previous_pages_and_definition() {
    let old_sequences = vec![vec!["link".to_owned()]];
    let old_definition = serde_json::to_string(&old_sequences).unwrap();
    let old_sequence = serde_json::to_string(&old_sequences[0]).unwrap();
    for (catalog, backend) in fixtures() {
        let mut graph =
            PersistentGraphStore::from_catalog(Arc::clone(&catalog), Arc::clone(&backend));
        graph.create_graph("items").unwrap();
        graph
            .transaction(|graph| {
                for id in 1..=260 {
                    graph.add_vertex(vertex(id, "Item"), "items")?;
                }
                for id in 1..260 {
                    graph.add_edge(edge(id, id, id + 1, "link"), "items")?;
                }
                Ok(())
            })
            .unwrap();
        uqa_graph::PathIndex::build_persistent(
            Arc::clone(&catalog),
            Arc::clone(&backend),
            "items::paths",
            "items",
            &old_sequences,
        )
        .unwrap();
        catalog.save_vertex(260, "Item", "corrupt-json").unwrap();
        // The empty sequence writes 256 valid pairs before reaching the bad
        // endpoint in the next sequence. Failure must roll back every page.
        let new_sequences = vec![Vec::new(), vec!["link".to_owned()]];
        assert!(uqa_graph::PathIndex::build_persistent(
            Arc::clone(&catalog),
            Arc::clone(&backend),
            "items::paths",
            "items",
            &new_sequences
        )
        .is_err());
        assert_eq!(
            catalog.load_path_indexes().unwrap(),
            vec![("items::paths".into(), old_definition.clone())]
        );
        assert_eq!(
            catalog
                .path_index_pairs("items::paths", &old_sequence, None, 4096)
                .unwrap()
                .len(),
            259
        );
        assert!(catalog
            .path_index_pairs("items::paths", "[]", None, 4096)
            .unwrap()
            .is_empty());
        assert!(!backend.in_transaction());
    }
}

#[test]
fn clear_releases_orphan_records_without_loading_their_payloads() {
    for (catalog, backend) in fixtures() {
        let mut graph = PersistentGraphStore::from_catalog(Arc::clone(&catalog), backend);
        graph.create_graph("items").unwrap();
        graph.add_vertex(vertex(1, "Item"), "items").unwrap();
        catalog.save_vertex(99, "Orphan", "corrupt-json").unwrap();
        catalog
            .save_edge(99, 99, 99, "Orphan", "corrupt-json")
            .unwrap();
        graph.clear().unwrap();
        assert!(catalog.graph_vertex(99).unwrap().is_none());
        assert!(catalog.graph_edge(99).unwrap().is_none());
        assert_eq!(graph.next_vertex_id().unwrap(), 1);
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uqa_core::{Edge, Value, Vertex};
use uqa_graph::{Direction, GraphStore, GraphStoreError, GraphStoreResult, PersistentGraphStore};
use uqa_storage::{
    CatalogFacade, GraphEntityFilter, GraphEntityKind, KeyValueCatalog, KeyValueStorageBackend,
    KeyValueStore, MemoryKeyValueStore, PersistentStorageBackend, StorageSavepointId,
};
use uqa_storage_sqlite::{ManagedConnection, SQLiteStorageBackend};

fn fixtures() -> Vec<(Arc<dyn CatalogFacade>, Arc<dyn PersistentStorageBackend>)> {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let sqlite: Arc<dyn CatalogFacade> =
        Arc::new(uqa_storage_sqlite::Catalog::open(connection.clone()).unwrap());
    let sqlite_backend: Arc<dyn PersistentStorageBackend> =
        Arc::new(SQLiteStorageBackend::new(connection));
    let keys: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let kv: Arc<dyn CatalogFacade> = Arc::new(KeyValueCatalog::new(Arc::clone(&keys)));
    kv.migrate_relation_namespace().unwrap();
    vec![
        (sqlite, sqlite_backend),
        (kv, Arc::new(KeyValueStorageBackend::new(keys))),
    ]
}

fn vertex(id: u64, label: &str) -> Vertex {
    Vertex {
        vertex_id: id,
        label: label.into(),
        properties: BTreeMap::from([("value".into(), Value::Int(7))]),
    }
}

fn edge(id: u64, source: u64, target: u64, label: &str) -> Edge {
    Edge {
        edge_id: id,
        source_id: source,
        target_id: target,
        label: label.into(),
        properties: BTreeMap::new(),
    }
}

#[test]
fn construction_clone_and_point_reads_do_not_hydrate_unrelated_entities() {
    for (catalog, backend) in fixtures() {
        let mut graph =
            PersistentGraphStore::from_catalog(Arc::clone(&catalog), Arc::clone(&backend));
        graph.create_graph("test_graph").unwrap();
        graph.add_vertex(vertex(1, "Item"), "test_graph").unwrap();
        graph.add_vertex(vertex(2, "Item"), "test_graph").unwrap();
        graph.add_edge(edge(1, 1, 2, "next"), "test_graph").unwrap();
        // An unrelated corrupt payload must not be fetched by construction,
        // handle cloning, membership checks, or a one-hop query elsewhere.
        catalog
            .save_vertex(99, "Unrelated", "invalid-json")
            .unwrap();
        catalog
            .save_graph_membership("vertex", 99, "test_graph")
            .unwrap();
        catalog
            .save_edge(99, 1, 2, "Unrelated", "invalid-json")
            .unwrap();
        catalog
            .save_graph_membership("edge", 99, "test_graph")
            .unwrap();
        let reopened =
            PersistentGraphStore::from_catalog(Arc::clone(&catalog), Arc::clone(&backend));
        let clone = reopened.clone();
        assert_eq!(clone.get_vertex(1).unwrap().unwrap().label, "Item");
        assert_eq!(
            clone
                .neighbors(1, Some("next"), Direction::Out, "test_graph")
                .unwrap(),
            vec![2]
        );
        assert!(clone.get_vertex(99).is_err());
        assert_eq!(
            clone.edges_by_label("next", "test_graph").unwrap(),
            vec![edge(1, 1, 2, "next")]
        );
        assert!(clone.edges_by_label("Unrelated", "test_graph").is_err());
        catalog.save_vertex(1, "Changed", "{}").unwrap();
        assert_eq!(graph.get_vertex(1).unwrap().unwrap().label, "Changed");
        assert_eq!(clone.get_vertex(1).unwrap().unwrap().label, "Changed");
    }
}

#[test]
fn indexed_adjacency_and_membership_follow_updates_and_shared_graph_removal() {
    for (catalog, backend) in fixtures() {
        let mut graph = PersistentGraphStore::from_catalog(catalog, backend);
        graph.create_graph("first").unwrap();
        for id in 1..=3 {
            graph.add_vertex(vertex(id, "Item"), "first").unwrap();
        }
        graph.add_edge(edge(5, 1, 2, "old"), "first").unwrap();
        graph.copy_graph("first", "second").unwrap();
        graph.add_edge(edge(5, 3, 1, "new"), "first").unwrap();
        assert!(graph.out_edge_ids(1, "first").unwrap().is_empty());
        assert!(graph.edge_ids_by_label("old", "first").unwrap().is_empty());
        assert_eq!(
            graph
                .neighbors(3, Some("new"), Direction::Out, "second")
                .unwrap(),
            vec![1]
        );
        graph.drop_graph("first").unwrap();
        assert!(graph.get_vertex(1).unwrap().is_some());
        assert_eq!(
            graph.vertex_graphs(1).unwrap(),
            BTreeSet::from(["second".into()])
        );
        graph.remove_vertex(1, "second").unwrap();
        assert!(graph.get_edge(5).unwrap().is_none());
        graph.drop_graph("second").unwrap();
        assert!(graph.vertices().unwrap().is_empty());
        assert!(graph.edges().unwrap().is_empty());
    }
}

#[test]
fn bounded_identity_pages_apply_every_filter_before_the_limit() {
    for (catalog, backend) in fixtures() {
        let mut graph = PersistentGraphStore::from_catalog(Arc::clone(&catalog), backend);
        graph.create_graph("selected").unwrap();
        graph.create_graph("unrelated").unwrap();
        for id in 1..=20 {
            graph
                .add_vertex(
                    vertex(id, if id % 2 == 0 { "Even" } else { "Odd" }),
                    if id % 3 == 0 { "selected" } else { "unrelated" },
                )
                .unwrap();
        }
        let mut filter = GraphEntityFilter::new(GraphEntityKind::Vertex, Some("selected"));
        filter.label = Some("Even");
        assert_eq!(
            catalog.graph_entity_ids(filter, None, 2).unwrap(),
            vec![6, 12]
        );
        assert_eq!(
            catalog.graph_entity_ids(filter, Some(12), 2).unwrap(),
            vec![18]
        );
        assert_eq!(catalog.graph_entity_count(filter).unwrap(), 3);
        assert!(catalog.graph_entity_ids(filter, None, 0).is_err());
        assert!(catalog
            .graph_entity_ids(filter, None, uqa_storage::MAX_GRAPH_ID_PAGE + 1)
            .is_err());
    }
}

#[test]
fn failed_delta_and_panic_roll_back_without_a_graph_snapshot() {
    for (catalog, backend) in fixtures() {
        let mut graph = PersistentGraphStore::from_catalog(catalog, Arc::clone(&backend));
        graph.create_graph("test_graph").unwrap();
        graph.add_vertex(vertex(1, "Item"), "test_graph").unwrap();
        let mut delta = uqa_graph::GraphDelta::new();
        delta.add_vertex(vertex(2, "Item"));
        delta.add_edge(edge(1, 2, 999, "invalid"));
        {
            let mut versioned = uqa_graph::VersionedGraphStore::new(&mut graph, "test_graph");
            assert!(versioned.apply(delta).is_err());
            assert_eq!(versioned.version(), 0);
        }
        assert!(graph.get_vertex(2).unwrap().is_none());
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: GraphStoreResult<()> = graph.transaction(|store| {
                store.add_vertex(vertex(3, "Item"), "test_graph")?;
                panic!("synthetic graph transaction panic")
            });
        }));
        assert!(panic.is_err());
        assert!(graph.get_vertex(3).unwrap().is_none());
        assert!(!backend.in_transaction());
    }
}

#[test]
fn nested_transaction_failure_can_be_caught_without_retaining_inner_writes() {
    for (catalog, backend) in fixtures() {
        let mut graph = PersistentGraphStore::from_catalog(catalog, Arc::clone(&backend));
        graph.create_graph("test_graph").unwrap();
        graph
            .transaction(|outer| {
                outer.add_vertex(vertex(1, "Item"), "test_graph")?;
                let failed: GraphStoreResult<()> = outer.transaction(|inner| {
                    inner.add_vertex(vertex(2, "Item"), "test_graph")?;
                    Err(GraphStoreError::InvalidMutation("synthetic failure".into()))
                });
                assert!(failed.is_err());
                assert!(outer.get_vertex(2)?.is_none());
                let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _: GraphStoreResult<()> = outer.transaction(|inner| {
                        inner.add_vertex(vertex(3, "Item"), "test_graph")?;
                        panic!("synthetic nested transaction panic")
                    });
                }));
                assert!(panic.is_err());
                assert!(outer.get_vertex(3)?.is_none());
                outer.add_vertex(vertex(4, "Item"), "test_graph")
            })
            .unwrap();
        assert_eq!(
            graph.vertex_ids_in_graph("test_graph").unwrap(),
            BTreeSet::from([1, 4])
        );
        assert!(!backend.in_transaction());
    }
}

#[test]
fn explicit_transaction_savepoint_and_read_snapshot_remain_backend_owned() {
    for (catalog, backend) in fixtures() {
        let mut graph =
            PersistentGraphStore::from_catalog(Arc::clone(&catalog), Arc::clone(&backend));
        graph.create_graph("test_graph").unwrap();
        backend.begin_transaction().unwrap();
        graph.add_vertex(vertex(1, "Item"), "test_graph").unwrap();
        let savepoint = StorageSavepointId::allocate();
        backend.savepoint(savepoint).unwrap();
        let result: GraphStoreResult<()> = graph.transaction(|store| {
            store.add_vertex(vertex(2, "Item"), "test_graph")?;
            Err(GraphStoreError::InvalidMutation("synthetic failure".into()))
        });
        assert!(result.is_err());
        assert!(graph.get_vertex(1).unwrap().is_some());
        assert!(graph.get_vertex(2).unwrap().is_none());
        graph.add_vertex(vertex(3, "Item"), "test_graph").unwrap();
        backend.rollback_to_savepoint(savepoint).unwrap();
        backend.release_savepoint(savepoint).unwrap();
        assert!(graph.get_vertex(3).unwrap().is_none());
        backend.commit_transaction().unwrap();
        let reopened = PersistentGraphStore::from_catalog(catalog, Arc::clone(&backend));
        assert!(reopened.get_vertex(1).unwrap().is_some());
        backend.begin_read_transaction().unwrap();
        assert!(reopened.get_vertex(1).unwrap().is_some());
        assert_eq!(
            reopened.vertex_ids_in_graph("test_graph").unwrap(),
            BTreeSet::from([1])
        );
        assert!(!backend.transaction_has_written().unwrap());
        backend.rollback_transaction().unwrap();
        assert!(!backend.in_transaction());
    }
}

#[test]
fn allocated_ids_and_age_label_tombstones_survive_handle_reconstruction() {
    for (catalog, backend) in fixtures() {
        let mut graph =
            PersistentGraphStore::from_catalog(Arc::clone(&catalog), Arc::clone(&backend));
        graph.create_graph("test_graph").unwrap();
        let first = graph.allocate_vertex_id("Item", "test_graph").unwrap();
        let second = graph.allocate_vertex_id("Item", "test_graph").unwrap();
        graph
            .add_vertex(vertex(first, "Item"), "test_graph")
            .unwrap();
        graph
            .add_vertex(vertex(second, "Item"), "test_graph")
            .unwrap();
        let edge_id = graph.allocate_edge_id("next", "test_graph").unwrap();
        graph
            .add_edge(edge(edge_id, first, second, "next"), "test_graph")
            .unwrap();
        let mut reopened = PersistentGraphStore::from_catalog(catalog, backend);
        assert_eq!(
            reopened.allocate_vertex_id("Item", "test_graph").unwrap(),
            second + 1
        );
        reopened.drop_label("test_graph", "Item").unwrap();
        assert!(reopened.vertices_in_graph("test_graph").unwrap().is_empty());
        assert_eq!(reopened.edges_in_graph("test_graph").unwrap().len(), 1);
        assert_eq!(graph.edges_in_graph("test_graph").unwrap().len(), 1);
    }
}
