//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{Edge, Predicate, Value};
use uqa_operators::EdgePatternIR;

fn edge(source: &str, target: &str, label: &str) -> EdgePatternIR {
    EdgePatternIR {
        source_var: source.into(),
        target_var: target.into(),
        label: Some(label.into()),
        constraints: Vec::new(),
    }
}

#[test]
fn graph_edge_filter_preserves_buffers_and_shares_the_selected_predicate() {
    let edges = vec![
        edge("a_b", "c", "selected"),
        edge("a_b", "c", "other"),
        edge("a", "b_c", "selected"),
        edge("a_b", "c", "selected"),
    ];
    let buffer = edges.as_ptr();
    let graph = "graph".repeat(4096);
    let name = graph.as_ptr();
    let plan = OperatorTree::Filter {
        field: "a_b_c.a.b".into(),
        predicate: Predicate::Equals(Value::Int(2)),
        source: Some(Box::new(OperatorTree::PatternMatch {
            pattern: GraphPatternIR {
                vertex_patterns: Vec::new(),
                edge_patterns: edges,
            },
            graph,
        })),
    };
    let OperatorTree::PatternMatch { pattern, graph } =
        QueryOptimizer::new().push_graph_pattern_filters(plan)
    else {
        panic!("pushed predicate");
    };
    assert_eq!(graph.as_ptr(), name);
    assert_eq!(pattern.edge_patterns.as_ptr(), buffer);
    let edges = pattern.edge_patterns;
    assert!(edges[1].constraints.is_empty() && edges[2].constraints.is_empty());
    assert!(Arc::ptr_eq(
        &edges[0].constraints[0],
        &edges[3].constraints[0]
    ));
    let check = &edges[0].constraints[0];
    let mut example = Edge::new(1, 2, 3, "selected");
    assert!(!check(&example));
    example.properties.insert("a.b".into(), Value::Int(2));
    assert!(check(&example));
}

#[test]
fn fused_patterns_keep_declaration_and_constraint_order_and_move_edge_storage() {
    let vertex = |name: &str, label: &str, allowed| VertexPatternIR {
        variable: name.into(),
        label: Some(label.into()),
        constraints: vec![Arc::new(move |_| allowed)],
    };
    let retained_edge = edge("x", "new", &"label".repeat(4096));
    let label = retained_edge.label.as_ref().unwrap().as_ptr();
    let mut left = GraphPatternIR {
        vertex_patterns: vec![
            vertex("x", "first", false),
            vertex("y", "middle", false),
            vertex("x", "last", true),
        ],
        edge_patterns: vec![edge("x", "y", "left")],
    };
    let right = GraphPatternIR {
        vertex_patterns: vec![
            vertex("new", "new", false),
            vertex("x", "ignored", false),
            vertex("x", "ignored", true),
        ],
        edge_patterns: vec![retained_edge],
    };
    merge_patterns(&mut left, right).expect("a shared vertex");
    assert_eq!(
        left.vertex_patterns
            .iter()
            .map(|v| v.variable.as_str())
            .collect::<Vec<_>>(),
        ["x", "y", "new"]
    );
    let x = &left.vertex_patterns[0];
    assert_eq!(x.label.as_deref(), Some("last"));
    let example = uqa_core::Vertex::new(1, "");
    assert_eq!(
        x.constraints
            .iter()
            .map(|check| check(&example))
            .collect::<Vec<_>>(),
        [true, false, true]
    );
    assert_eq!(
        left.edge_patterns[1].label.as_ref().unwrap().as_ptr(),
        label
    );
}
