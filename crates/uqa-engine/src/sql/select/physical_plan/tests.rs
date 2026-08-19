//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn position_bound_order_by_reuses_qualified_primary_key_ordering() {
    let schema = uqa_execution::RowSchema::with_qualified_types(
        "lineitem",
        vec!["id".into(), "extended_price".into()],
        vec![None, None],
    );
    let projections = vec![
        ProjectionPlan {
            expr: ScalarExpr::Column("id".into()),
            alias: None,
        },
        ProjectionPlan {
            expr: ScalarExpr::Column("extended_price".into()),
            alias: None,
        },
    ];
    let (_, output) = order_projection(&projections, &schema).unwrap();
    let expression = resolve_order_expression(&ScalarExpr::Column("id".into()), &output).unwrap();
    assert_eq!(expression, ScalarExpr::Position(0));
    let required = [uqa_execution::PhysicalOrder {
        position: uqa_execution::order_expression_position(&schema, &expression).unwrap(),
        descending: false,
        nulls_first: Some(false),
        nullable: true,
    }];
    let actual = [uqa_execution::PhysicalOrder {
        position: 0,
        descending: false,
        nulls_first: None,
        nullable: false,
    }];
    assert!(uqa_execution::ordering_satisfies(&actual, &required));
}
