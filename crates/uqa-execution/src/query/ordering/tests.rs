//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{order_projection, resolve_order_expression};
use uqa_sql::{plan::ProjectionPlan, ScalarExpr};

#[test]
fn output_ordinals_preserve_independent_positions_with_duplicate_labels() {
    let output = super::identity_order_columns(&["same".into(), "same".into()]);
    for position in [1, 2] {
        let expression = resolve_order_expression(
            &ScalarExpr::Literal(uqa_core::Value::Int(position)),
            &output,
        )
        .unwrap();
        assert_eq!(
            expression,
            ScalarExpr::Position(usize::try_from(position - 1).unwrap())
        );
    }
    assert_eq!(
        resolve_order_expression(&ScalarExpr::Column("same".into()), &output)
            .unwrap_err()
            .sqlstate(),
        Some("42702")
    );
}

#[test]
fn position_bound_order_by_reuses_qualified_primary_key_ordering() {
    let schema = crate::RowSchema::with_qualified_types(
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
    let required = [crate::PhysicalOrder {
        position: crate::order_expression_position(&schema, &expression).unwrap(),
        descending: false,
        nulls_first: Some(false),
        nullable: true,
    }];
    let actual = [crate::PhysicalOrder {
        position: 0,
        descending: false,
        nulls_first: None,
        nullable: false,
    }];
    assert!(crate::ordering_satisfies(&actual, &required));
}
