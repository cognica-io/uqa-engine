//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Group expression identity preserves the analyzed literal type and representation.

use super::*;

#[test]
fn array_and_row_ordering_expressions_keep_structural_group_identity() {
    for (left, right, expected) in [
        ("ARRAY[n]", "ARRAY[n]", true),
        ("ROW(n, 0)", "ROW(n, 0)", true),
        ("ROW(ARRAY[n], 0)", "ROW(ARRAY[n], 0)", true),
        ("ARRAY[n, 0]", "ARRAY[0, n]", false),
        ("ARRAY[n]", "ARRAY[n, 0]", false),
        ("ROW(n, 0)", "ROW(n, 1)", false),
        ("ROW(n, 0)", "ROW(n, 0.0)", false),
        ("ARRAY[n]", "ROW(n)", false),
    ] {
        let sql = format!("SELECT DISTINCT ON ({left}) n FROM t ORDER BY {right},n");
        let plan = crate::plan::UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0));
        let crate::plan::UnifiedPlan::Query(query) = plan else {
            unreachable!()
        };
        let crate::plan::RelationalPlan::QueryBlock(block) = query.root else {
            unreachable!()
        };
        assert_eq!(
            exprs_match(&block.distinct_on[0], &block.order_by[0].expr),
            expected,
            "{sql}"
        );
    }
}

#[test]
fn builtin_aggregate_identity_distinguishes_qualification_and_scalar_overloads() {
    let call = |name: &str, binding| ScalarExpr::Func {
        order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
        name: name.into(),
        binding,
        args: vec![ScalarExpr::Literal(Value::Int(1))],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    for name in ["sum", "pg_catalog.sum", "PG_CATALOG.SUM"] {
        assert!(is_builtin_aggregate(&call(name, None)), "{name}");
    }
    assert!(!is_builtin_aggregate(&call("public.sum", None)));
    let mut binding = crate::ast::FunctionBinding {
        object_id: Some([1; 16]),
        name: "public.sum".into(),
        argument_types: vec!["integer".into()],
        builtin: false,
        dispatch: None,
        invocation: None,
        composite_field: None,
        resolution_error: None,
    };
    assert!(!is_builtin_aggregate(&call("sum", Some(binding.clone()))));
    binding.object_id = None;
    binding.name = "pg_catalog.sum".into();
    binding.builtin = true;
    assert!(is_builtin_aggregate(&call("sum", Some(binding))));
}

#[test]
fn grouped_expression_identity_includes_the_written_ordering_syntax() {
    let call = |syntax| ScalarExpr::Func {
        name: "sum".into(),
        binding: None,
        args: vec![ScalarExpr::Column("value".into())],
        distinct: false,
        order_by: Vec::new(),
        order_syntax: syntax,
        filter: None,
    };
    let ordinary = call(crate::ast::FunctionOrderSyntax::Ordinary);
    assert!(exprs_match(&ordinary, &ordinary));
    for syntax in [
        crate::ast::FunctionOrderSyntax::WithinGroup,
        crate::ast::FunctionOrderSyntax::Legacy,
    ] {
        assert!(!exprs_match(&ordinary, &call(syntax)));
    }
}

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
        composite_source: None,
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
        composite_source: None,
        value: decimal("1.0"),
        ty: "numeric".into(),
        bound_type: None,
        parameter_index: Some(1),
    };
    let second = ScalarExpr::TypedLiteral {
        composite_source: None,
        value: decimal("1.0"),
        ty: "numeric".into(),
        bound_type: None,
        parameter_index: Some(2),
    };
    assert!(exprs_match(&first, &first.clone()));
    assert!(!exprs_match(&first, &second));
}

#[test]
fn composite_literals_with_equal_current_values_keep_distinct_original_datums() {
    use crate::expr::composites::{
        CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor,
    };
    let expression = |original: &str| ScalarExpr::TypedLiteral {
        value: Value::Record(vec![("a".into(), Value::Bool(true))]),
        ty: "composite#20001".into(),
        bound_type: None,
        parameter_index: None,
        composite_source: Some(Box::new(CompositeConstantSource {
            value: Value::Record(vec![("a".into(), Value::Str(original.into()))]),
            descriptors: vec![CompositeTypeDescriptor {
                dropped: Vec::new(),
                type_oid: 20_001,
                relation_oid: 20_003,
                attributes: vec![CompositeAttribute {
                    name: "a".into(),
                    ty: crate::ColumnType::InternalChar,
                    number: 1,
                }],
            }],
        })),
    };
    let first = expression("A");
    assert!(exprs_match(&first, &first.clone()));
    assert!(!exprs_match(&first, &expression("B")));
}
