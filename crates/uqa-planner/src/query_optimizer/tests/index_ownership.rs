//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::query_optimizer::{IndexScanCandidate, QueryOptimizer};
use uqa_core::{Predicate, Value};
use uqa_operators::OperatorTree;

#[test]
fn selected_index_scan_moves_filter_payloads_and_copies_only_the_winning_name() {
    let field = "f".repeat(4 * 1024 * 1024);
    let text = "x".repeat(4 * 1024 * 1024);
    let field_address = field.as_ptr();
    let value_address = text.as_ptr();
    let predicate = Predicate::Equals(Value::Str(text));
    let candidates = [
        IndexScanCandidate {
            index_name: "ignored".repeat(1024 * 1024),
            table_name: "docs".into(),
            field: field.clone(),
            predicate: predicate.clone(),
            scan_cost: 2.0,
        },
        IndexScanCandidate {
            index_name: "selected".into(),
            table_name: "docs".into(),
            field: field.clone(),
            predicate: predicate.clone(),
            scan_cost: 1.0,
        },
    ];
    let optimizer = QueryOptimizer::new().with_index_candidates(candidates, "docs");
    let tree = OperatorTree::Filter {
        field,
        predicate,
        source: None,
    };
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(optimizer.apply_index_scan(tree));
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    let Some(OperatorTree::IndexScan {
        index_name,
        field,
        predicate,
    }) = output
    else {
        panic!("selected physical index")
    };
    assert_eq!(index_name, "selected");
    assert_eq!(field.as_ptr(), field_address);
    let Predicate::Equals(Value::Str(text)) = predicate else {
        panic!("original predicate")
    };
    assert_eq!(text.as_ptr(), value_address);
}

#[test]
fn declined_index_scan_keeps_input_and_does_not_copy_catalog_names() {
    let field = String::from("id");
    let text = "x".repeat(4 * 1024 * 1024);
    let field_address = field.as_ptr();
    let value_address = text.as_ptr();
    let predicate = Predicate::Equals(Value::Str(text));
    let candidate = IndexScanCandidate {
        index_name: "ignored".repeat(1024 * 1024),
        table_name: "docs".into(),
        field: field.clone(),
        predicate: predicate.clone(),
        scan_cost: f64::MAX,
    };
    let optimizer = QueryOptimizer::new().with_index_candidates([candidate], "docs");
    let tree = OperatorTree::Filter {
        field,
        predicate,
        source: None,
    };
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(optimizer.apply_index_scan(tree));
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    let Some(OperatorTree::Filter {
        field,
        predicate,
        source: None,
    }) = output
    else {
        panic!("unchanged input")
    };
    assert_eq!(field.as_ptr(), field_address);
    let Predicate::Equals(Value::Str(text)) = predicate else {
        panic!("original predicate")
    };
    assert_eq!(text.as_ptr(), value_address);
}

#[test]
fn index_selection_preserves_cost_thresholds_invalid_costs_and_first_ties() {
    let scan = crate::CostEstimator::default()
        .estimate_unary(crate::OperatorKind::TableScan, 1000.0)
        .total();
    for (cost, selected) in [
        (0.0, true),
        (scan / 2.0, true),
        (scan, false),
        (f64::INFINITY, false),
        (f64::NAN, false),
        (-1.0, false),
    ] {
        let candidate = |name: &str| IndexScanCandidate {
            index_name: name.into(),
            table_name: "docs".into(),
            field: "id".into(),
            predicate: Predicate::Equals(Value::Int(1)),
            scan_cost: cost,
        };
        let optimizer = QueryOptimizer::new()
            .with_index_candidates([candidate("first"), candidate("second")], "docs");
        let tree = OperatorTree::Filter {
            field: "id".into(),
            predicate: Predicate::Equals(Value::Int(1)),
            source: None,
        };
        match optimizer.apply_index_scan(tree) {
            OperatorTree::IndexScan {
                index_name,
                field,
                predicate,
            } => {
                assert!(selected, "{cost}");
                assert_eq!(index_name, "first");
                assert_eq!(field, "id");
                assert_eq!(predicate, Predicate::Equals(Value::Int(1)));
            }
            OperatorTree::Filter { .. } => assert!(!selected, "{cost}"),
            _ => panic!("unexpected substitution"),
        }
    }
}
