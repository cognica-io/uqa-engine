//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::relational::DefaultExpressionEvaluator;
use uqa_sql::ast::InternalRelationId;

fn row(qualifier: &str, names: &[&str], values: Vec<Value>) -> OwnedPhysicalRow {
    OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(
            qualifier,
            names.iter().map(|name| (*name).to_owned()).collect(),
            vec![Some(ColumnType::Integer); names.len()],
        ),
        PhysicalRow::from_values(values),
    )
}

#[test]
fn lexical_values_keep_structural_identity_and_local_null_precedence() {
    let mut outer = row(
        "target",
        &["n", "hidden"],
        vec![Value::Int(10), Value::Int(11)],
    );
    let reference = InternalRelationId::allocate().column(0);
    outer.schema = RowSchema::with_named_internal_aliases(
        &outer.schema,
        &[(reference, uqa_sql::ColumnIdentity::qualified("target", "n"))],
    );
    let local = row("target", &["n"], vec![Value::Null]);
    let evaluator = with_outer_row(DefaultExpressionEvaluator::shared(Vec::new()), Some(&outer));
    for (expression, expected) in [
        (ScalarExpr::Column("n".into()), Value::Null),
        (ScalarExpr::Column("hidden".into()), Value::Int(11)),
        (ScalarExpr::InternalColumn(reference), Value::Int(10)),
    ] {
        assert_eq!(evaluator.evaluate(&expression, &local).unwrap(), expected);
        assert_eq!(
            evaluator
                .evaluate_physical(&expression, &local.schema, &local.row)
                .unwrap(),
            expected
        );
        let prepared = crate::scalar::PreparedExpressions::scalar(expression.clone());
        assert_eq!(
            evaluator
                .evaluate_physical_with_function_states(
                    &prepared,
                    &local.schema,
                    &local.row,
                    prepared.calls(),
                )
                .unwrap(),
            expected
        );
        assert_eq!(
            evaluator
                .expression_type(&expression, &local.schema)
                .unwrap(),
            Some(ColumnType::Integer)
        );
    }
    let lookup = OuterLookup {
        local: &local,
        outer: &outer,
    };
    assert_eq!(lookup.qualified_column("target", "hidden"), None);
    assert_eq!(
        evaluator.project_star(&local).unwrap(),
        [("n".into(), Value::Null)]
    );
}

#[test]
fn local_ambiguity_survives_outer_lookup_and_schema_reuse_is_bounded() {
    let outer = row("target", &["n"], vec![Value::Int(10)]);
    let left = row("left", &["n"], vec![Value::Int(1)]);
    let right = row("right", &["n"], vec![Value::Int(2)]);
    let local = OwnedPhysicalRow::new(
        RowSchema::join(&left.schema, &right.schema, []),
        PhysicalRow::concat(&left.row, &right.row),
    );
    let evaluator = OuterExpressionEvaluator {
        evaluator: DefaultExpressionEvaluator::shared(Vec::new()),
        outer,
        physical_schemas: std::array::from_fn(|_| OnceLock::new()),
    };
    let lookup = OuterLookup {
        local: &local,
        outer: &evaluator.outer,
    };
    assert!(lookup.column_is_ambiguous("n"));
    assert!(evaluator
        .physical_schema(&local.schema)
        .column_is_ambiguous("n"));
    let first = evaluator.physical_schema(&local.schema).into_owned();
    let left_scope = evaluator.physical_schema(&left.schema).into_owned();
    let right_scope = evaluator.physical_schema(&right.schema).into_owned();
    for _ in 0..32 {
        assert!(evaluator
            .physical_schema(&local.schema)
            .shares_index(&first));
        assert!(evaluator
            .physical_schema(&left.schema)
            .shares_index(&left_scope));
        assert!(evaluator
            .physical_schema(&right.schema)
            .shares_index(&right_scope));
    }
    let different = row("other", &["value"], vec![Value::Int(99)]);
    assert_eq!(
        evaluator
            .evaluate_physical(
                &ScalarExpr::Column("n".into()),
                &different.schema,
                &different.row
            )
            .unwrap(),
        Value::Int(10)
    );
    assert!(evaluator
        .physical_schema(&local.schema)
        .shares_index(&first));
}
