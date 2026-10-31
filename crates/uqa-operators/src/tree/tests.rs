//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn leaf(value: i64) -> OperatorTree {
    OperatorTree::Filter {
        field: "id".into(),
        predicate: Predicate::Equals(Value::Int(value)),
        source: None,
    }
}

fn traversal_fixture() -> OperatorTree {
    OperatorTree::Union(vec![
        OperatorTree::Complement(Box::new(leaf(1))),
        OperatorTree::HybridJoin {
            left: Box::new(leaf(2)),
            right: Box::new(leaf(3)),
        },
        OperatorTree::FacetVector {
            vector_op: Box::new(leaf(4)),
            facet_field: "id".into(),
        },
        OperatorTree::MultiStage {
            stages: vec![
                MultiStageEntry {
                    child: leaf(5),
                    cutoff: MultiStageCutoff::TopK(1),
                },
                MultiStageEntry {
                    child: leaf(6),
                    cutoff: MultiStageCutoff::TopK(1),
                },
            ],
        },
        OperatorTree::ProgressiveFusion {
            stages: vec![
                ProgressiveFusionEntry {
                    signal: leaf(7),
                    k: 1,
                },
                ProgressiveFusionEntry {
                    signal: leaf(8),
                    k: 1,
                },
            ],
            alpha: 0.5,
            gating: GatingSpec::Pass,
        },
        OperatorTree::DeepFusion {
            layers: vec![
                DeepFusionLayer::Signal {
                    signals: vec![leaf(9), leaf(10)],
                },
                DeepFusionLayer::Softmax,
                DeepFusionLayer::Signal {
                    signals: vec![leaf(11)],
                },
            ],
            alpha: 0.5,
            gating: GatingSpec::Pass,
        },
        OperatorTree::Opaque {
            kind: "opaque".into(),
            children: vec![leaf(12)],
            meta: BTreeMap::new(),
        },
    ])
}

#[test]
fn fallible_tree_visit_preserves_preorder_and_borrowed_nodes_without_workspace() {
    let tree = traversal_fixture();
    let expected = [
        0, -1, 1, -2, 2, 3, -3, 4, -4, 5, 6, -5, 7, 8, -6, 9, 10, 11, -7, 12,
    ];
    let mut ordinary = Vec::new();
    tree.visit(&mut |node| ordinary.push(std::ptr::from_ref(node)));
    let mut index = 0;
    let allocation = allocation_counter::measure(|| {
        tree.try_visit(&mut |node| {
            assert_eq!(std::ptr::from_ref(node), ordinary[index]);
            let identity = match node {
                OperatorTree::Union(_) => 0,
                OperatorTree::Complement(_) => -1,
                OperatorTree::HybridJoin { .. } => -2,
                OperatorTree::FacetVector { .. } => -3,
                OperatorTree::MultiStage { .. } => -4,
                OperatorTree::ProgressiveFusion { .. } => -5,
                OperatorTree::DeepFusion { .. } => -6,
                OperatorTree::Opaque { .. } => -7,
                OperatorTree::Filter {
                    predicate: Predicate::Equals(Value::Int(value)),
                    ..
                } => *value,
                _ => panic!("unexpected fixture node"),
            };
            assert_eq!(identity, expected[index]);
            index += 1;
            Ok::<(), ()>(())
        })
        .unwrap();
    });
    assert_eq!(index, expected.len());
    assert_eq!(index, ordinary.len());
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
}

#[test]
fn fallible_tree_visit_stops_at_each_child_boundary_and_returns_the_original_error() {
    let tree = traversal_fixture();
    let mut total = 0;
    tree.visit(&mut |_| total += 1);
    for stop in 0..total {
        let mut visited = 0;
        let failure = Box::new(stop);
        let address = std::ptr::from_ref(failure.as_ref());
        let mut failure = Some(failure);
        let error = tree
            .try_visit(&mut |_| {
                let current = visited;
                visited += 1;
                if current == stop {
                    Err(failure.take().unwrap())
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(visited, stop + 1);
        assert_eq!(std::ptr::from_ref(error.as_ref()), address);
    }
}
