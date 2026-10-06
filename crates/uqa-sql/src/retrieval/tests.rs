//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ColumnType, RowSchema};

fn bound(sql: &str) -> ScalarExpr {
    let crate::Statement::Select(mut query) =
        crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        panic!("SELECT")
    };
    let scalar = crate::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
    crate::bind_type_introspection(
        scalar,
        &RowSchema::with_types(vec!["c".into()], vec![Some(ColumnType::Integer)]),
        &[],
    )
}

fn lower(sql: &str) -> Option<RetrievalExpr> {
    struct Source;
    impl GraphNameCatalog for Source {
        fn list_graphs(&self) -> Result<Vec<String>, SQLError> {
            unreachable!("membership reads no graphs")
        }
    }
    impl RetrievalArguments for Source {
        fn evaluate_argument(&self, _: &ScalarExpr, _: &[SQLParam]) -> Result<Value, SQLError> {
            unreachable!("membership uses the constant evaluator")
        }
    }
    lower_where_bound(
        &Source,
        &bound(sql),
        &RetrievalConstants {
            params: &[],
            evaluate: &|expression, _| constant(expression),
            stores: &|_| true,
        },
    )
    .unwrap()
}

fn constant(expression: &ScalarExpr) -> Result<Value, SQLError> {
    match expression {
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => Ok(value.clone()),
        ScalarExpr::Cast { expr, ty, .. } => crate::expr::cast_value(&constant(expr)?, ty),
        _ => Err(SQLError::Internal(
            "test accepts only retained constants".into(),
        )),
    }
}

#[test]
fn analyzed_membership_keeps_set_filter_and_null_rejection() {
    let Some(RetrievalExpr::Filter {
        field,
        predicate: Predicate::InSet(values),
        ..
    }) = lower("c IN (1, 2, NULL)")
    else {
        panic!("set filter")
    };
    assert_eq!(field, "c");
    assert_eq!(values, BTreeSet::from([Value::Int(1), Value::Int(2)]));
    assert!(matches!(
        lower("c NOT IN (1, NULL)"),
        Some(RetrievalExpr::Empty)
    ));
    let Some(RetrievalExpr::Intersect(parts)) = lower("c NOT IN (1, 2)") else {
        panic!("three-valued negation")
    };
    assert!(matches!(
        parts.as_slice(),
        [
            RetrievalExpr::Complement(_),
            RetrievalExpr::Filter {
                predicate: Predicate::IsNotNull,
                ..
            }
        ]
    ));
}

#[test]
fn set_recognition_retains_conversions_and_quantified_operator_identity() {
    for sql in [
        "c < ANY(ARRAY[1, 2])",
        "c = ALL(ARRAY[1, 2])",
        "c <> ALL(ARRAY[]::integer[])",
        "c IN (c, 2)",
    ] {
        assert!(
            crate::semantics::membership_operands(&bound(sql)).is_none(),
            "{sql}"
        );
    }
    let mut scalar = bound("c IN (1, 2)");
    let ScalarExpr::Func { args, .. } = &mut scalar else {
        panic!("scalar-array comparison")
    };
    args[1] = ScalarExpr::Cast {
        expr: Box::new(args[1].clone()),
        ty: "bigint[]".into(),
        implicit: true,
    };
    assert!(crate::semantics::membership_operands(&scalar).is_none());
    scalar = bound("c IN (1, 2)");
    let ScalarExpr::Func { binding, .. } = &mut scalar else {
        unreachable!()
    };
    *binding = None;
    assert!(crate::semantics::membership_operands(&scalar).is_none());
}
