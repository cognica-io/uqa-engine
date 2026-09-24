//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph selectors and lifetime keys participate in real commit histories, including absent intervals.

use super::*;
use uqa_storage::catalog::graph_identifiers::GraphIdentifierNamespace;
use uqa_storage::catalog::graph_observations::{
    scope_lifetime, GraphEntityTopology, GraphMembershipKey, GraphSelectionKey,
};
use uqa_storage::{GraphEntityFilter, GraphEntityKind, KeyValueBatch, StorageBackendResult};

fn history(
    predicate: SerializablePredicate<'_>,
    write_graph: impl Fn(&mut dyn KeyValueBatch) -> StorageBackendResult<()>,
    conflict: bool,
) {
    let persistence = Persistence::new();
    let (a, ra) = start(&persistence, false);
    let (b, rb) = start(&persistence, false);
    ra.observe_read(predicate, &StorageReadControl::with_limit(1 << 20))
        .unwrap();
    read(&rb, b"pivot");
    write(&a, b"pivot");
    b.with_mutation(&mut |_, batch| {
        write_graph(batch)?;
        batch.put(b"evaluated graph change", b"changed")
    })
    .unwrap();
    a.commit_transaction().unwrap();
    if conflict {
        b.commit_transaction().unwrap_err();
        b.rollback_transaction().unwrap();
    } else {
        b.commit_transaction().unwrap();
    }
}

#[test]
fn graph_selector_conjunctions_match_old_and_new_topology_without_crossing_other_filters() {
    let namespace = GraphIdentifierNamespace::new(None, [0; 16]);
    for mask in 0..8 {
        let mut filter = GraphEntityFilter::new(GraphEntityKind::Edge, Some("g"));
        filter.label = (mask & 1 != 0).then_some("knows");
        filter.source = (mask & 2 != 0).then_some(1);
        filter.target = (mask & 4 != 0).then_some(2);
        let selected = GraphSelectionKey::new(namespace, filter, None).unwrap();
        for (graph, label, source, target, conflict) in [
            ("g", "knows", 1, 2, true),
            ("other", "knows", 1, 2, false),
            ("g", "other", 1, 2, mask & 1 == 0),
            ("g", "knows", 3, 2, mask & 2 == 0),
            ("g", "knows", 1, 3, mask & 4 == 0),
        ] {
            history(
                selected.predicate(),
                |batch| {
                    GraphEntityTopology::Edge {
                        label,
                        source,
                        target,
                    }
                    .observe_write(namespace, 10, Some(graph), batch)
                },
                conflict,
            );
        }
    }
    let mut filter = GraphEntityFilter::new(GraphEntityKind::Vertex, None);
    filter.label = Some("");
    for (id, label, conflict) in [(10, "", false), (11, "", true), (11, "other", false)] {
        let selected = GraphSelectionKey::new(namespace, filter, Some(10)).unwrap();
        history(
            selected.predicate(),
            |batch| GraphEntityTopology::Vertex { label }.observe_write(namespace, id, None, batch),
            conflict,
        );
    }
}

#[test]
fn graph_membership_points_and_ranges_keep_graph_kind_and_entity_identity() {
    let namespace = GraphIdentifierNamespace::new(None, [0; 16]);
    for graph in [None, Some(""), Some("g")] {
        let selected = GraphMembershipKey::new(namespace, GraphEntityKind::Vertex, 1, graph);
        for (kind, id, target) in [
            (GraphEntityKind::Vertex, 1, ""),
            (GraphEntityKind::Vertex, 1, "g"),
            (GraphEntityKind::Vertex, 2, "g"),
            (GraphEntityKind::Edge, 1, "g"),
        ] {
            let written = GraphMembershipKey::new(namespace, kind, id, Some(target));
            history(
                selected.predicate(),
                |batch| batch.observe_serializable_write(written.predicate()),
                kind == GraphEntityKind::Vertex
                    && id == 1
                    && graph.is_none_or(|graph| graph == target),
            );
        }
    }
}

#[test]
fn graph_clear_observations_span_generations_without_crossing_physical_scopes() {
    let original = GraphIdentifierNamespace::new(None, [1; 16]);
    for (scope, generation, conflict) in [
        (None, [2; 16], true),
        (Some(""), [1; 16], false),
        (Some("other"), [2; 16], false),
    ] {
        let replaced = GraphIdentifierNamespace::new(scope, generation);
        history(
            scope_lifetime(original),
            |batch| batch.observe_serializable_write(scope_lifetime(replaced)),
            conflict,
        );
    }
}
