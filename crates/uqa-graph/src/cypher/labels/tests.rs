//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::cypher::{parse_cypher, CypherError};
use crate::{GraphStore as _, GraphStoreHandle};

#[test]
fn nested_patterns_require_labels_even_inside_scalar_expressions() {
    for source in [
        "RETURN coalesce(exists((a)-[r]->(b)), false)",
        "RETURN CASE WHEN true THEN exists((a)-[r]->(b)) ELSE false END",
        "RETURN [false, exists((a)-[r]->(b))][1]",
        "RETURN {matched: exists((a)-[r]->(b))}",
        "RETURN NOT exists((a)-[r]->(b))",
        "RETURN exists((a)-[r]->(b)) IS NOT NULL",
        "RETURN [x IN [true] WHERE exists((a)-[r]->(b)) | x]",
    ] {
        let query = parse_cypher(source).unwrap();
        assert_eq!(label_requirements(&query), (true, true), "{source}");
    }
    for source in ["RETURN 1", "RETURN $value", "UNWIND [1, 2] AS n RETURN n"] {
        assert_eq!(
            label_requirements(&parse_cypher(source).unwrap()),
            (false, false),
            "{source}"
        );
    }
}

#[test]
fn clause_conditions_properties_and_projection_clauses_keep_pattern_requirements() {
    for source in [
        "MATCH (n) WHERE exists((a)-[r]->(b)) RETURN n",
        "CREATE (n {matched: exists((a)-[r]->(b))})",
        "MERGE (n) ON CREATE SET n.matched = exists((a)-[r]->(b))",
        "MERGE (n) ON MATCH SET n.matched = exists((a)-[r]->(b))",
        "WITH 1 AS n WHERE exists((a)-[r]->(b)) RETURN n",
        "RETURN 1 ORDER BY exists((a)-[r]->(b))",
        "UNWIND [exists((a)-[r]->(b))] AS found RETURN found",
    ] {
        assert_eq!(
            label_requirements(&parse_cypher(source).unwrap()),
            (true, true),
            "{source}"
        );
    }
    assert_eq!(
        label_requirements(&parse_cypher("MATCH (n) RETURN n").unwrap()),
        (true, false)
    );
}

#[test]
fn missing_default_labels_keep_vertex_before_edge_diagnostics() {
    for (removed, expected) in [
        (
            vec!["_ag_label_vertex", "_ag_label_edge"],
            "g._ag_label_vertex",
        ),
        (vec!["_ag_label_edge"], "g._ag_label_edge"),
    ] {
        let mut store = GraphStoreHandle::default();
        store.create_graph("g").unwrap();
        for label in removed {
            assert!(store.drop_label("g", label).unwrap().is_some());
        }
        validate_default_label_relations(&store, "g", &parse_cypher("RETURN 1").unwrap()).unwrap();
        let query = parse_cypher("RETURN exists((a)-[r]->(b))").unwrap();
        let error = validate_default_label_relations(&store, "g", &query).unwrap_err();
        assert!(matches!(error, CypherError::MissingLabelRelation(name) if name == expected));
    }
}

#[test]
fn graph_catalog_errors_remain_storage_errors_for_constant_and_pattern_queries() {
    let store = GraphStoreHandle::default();
    let expected = store.graph_labels("missing").unwrap_err().to_string();
    for source in ["RETURN 1", "MATCH (n) RETURN n"] {
        let error =
            validate_default_label_relations(&store, "missing", &parse_cypher(source).unwrap())
                .unwrap_err();
        assert!(matches!(error, CypherError::Storage(message) if message == expected));
    }
}
