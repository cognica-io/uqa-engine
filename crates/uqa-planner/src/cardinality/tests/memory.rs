//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::ScalarExpr;

fn cast_comparison(value: Value, casts: usize, op: BinaryOp) -> ScalarExpr {
    let mut literal = ScalarExpr::Literal(value);
    for _ in 0..casts {
        literal = ScalarExpr::Cast {
            expr: Box::new(literal),
            ty: "text".into(),
            implicit: false,
        };
    }
    ScalarExpr::Binary {
        op,
        lhs: Box::new(ScalarExpr::Column("id".into())),
        rhs: Box::new(literal),
    }
}

fn resident(value: &Value) -> ScalarExpr {
    let control = ProductionControl::uncontrolled();
    ScalarExpr::RetainedLiteral(uqa_sql::ir::RetainedScalarLiteral::new(
        uqa_sql::ir::RetainedConstantValue::from_produced(
            control.copy_value(value).unwrap(),
            &control,
        )
        .unwrap(),
        None,
        None,
        None,
    ))
}

#[test]
fn scalar_costing_retains_cast_storage_through_nested_and_sibling_production() {
    const BYTES: usize = 4 * 1024 * 1024;
    let value = Value::Str("x".repeat(BYTES));
    let statistics = RelationStats::new(100).with_column(
        "id",
        ColumnStats {
            row_count: 100,
            distinct_count: 10,
            mcv_values: vec![value.clone()],
            mcv_frequencies: vec![0.25],
            ..Default::default()
        },
    );
    let estimator = CardinalityEstimator::new();
    let token = CancellationToken::new();
    for casts in [1, 2] {
        for (operator, expected) in [(BinaryOp::Equal, 0.25), (BinaryOp::NotEqual, 0.75)] {
            let predicate = cast_comparison(value.clone(), casts, operator);
            let budget = MemoryBudget::new(casts * BYTES);
            let control = ProductionControl::new(&budget, &token, &token);
            let allocation = allocation_counter::measure(|| {
                assert_eq!(
                    estimator
                        .scalar_selectivity_with_control(&predicate, &statistics, &control)
                        .unwrap()
                        .raw(),
                    expected
                );
            });
            assert!(
                allocation.bytes_total < (casts * BYTES + 4096) as u64,
                "{casts} casts, {operator:?}: {allocation:?}"
            );
            assert_eq!(budget.peak(), casts * BYTES);
            assert_eq!(budget.used(), 0);

            // The outer cast must coexist with its input, and a failed sibling must not release another owner's lease.
            let parent = MemoryBudget::new(casts * BYTES);
            let sibling = parent.reserve(1).unwrap();
            let budget = parent.child(casts * BYTES);
            let control = ProductionControl::new(&budget, &token, &token);
            assert_eq!(
                estimator
                    .scalar_selectivity_with_control(&predicate, &statistics, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
            assert_eq!(budget.used(), 0);
            assert_eq!(parent.used(), 1);
            drop(sibling);
            assert_eq!(
                estimator
                    .scalar_selectivity_with_control(&predicate, &statistics, &control)
                    .unwrap()
                    .raw(),
                expected
            );
            assert_eq!(parent.used(), 0);
        }
    }
}

#[test]
fn scalar_costing_releases_each_membership_cast_and_checks_both_cancellations() {
    const BYTES: usize = 4 * 1024 * 1024;
    let statistics = RelationStats::new(100).with_column(
        "id",
        ColumnStats {
            row_count: 100,
            distinct_count: 10,
            ..Default::default()
        },
    );
    let ScalarExpr::Binary { rhs, .. } =
        cast_comparison(Value::Str("x".repeat(BYTES)), 1, BinaryOp::Equal)
    else {
        unreachable!()
    };
    let predicate = ScalarExpr::InList {
        expr: Box::new(ScalarExpr::Column("id".into())),
        list: vec![*rhs.clone(), *rhs],
        negated: false,
    };
    let estimator = CardinalityEstimator::new();
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let budget = MemoryBudget::new(BYTES);
    let control = ProductionControl::new(&budget, &original, &invoking);
    assert_eq!(
        estimator
            .scalar_selectivity_with_control(&predicate, &statistics, &control)
            .unwrap()
            .raw(),
        0.2
    );
    assert_eq!(budget.peak(), BYTES);
    assert_eq!(budget.used(), 0);
    for token in [&original, &invoking] {
        token.cancel();
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                estimator
                    .scalar_selectivity_with_control(&predicate, &statistics, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
        });
        assert!(allocation.bytes_total < 4096, "{allocation:?}");
        assert_eq!(budget.used(), 0);
        token.reset();
    }
    // Costing chooses a RHS literal before inspecting the LHS; an unused cast must not consume allowance or change diagnostics.
    let ScalarExpr::InList { mut list, .. } = predicate else {
        unreachable!()
    };
    let predicate = ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(list.remove(0)),
        rhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
    };
    let budget = MemoryBudget::new(0);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            estimator
                .scalar_selectivity_with_control(&predicate, &statistics, &control)
                .unwrap()
                .raw(),
            estimator.default_selectivity
        );
    });
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn scalar_costing_borrows_large_literal_and_membership_values() {
    let large = Value::Str("x".repeat(4 * 1024 * 1024));
    let statistics = RelationStats::new(100).with_column(
        "id",
        ColumnStats {
            row_count: 100,
            distinct_count: 10,
            mcv_values: vec![large.clone()],
            mcv_frequencies: vec![0.25],
            ..Default::default()
        },
    );
    let estimator = CardinalityEstimator::new();
    let comparison = ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::Column("id".into())),
        rhs: Box::new(ScalarExpr::Literal(large.clone())),
    };
    let mut cases: Vec<(ScalarExpr, f64)> = vec![
        (comparison, 0.25),
        (
            ScalarExpr::Binary {
                op: BinaryOp::Equal,
                lhs: Box::new(ScalarExpr::Column("id".into())),
                rhs: Box::new(resident(&large)),
            },
            0.25,
        ),
        (
            ScalarExpr::InList {
                expr: Box::new(ScalarExpr::Column("id".into())),
                list: vec![],
                negated: false,
            },
            -0.0,
        ),
    ];
    for negated in [false, true] {
        let membership = ScalarExpr::InList {
            expr: Box::new(ScalarExpr::Column("id".into())),
            list: vec![
                ScalarExpr::Literal(large.clone()),
                ScalarExpr::Literal(Value::Str("other".into())),
            ],
            negated,
        };
        let mut analyzed = uqa_sql::bind_type_introspection(
            membership.clone(),
            &uqa_sql::RowSchema::with_types(
                vec!["id".into()],
                vec![Some(uqa_sql::ColumnType::Text)],
            ),
            &[],
        );
        let ScalarExpr::Func { args, .. } = &mut analyzed else {
            panic!("analyzed scalar-array comparison");
        };
        args[1] = ScalarExpr::Literal(Value::Array(
            uqa_core::ArrayValue::try_new(vec![large.clone(), Value::Str("other".into())]).unwrap(),
        ));
        assert!(matches!(
            uqa_sql::semantics::membership_operands(&analyzed)
                .unwrap()
                .items,
            uqa_sql::semantics::MembershipItems::Constants(_)
        ));
        let expected = 0.25 + 0.75 / 9.0;
        let expected = if negated { 1.0 - expected } else { expected };
        let mut shared = analyzed.clone();
        let ScalarExpr::Func { args, .. } = &mut shared else {
            panic!("shared membership");
        };
        let ScalarExpr::Literal(array) = &args[1] else {
            panic!("constant array");
        };
        args[1] = resident(array);
        cases.extend([
            (membership, expected),
            (analyzed, expected),
            (shared, expected),
        ]);
    }
    for (predicate, expected) in cases {
        let allocation = allocation_counter::measure(|| {
            let actual = estimator.scalar_selectivity(&predicate, &statistics).raw();
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "{actual} != {expected}"
            );
        });
        assert!(allocation.bytes_total < 4096, "{allocation:?}");
        assert_eq!(allocation.bytes_current, 0);
    }
}

