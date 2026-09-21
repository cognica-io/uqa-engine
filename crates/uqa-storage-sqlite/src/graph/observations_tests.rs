//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standalone graph scopes use original SSI participants and retain typed cancellation.

use crate::{ManagedConnection, SQLiteGraphStore, SQLiteStorageBackend};
use uqa_core::{Value, Vertex};
use uqa_storage::mvcc::{SerializableKeySpace, SerializablePredicate, VersionedSessionOptions};
use uqa_storage::PersistentStorageBackend;

#[test]
fn standalone_graph_payloads_share_only_their_own_physical_scope() {
    for same_scope in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph-observations.db");
        let first = ManagedConnection::open(&path).unwrap();
        first
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let mut a = SQLiteGraphStore::open(first.clone(), None).unwrap();
        a.create_graph("g").unwrap();
        a.add_vertex(Vertex::new(1, "P"), "g").unwrap();
        let mut other = SQLiteGraphStore::open(first.clone(), Some("other")).unwrap();
        other.create_graph("g").unwrap();
        other.add_vertex(Vertex::new(1, "P"), "g").unwrap();
        let second = ManagedConnection::open(&path).unwrap();
        second
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let mut b =
            SQLiteGraphStore::open(second.clone(), (!same_scope).then_some("other")).unwrap();
        let backend_a = SQLiteStorageBackend::new(first);
        let backend_b = SQLiteStorageBackend::new(second);
        backend_a.begin_transaction().unwrap();
        backend_b.begin_transaction().unwrap();
        let sa = backend_a.serializable_session().unwrap();
        let sb = backend_b.serializable_session().unwrap();
        sa.establish_serializable_snapshot().unwrap();
        let reader = sb.establish_serializable_snapshot().unwrap();
        assert!(a.get_vertex(1).unwrap().is_some());
        let predicate = SerializablePredicate::point([9; 16], SerializableKeySpace::Rows, b"pivot");
        reader
            .observe_read(
                predicate,
                &reader.read_control(&uqa_core::CancellationToken::new()),
            )
            .unwrap();
        sa.observe_serializable_write(predicate).unwrap();
        let mut vertex = Vertex::new(1, "P");
        vertex.properties.insert("value".into(), Value::Int(2));
        b.add_vertex(vertex, "g").unwrap();
        backend_a.commit_transaction().unwrap();
        if same_scope {
            backend_b.commit_transaction().unwrap_err();
            backend_b.rollback_transaction().unwrap();
        } else {
            backend_b.commit_transaction().unwrap();
        }

        backend_a.begin_transaction().unwrap();
        sa.establish_serializable_snapshot().unwrap();
        let cancellation = backend_a.write_cancellation().unwrap();
        cancellation.cancel();
        let error = a.get_vertex(1).unwrap_err();
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&error);
        let mut found = false;
        while let Some(error) = cause {
            found |= matches!(
                error.downcast_ref::<uqa_storage::StorageBackendError>(),
                Some(uqa_storage::StorageBackendError::Cancelled(_))
            );
            cause = error.source();
        }
        assert!(found, "cancellation lost its original type: {error}");
        cancellation.reset();
        assert!(a.get_vertex(1).unwrap().is_some());
        backend_a.rollback_transaction().unwrap();
    }
}

#[test]
fn standalone_graph_selectors_and_algebra_retain_the_original_participant() {
    for route in ["label", "adjacency", "membership", "copy", "clear"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph-selectors.db");
        let first = ManagedConnection::open(&path).unwrap();
        first
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let mut a = SQLiteGraphStore::open(first.clone(), None).unwrap();
        a.create_graph("g").unwrap();
        a.create_graph("empty").unwrap();
        for id in [1, 2] {
            a.add_vertex(Vertex::new(id, "P"), "g").unwrap();
        }
        let second = ManagedConnection::open(&path).unwrap();
        second
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let mut b = SQLiteGraphStore::open(second.clone(), None).unwrap();
        let backend_a = SQLiteStorageBackend::new(first);
        let backend_b = SQLiteStorageBackend::new(second);
        backend_a.begin_transaction().unwrap();
        backend_b.begin_transaction().unwrap();
        let sa = backend_a.serializable_session().unwrap();
        let sb = backend_b.serializable_session().unwrap();
        sa.establish_serializable_snapshot().unwrap();
        let reader = sb.establish_serializable_snapshot().unwrap();
        match route {
            "label" | "clear" => assert!(a.vertex_ids_by_label("Q", "g").unwrap().is_empty()),
            "adjacency" => assert!(a
                .neighbors(1, Some("likes"), uqa_graph::Direction::Out, "g")
                .unwrap()
                .is_empty()),
            "membership" => assert!(a.vertex_graphs(99).unwrap().is_empty()),
            "copy" => a.copy_graph("empty", "copy").unwrap(),
            _ => unreachable!(),
        }
        let predicate = SerializablePredicate::point([9; 16], SerializableKeySpace::Rows, b"pivot");
        reader
            .observe_read(
                predicate,
                &reader.read_control(&uqa_core::CancellationToken::new()),
            )
            .unwrap();
        sa.observe_serializable_write(predicate).unwrap();
        match route {
            "label" | "membership" => b.add_vertex(Vertex::new(99, "Q"), "g").unwrap(),
            "adjacency" => b
                .add_edge(uqa_core::Edge::new(10, 1, 2, "likes"), "g")
                .unwrap(),
            "copy" => b.add_vertex(Vertex::new(99, "Q"), "empty").unwrap(),
            "clear" => {
                b.clear().unwrap();
                b.create_graph("g").unwrap();
                b.add_vertex(Vertex::new(99, "Q"), "g").unwrap();
            }
            _ => unreachable!(),
        }
        backend_a.commit_transaction().unwrap();
        backend_b.commit_transaction().expect_err(route);
        backend_b.rollback_transaction().unwrap();
    }
}
