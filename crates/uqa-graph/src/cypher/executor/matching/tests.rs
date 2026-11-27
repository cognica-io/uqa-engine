//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::cypher::{executor::PatternCache, parse_cypher, CypherClause};
use crate::MemoryGraphStore;
use std::collections::BTreeMap;

#[test]
fn bound_start_vertex_does_not_enumerate_unrelated_vertices() {
    let query = parse_cypher("MATCH (a) RETURN a").unwrap();
    let CypherClause::Match(clause) = &query.clauses[0] else {
        panic!("expected MATCH");
    };
    let vertex = Vertex::new(1, "item");
    let seed = BindingRow::from([("a".into(), Binding::Vertex(vertex.clone()))]);
    let mut store = MemoryGraphStore::new();
    store.create_graph("g").unwrap();
    store.add_vertex(vertex, "g").unwrap();
    let params = BTreeMap::new();
    let patterns = PatternCache::default();
    let measure = |store: &MemoryGraphStore| {
        let reader = StatementReader::new(store, "g", &params, &patterns);
        allocation_counter::measure(|| {
            let rows = reader
                .match_path_pattern(&clause.patterns[0], &seed)
                .unwrap();
            assert_eq!(rows.len(), 1);
            assert!(matches!(&rows[0]["a"], Binding::Vertex(v) if v.vertex_id == 1));
        })
    };
    let small = measure(&store);
    for id in 2..=4096 {
        store.add_vertex(Vertex::new(id, "item"), "g").unwrap();
    }
    let large = measure(&store);
    assert_eq!(small.count_total, large.count_total);
    assert_eq!(small.bytes_total, large.bytes_total);
}

#[test]
fn unnamed_trails_validate_identifiers_in_entity_order_without_copying_properties() {
    let mut vertex = Vertex::new(3, "item");
    vertex
        .properties
        .insert("payload".into(), Value::Str("x".repeat(64 * 1024)));
    let mut edge = Edge::new(1, 2, 3, "link");
    edge.properties.clone_from(&vertex.properties);
    let mut trail = Vec::new();
    let allocated = allocation_counter::measure(|| {
        push_trail_vertex(&mut trail, &vertex, false).unwrap();
        push_trail_edge(&mut trail, &edge, &vertex, false).unwrap();
    });
    assert!(trail.is_empty());
    assert_eq!(allocated.count_total, 0);
    push_trail_edge(&mut trail, &edge, &vertex, true).unwrap();
    assert_eq!(
        trail,
        [
            agtype::edge_to_value(&edge).unwrap(),
            agtype::vertex_to_value(&vertex).unwrap()
        ]
    );

    let invalid = i64::MAX as u64 + 1;
    vertex.vertex_id = invalid + 3;
    for (id, from, to, expected) in [
        (invalid, invalid + 1, invalid + 2, invalid),
        (1, invalid + 1, invalid + 2, invalid + 1),
        (1, 2, invalid + 2, invalid + 2),
        (1, 2, 3, invalid + 3),
    ] {
        let edge = Edge::new(id, from, to, "link");
        let error =
            CypherError::from(agtype::AgtypeConversionError::GraphIdOutOfRange { id: expected });
        for needed in [false, true] {
            assert_eq!(
                push_trail_edge(&mut Vec::new(), &edge, &vertex, needed).unwrap_err(),
                error
            );
        }
    }
}