#[test]
fn borrowed_scalar_costing_preserves_estimates_and_cast_fallbacks() {
    let statistics = RelationStats::new(100).with_column(
        "id",
        ColumnStats {
            row_count: 100,
            null_count: 20,
            distinct_count: 10,
            mcv_values: vec![Value::Int(7), Value::Int(2)],
            mcv_frequencies: vec![0.3, 0.2],
            ..Default::default()
        },
    );
    let estimator = CardinalityEstimator::new();
    for (predicate, expected) in [
        ("id = 7", 0.3),
        ("7 = id", 0.3),
        ("id = 999", 0.3 / 8.0),
        ("id <> 7", 0.5),
        ("id = NULL", 0.0),
        ("id IS NULL", 0.2),
        ("id IS NOT NULL", 0.8),
        ("id = 7 AND id = 2", 0.3 * 0.2),
        ("id = 7 OR id = 2", 1.0 - 0.7 * 0.8),
        ("NOT (id = 7)", 0.7),
        ("id < 10", 0.3),
        ("id = '7'::integer", 0.3),
        ("id = 'bad'::integer", 0.8 / 10.0),
        ("id = 7::bigint", 0.3),
        ("id = $1", 0.8 / 10.0),
        ("true", 1.0),
        ("false", 0.0),
        ("unknown_function(id)", 0.1),
        // SQL lowering gives BETWEEN two conjunctive comparisons.
        ("id BETWEEN 1 AND 10", 0.3 * 0.3),
        ("knn_match('vector', ARRAY[1], 5)", 0.05),
    ] {
        let sql = format!("SELECT * FROM source_table WHERE {predicate}");
        let plan = crate::UnifiedPlan::lower(uqa_sql::compile(&sql).unwrap().remove(0));
        let crate::UnifiedPlan::Query(query) = plan else {
            panic!("query");
        };
        let crate::RelationalPlan::QueryBlock(block) = query.root else {
            panic!("query block");
        };
        let actual = estimator
            .scalar_selectivity(block.r#where.as_ref().unwrap(), &statistics)
            .raw();
        assert!(
            (actual - expected).abs() < 1e-12,
            "{predicate}: {actual} != {expected}"
        );
        let budget = MemoryBudget::new(16384);
        let token = CancellationToken::new();
        let controlled = estimator
            .scalar_selectivity_with_control(
                block.r#where.as_ref().unwrap(),
                &statistics,
                &ProductionControl::new(&budget, &token, &token),
            )
            .unwrap()
            .raw();
        assert_eq!(controlled.to_bits(), actual.to_bits(), "{predicate}");
        assert_eq!(budget.used(), 0);
    }
}
