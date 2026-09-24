//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Group expression identity preserves the analyzed literal type and representation.

use super::*;

fn decimal(text: &str) -> Value {
    Value::Decimal(uqa_core::DecimalValue::parse(text).unwrap())
}

#[test]
fn grouped_expression_slots_match_the_complete_key_before_its_inputs() {
    let input = ScalarExpr::Column("n".into());
    let computed = ScalarExpr::Binary {
        op: crate::ast::BinaryOp::Add,
        lhs: Box::new(input.clone()),
        rhs: Box::new(ScalarExpr::Literal(decimal("1.0"))),
    };
    let relation = crate::ast::InternalRelationId::allocate();
    let rewritten =
        compile_group_slots(&computed, &[input, computed.clone()], relation, 3).unwrap();
    assert_eq!(rewritten, ScalarExpr::InternalColumn(relation.column(4)));
}

#[test]
fn absent_group_keys_become_null_without_changing_aggregate_arguments() {
    let plan = crate::plan::UnifiedPlan::lower(crate::compile(
        "SELECT n + 1.0 AS shifted, sum(n + 1.0) FROM t GROUP BY GROUPING SETS ((n, n + 1.0), (n)) HAVING n + 1.0 IS NULL"
    ).unwrap().remove(0));
    let crate::plan::UnifiedPlan::Query(query) = plan else {
        unreachable!()
    };
    let crate::plan::RelationalPlan::QueryBlock(statement) = query.root else {
        unreachable!()
    };
    let active =
        select_grouping_set(&|_: &str| false, &statement, &statement.grouping_sets[1]).unwrap();
    assert!(matches!(
        active.projections[0].expr,
        ScalarExpr::Literal(Value::Null)
    ));
    assert_eq!(active.projections[1].expr, statement.projections[1].expr);
    assert!(
        matches!(active.having, Some(ScalarExpr::IsNull { expr, negated: false }) if matches!(*expr, ScalarExpr::Literal(Value::Null)))
    );
}

#[test]
fn decimal_literal_identity_matches_its_clone_without_erasing_scale() {
    for text in ["1.0", "1.00", "1e0", "1.00e1", "-0.00", "NaN", "Infinity"] {
        let expression = ScalarExpr::Literal(decimal(text));
        assert!(exprs_match(&expression, &expression.clone()), "{text}");
    }
    for (left, right, expected) in [
        ("1.0", "1.00", false),
        ("1e0", "1.", true),
        ("1.00e1", "10.0", true),
        ("1.00e1", "10.", false),
        ("-0.0", "0.0", true),
    ] {
        assert_eq!(literals_equal(&decimal(left), &decimal(right)), expected);
        assert_eq!(literals_equal(&decimal(right), &decimal(left)), expected);
    }
    for value in [Value::Int(1), Value::Float(1.0), Value::Str("1.0".into())] {
        assert!(!literals_equal(&decimal("1.0"), &value));
        assert!(!literals_equal(&value, &decimal("1.0")));
    }
}

#[test]
fn typed_literal_identity_retains_the_declared_type_and_datum() {
    let expression = |value, ty: &str| ScalarExpr::TypedLiteral {
        value,
        ty: ty.into(),
        bound_type: None,
        parameter_index: None,
    };
    let value = expression(decimal("1.0"), "numeric");
    assert!(exprs_match(&value, &value.clone()));
    assert!(!exprs_match(
        &value,
        &expression(decimal("1.00"), "numeric")
    ));
    assert!(!exprs_match(
        &expression(Value::Null, "integer"),
        &expression(Value::Null, "bigint")
    ));
    assert!(!exprs_match(
        &expression(Value::Int(1), "integer"),
        &ScalarExpr::Literal(Value::Int(1))
    ));
    let first = ScalarExpr::TypedLiteral {
        value: decimal("1.0"),
        ty: "numeric".into(),
        bound_type: None,
        parameter_index: Some(1),
    };
    let second = ScalarExpr::TypedLiteral {
        value: decimal("1.0"),
        ty: "numeric".into(),
        bound_type: None,
        parameter_index: Some(2),
    };
    assert!(exprs_match(&first, &first.clone()));
    assert!(!exprs_match(&first, &second));
}
