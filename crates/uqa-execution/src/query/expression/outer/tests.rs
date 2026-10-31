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
        outer: Arc::new(outer),
        physical_schemas: std::array::from_fn(|_| OnceLock::new()),
        resources: ProductionControl::uncontrolled().capture(),
        _memory: None,
    };
    let lookup = OuterLookup {
        local: &local,
        outer: &evaluator.outer,
    };
    assert!(lookup.column_is_ambiguous("n"));
    assert!(evaluator
        .physical_schema(&local.schema)
        .unwrap()
        .column_is_ambiguous("n"));
    let first = evaluator
        .physical_schema(&local.schema)
        .unwrap()
        .into_owned();
    let left_scope = evaluator
        .physical_schema(&left.schema)
        .unwrap()
        .into_owned();
    let right_scope = evaluator
        .physical_schema(&right.schema)
        .unwrap()
        .into_owned();
    for _ in 0..32 {
        assert!(evaluator
            .physical_schema(&local.schema)
            .unwrap()
            .shares_index(&first));
        assert!(evaluator
            .physical_schema(&left.schema)
            .unwrap()
            .shares_index(&left_scope));
        assert!(evaluator
            .physical_schema(&right.schema)
            .unwrap()
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
        .unwrap()
        .shares_index(&first));
}

#[test]
fn outer_evaluator_shares_rows_and_keeps_only_admitted_headers() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    let outer = Arc::new(row(
        "outer",
        &["payload"],
        vec![Value::Str("x".repeat(4 << 20))],
    ));
    let evaluator = DefaultExpressionEvaluator::shared(Vec::new());
    let token = CancellationToken::new();
    let bytes = size_of::<OuterExpressionEvaluator<'_>>();
    for limit in [bytes - 1, bytes] {
        let budget = MemoryBudget::new(limit);
        let control = ProductionControl::new(&budget, &token, &token);
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(with_outer_row_with_control(
                evaluator.clone(),
                Some(outer.clone()),
                &control,
            ));
        });
        assert!(allocation.bytes_total < 4096, "{allocation:?}");
        if limit < bytes {
            assert_eq!(result.unwrap().err().unwrap().sqlstate(), Some("53200"));
        } else {
            let captured = result.unwrap().unwrap();
            assert_eq!(budget.used(), bytes);
            assert_eq!(Arc::strong_count(&outer), 2);
            let reader = captured.clone();
            drop(captured);
            assert_eq!(budget.used(), bytes);
            drop(reader);
        }
        assert_eq!(Arc::strong_count(&outer), 1);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn outer_layout_cache_admits_before_copying_and_publishes_only_complete_schemas() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let evaluator = OuterExpressionEvaluator {
        evaluator: DefaultExpressionEvaluator::shared(Vec::new()),
        outer: Arc::new(row("outer", &["n"], vec![Value::Int(7)])),
        physical_schemas: std::array::from_fn(|_| OnceLock::new()),
        resources: control.capture(),
        _memory: None,
    };
    let local = row("local", &["n"], vec![Value::Int(1)]);
    let retained = evaluator
        .physical_schema(&local.schema)
        .unwrap()
        .into_owned();
    let before = budget.used();
    assert!(before > 0);
    let large = RowSchema::new(vec!["x".repeat(4 << 20)]);
    let occupied = budget.reserve(budget.available() - (64 << 10)).unwrap();
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            evaluator.physical_schema(&large).unwrap_err().sqlstate(),
            Some("53200")
        );
    });
    assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
    assert_eq!(budget.used(), before + occupied.bytes());
    assert_eq!(
        evaluator
            .physical_schemas
            .iter()
            .filter(|entry| entry.get().is_some())
            .count(),
        1
    );
    assert!(evaluator
        .physical_schema(&local.schema)
        .unwrap()
        .shares_index(&retained));
    drop(occupied);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            evaluator
                .physical_schema(&local.schema)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), before);
        token.reset();
    }
    drop(evaluator);
    assert_eq!(budget.used(), before);
    drop(retained);
    assert_eq!(budget.used(), 0);
}
