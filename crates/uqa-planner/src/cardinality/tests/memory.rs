//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ScalarExpr;

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
    let mut cases = vec![(comparison, 0.25)];
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
        cases.extend([(membership, expected), (analyzed, expected)]);
    }
    for (predicate, expected) in cases {
        let allocation = allocation_counter::measure(|| {
            let actual = estimator.scalar_selectivity(&predicate, &statistics).raw();
            assert!((actual - expected).abs() < 1e-12, "{actual} != {expected}");
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
    }
}
