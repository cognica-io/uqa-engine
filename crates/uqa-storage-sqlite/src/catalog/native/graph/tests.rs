//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reads release physical I/O between bounded pages while retaining one logical view.

use std::{sync::mpsc, time::Duration};

use uqa_storage::mvcc::VersionedSessionOptions;

use super::*;
use crate::{Catalog, ManagedConnection};

fn connection(vertices: i64) -> ManagedConnection {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .with(|connection| {
            connection.execute("INSERT INTO _named_graphs VALUES ('g')", [])?;
            connection.execute(
                "WITH RECURSIVE ids(id) AS (VALUES (1) UNION ALL SELECT id + 1 FROM ids WHERE id < ?1) INSERT INTO _graph_vertices SELECT id, 'node', '{}' FROM ids",
                [vertices],
            )?;
            connection.execute(
                "INSERT INTO _graph_membership SELECT 'vertex', vertex_id, 'g' FROM _graph_vertices",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    connection
}

#[test]
fn selection_releases_physical_io_and_keeps_predicates_and_payloads_on_one_snapshot() {
    let connection = connection(2);
    let catalog = Catalog::open(connection.clone()).unwrap();
    let snapshot = connection.native_snapshot().unwrap().unwrap();
    let other = connection.new_session();
    let (start, ready) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        ready.recv().unwrap();
        let result = other.with_native_write(|snapshot, batch| {
            let id = ValueRef::Integer(2);
            snapshot.put_row(
                batch,
                Family::GraphVertices,
                owner(snapshot),
                &[id, text("changed"), text("new properties")],
            )?;
            snapshot.put_row(
                batch,
                Family::GraphLookups,
                owner(snapshot),
                &[
                    text("label"),
                    text("changed"),
                    ValueRef::Integer(0),
                    text("vertex"),
                    id,
                ],
            )?;
            for (family, key) in [
                (
                    Family::GraphLookups,
                    vec![
                        text("label"),
                        text("node"),
                        ValueRef::Integer(0),
                        text("vertex"),
                        id,
                    ],
                ),
                (
                    Family::GraphLookups,
                    vec![
                        text("member"),
                        text("g"),
                        ValueRef::Integer(0),
                        text("vertex"),
                        id,
                    ],
                ),
                (Family::GraphMembership, vec![text("vertex"), id, text("g")]),
            ] {
                snapshot.delete_prefix(batch, family, owner(snapshot), &key)?;
            }
            Ok(())
        });
        finished.send(result).unwrap();
    });
    let mut filter = GraphEntityFilter::new(GraphEntityKind::Vertex, Some("g"));
    filter.label = Some("node");
    let mut seen = Vec::new();
    selection::visit(&snapshot, filter, None, |id| {
        if id == 1 {
            start.send(()).unwrap();
            completion
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap()
                .unwrap();
        }
        let row = vertex(&snapshot, id as u64)?.unwrap();
        seen.push((row.vertex_id, row.label, row.properties_json));
        Ok(true)
    })
    .unwrap();
    writer.join().unwrap();
    assert_eq!(
        seen,
        [
            (1, "node".into(), "{}".into()),
            (2, "node".into(), "{}".into())
        ]
    );
    assert_eq!(catalog.graph_entity_ids(filter, None, 8).unwrap(), [1]);
    let current = catalog.graph_vertex(2).unwrap().unwrap();
    assert_eq!(
        (current.label.as_str(), current.properties_json.as_str()),
        ("changed", "new properties")
    );
    assert!(!catalog
        .graph_has_membership(GraphEntityKind::Vertex, 2, "g")
        .unwrap());
}

#[test]
fn graph_pages_advance_over_tombstones_and_apply_secondary_filters_before_the_limit() {
    let connection = connection(600);
    let catalog = Catalog::open(connection.clone()).unwrap();
    let retained = connection.native_snapshot().unwrap().unwrap();
    connection.begin_transaction().unwrap();
    connection
        .with_native_write(|snapshot, batch| {
            for id in 1..=300 {
                let id = ValueRef::Integer(id);
                snapshot.delete_prefix(
                    batch,
                    Family::GraphMembership,
                    owner(snapshot),
                    &[text("vertex"), id, text("g")],
                )?;
                snapshot.delete_prefix(
                    batch,
                    Family::GraphLookups,
                    owner(snapshot),
                    &[
                        text("member"),
                        text("g"),
                        ValueRef::Integer(0),
                        text("vertex"),
                        id,
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap()
        .unwrap();
    for committed in [false, true] {
        if committed {
            connection.commit_transaction().unwrap();
        }
        let mut filter = GraphEntityFilter::new(GraphEntityKind::Vertex, Some("g"));
        for label in [None, Some("node")] {
            filter.label = label;
            assert_eq!(
                catalog.graph_entity_ids(filter, None, 3).unwrap(),
                [301, 302, 303]
            );
            assert_eq!(
                catalog.graph_entity_ids(filter, Some(599), 3).unwrap(),
                [600]
            );
            assert_eq!(catalog.graph_entity_count(filter).unwrap(), 300);
            assert_eq!(count(&retained, filter).unwrap(), 600);
        }
        let graph = catalog.load_named_graph_snapshot("g").unwrap().unwrap();
        assert_eq!(graph.vertices.len(), 300);
        assert_eq!(graph.vertices[0].vertex_id, 301);
        assert_eq!(graph.vertices.last().unwrap().vertex_id, 600);
    }
}
