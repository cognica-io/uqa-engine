//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Real native and byte-store graph allocation schedules over independent sessions.

use std::{path::Path, sync::Arc};

use super::{open, Mode, MODES};
use uqa_core::Vertex;
use uqa_graph::{graphid_label_id, graphid_sequence, GraphStore, LabelKind, PersistentGraphStore};
use uqa_storage::{
    mvcc::VersionedSessionOptions, KeyValueCatalog, KeyValueStorageBackend, KeyValueStore,
    PersistentStorageSession,
};
use uqa_storage_sqlite::{Catalog, SQLiteKeyValueStore, SQLiteStorageBackend};

fn session(mode: Mode, path: &Path, native: bool, initialize: bool) -> PersistentStorageSession {
    let connection = open(mode, path);
    if native {
        if initialize {
            Catalog::open(connection.clone()).unwrap();
        }
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        PersistentStorageSession::new(
            Arc::new(Catalog::open(connection.clone()).unwrap()),
            Arc::new(SQLiteStorageBackend::new(connection)),
        )
    } else {
        let store: Arc<dyn KeyValueStore> = Arc::new(SQLiteKeyValueStore::new(connection).unwrap());
        PersistentStorageSession::new(
            Arc::new(KeyValueCatalog::new(store.clone())),
            Arc::new(KeyValueStorageBackend::new(store)),
        )
    }
}

fn graph(session: &PersistentStorageSession) -> PersistentGraphStore {
    PersistentGraphStore::from_catalog(session.catalog.clone(), session.backend.clone())
}

fn verify_graph_export(
    source: &PersistentStorageSession,
    copied: &PersistentStorageSession,
    renamed: u64,
    edge: u64,
) {
    let exported = source
        .catalog
        .load_named_graph_snapshot("renamed")
        .unwrap()
        .unwrap();
    let exported_registry: uqa_graph::GraphLabelRegistry =
        serde_json::from_str(&exported.label_registry_json).unwrap();
    assert!(
        exported_registry.sequences[&graphid_label_id(renamed)] >= graphid_sequence(renamed),
        "graph export lost autonomous identifier reservations"
    );
    assert_eq!(
        exported_registry.sequences[&graphid_label_id(edge)],
        graphid_sequence(edge)
    );
    copied
        .catalog
        .replace_named_graph("restored", &exported)
        .unwrap();
    let mut restored_export = graph(copied);
    assert!(
        restored_export
            .allocate_vertex_id("item", "restored")
            .unwrap()
            > renamed
    );
    assert!(
        restored_export
            .allocate_edge_id("link", "restored")
            .unwrap()
            > edge
    );
}

#[test]
fn graph_identifier_reservations_cover_shared_entities_undo_rename_clear_and_reopen() {
    for mode in MODES {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("graph-identifiers.db");
            let a = session(mode, &path, native, true);
            let b = session(mode, &path, native, false);
            let mut left = graph(&a);
            let mut right = graph(&b);
            for name in ["g", "other"] {
                left.create_graph(name).unwrap();
                left.create_label(name, "item", LabelKind::Vertex).unwrap();
            }
            let first = left.allocate_vertex_id("item", "g").unwrap();
            let other = right.allocate_vertex_id("item", "other").unwrap();
            assert_eq!(graphid_label_id(first), graphid_label_id(other));
            assert_ne!(first, other);
            left.add_vertex(Vertex::new(first, "item"), "g").unwrap();
            right
                .add_vertex(Vertex::new(other, "item"), "other")
                .unwrap();
            assert_eq!(left.vertices_in_graph("g").unwrap().len(), 1);
            assert_eq!(right.vertices_in_graph("other").unwrap().len(), 1);
            a.backend.begin_transaction().unwrap();
            let undone = left.allocate_vertex_id("item", "g").unwrap();
            let label = left
                .create_label("g", "undone", LabelKind::Vertex)
                .unwrap()
                .unwrap();
            a.backend.rollback_transaction().unwrap();
            assert!(right.allocate_vertex_id("item", "g").unwrap() > undone);
            assert!(
                right
                    .create_label("g", "new", LabelKind::Vertex)
                    .unwrap()
                    .unwrap()
                    > label
            );
            let plain = left.next_vertex_id().unwrap();
            let after_plain = right.allocate_vertex_id("item", "g").unwrap();
            assert!(after_plain > plain);
            let identity = left.label_registry("g").unwrap().allocation_id;
            left.rename_graph("g", "renamed").unwrap();
            assert_eq!(
                left.label_registry("renamed").unwrap().allocation_id,
                identity
            );
            let renamed = right.allocate_vertex_id("item", "renamed").unwrap();
            assert!(renamed > after_plain);
            let edge = right.allocate_edge_id("link", "renamed").unwrap();
            let copied = session(mode, &directory.path().join("restored.db"), native, true);
            verify_graph_export(&a, &copied, renamed, edge);
            left.drop_graph("renamed").unwrap();
            left.create_graph("renamed").unwrap();
            assert_ne!(
                left.label_registry("renamed").unwrap().allocation_id,
                identity
            );
            assert!(left.allocate_vertex_id("item", "renamed").unwrap() > renamed);
            a.backend.begin_transaction().unwrap();
            left.clear().unwrap();
            assert_eq!(left.next_vertex_id().unwrap(), 1);
            a.backend.rollback_transaction().unwrap();
            let last = left.next_vertex_id().unwrap();
            assert!(last > renamed);
            drop((left, right, a, b));
            let reopened = session(mode, &path, native, false);
            let mut restored = graph(&reopened);
            assert!(restored.next_vertex_id().unwrap() > last);
            restored.clear().unwrap();
            assert_eq!(restored.next_vertex_id().unwrap(), 1);
            restored.create_graph("empty").unwrap();
            assert_eq!(
                graphid_sequence(restored.allocate_vertex_id("item", "empty").unwrap()),
                1
            );
        }
    }
}

#[test]
fn graph_identifier_definition_guards_reject_both_commit_orders() {
    for mode in MODES {
        for native in [false, true] {
            for clear in [false, true] {
                for data_wins in [false, true] {
                    let directory = tempfile::tempdir().unwrap();
                    let path = directory.path().join("graph-definition.db");
                    let a = session(mode, &path, native, true);
                    let b = session(mode, &path, native, false);
                    let mut left = graph(&a);
                    let mut right = graph(&b);
                    left.create_graph("g").unwrap();
                    left.create_label("g", "item", LabelKind::Vertex).unwrap();
                    a.backend.begin_transaction().unwrap();
                    b.backend.begin_transaction().unwrap();
                    let id = left.allocate_vertex_id("item", "g").unwrap();
                    left.add_vertex(Vertex::new(id, "item"), "g").unwrap();
                    if clear {
                        right.clear().unwrap();
                    } else {
                        right.drop_label("g", "item").unwrap();
                    }
                    let (winner, loser) = if data_wins { (&a, &b) } else { (&b, &a) };
                    winner.backend.commit_transaction().unwrap();
                    assert!(
                        loser.backend.commit_transaction().is_err(),
                        "incompatible graph definition and data both committed"
                    );
                    loser.backend.rollback_transaction().unwrap();
                    assert_eq!(left.get_vertex(id).unwrap().is_some(), data_wins);
                }
            }
        }
    }
}
