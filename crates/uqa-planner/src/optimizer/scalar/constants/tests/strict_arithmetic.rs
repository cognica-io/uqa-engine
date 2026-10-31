//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn expression(sql: &str) -> ScalarExpr {
    let uqa_sql::Statement::Select(mut query) = uqa_sql::compile(sql).unwrap().remove(0) else {
        panic!("SELECT");
    };
    let expression = uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
    uqa_sql::bind_type_introspection(expression, &RowSchema::default(), &[])
}

#[test]
fn strict_arithmetic_null_preserves_postgresql_result_types_without_effects() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("pg18_strict_null.json")).unwrap();
    let config = crate::optimizer::OptimizerConfig::new(|_, _, _| {
        panic!("strict NULL arithmetic evaluated a volatile operand")
    });
    let mut checked = 0;
    for case in reference["cases"].as_array().unwrap() {
        if !case["fold_expression"].is_string() {
            continue;
        }
        assert!(case["error"].is_null());
        let sql = case["sql"].as_str().unwrap();
        let mut expression = expression(sql);
        crate::optimizer::scalar::optimize_scalar_slot(&mut expression, &config)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(
            literal_value(&expression).unwrap().as_deref(),
            Some(&Value::Null),
            "{sql}: {expression:?}"
        );
        let ty = scalar_type(&expression, &RowSchema::default(), &[])
            .unwrap()
            .expect("the selected operator retains its SQL result type");
        assert_eq!(
            uqa_sql::catalog::type_metadata::pg_type_oid(&ty),
            case["results"][0]["type_oids"][0].as_i64().unwrap(),
            "{sql}: {expression:?}"
        );
        checked += 1;
    }
    assert_eq!(checked, 16);
}

#[test]
fn strict_arithmetic_null_keeps_postgresql_child_constant_error_order() {
    let mut expression = expression("SELECT NULL::integer + (1 / 0)");
    let config = crate::optimizer::OptimizerConfig::new(
        uqa_execution::scalar::eval_constant_scalar_with_control,
    );
    let error =
        crate::optimizer::scalar::optimize_scalar_slot(&mut expression, &config).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(error.to_string(), "division by zero");
}

#[test]
fn strict_arithmetic_does_not_eliminate_unresolved_parameter_effects() {
    let expression = expression("SELECT $1 + nextval('seq')::integer");
    let original = expression.clone();
    let mut optimized = expression;
    let config = crate::optimizer::OptimizerConfig::new(|_, _, _| panic!("runtime argument"));
    crate::optimizer::scalar::optimize_scalar_slot(&mut optimized, &config).unwrap();
    assert_eq!(optimized, original);
}

#[test]
fn strict_arithmetic_null_uses_the_analyzed_result_after_losing_row_types() {
    let mut expression = ScalarExpr::Collated {
        expr: Box::new(expression("SELECT NULL::pg_catalog.interval + d")),
        ty: ColumnType::Timestamp,
        collation: serde_json::from_value(serde_json::json!({
            "derivation": "None", "initial_oid": 0
        }))
        .unwrap(),
    };
    let config = crate::optimizer::OptimizerConfig::new(|_, _, _| panic!("discarded row access"));
    crate::optimizer::scalar::optimize_scalar_slot(&mut expression, &config).unwrap();
    assert_eq!(
        literal_value(&expression).unwrap().as_deref(),
        Some(&Value::Null)
    );
    assert_eq!(
        scalar_type(&expression, &RowSchema::default(), &[]).unwrap(),
        Some(ColumnType::Timestamp)
    );
}

#[test]
fn strict_arithmetic_does_not_guess_a_result_for_an_unanalyzed_row() {
    let mut expression = expression("SELECT NULL::pg_catalog.interval + d");
    let config = crate::optimizer::OptimizerConfig::new(|_, _, _| panic!("unknown row type"));
    crate::optimizer::scalar::optimize_scalar_slot(&mut expression, &config).unwrap();
    assert!(matches!(
        expression,
        ScalarExpr::Binary {
            op: BinaryOp::Add,
            ..
        }
    ));
}
