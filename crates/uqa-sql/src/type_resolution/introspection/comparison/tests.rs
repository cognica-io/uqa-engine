//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ast::ColumnType, plan::ExpressionPlan, RowSchema, ScalarExpr};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn expression(source: &str) -> ScalarExpr {
    let crate::Statement::Select(mut query) = crate::compile(&format!("SELECT {source}"))
        .unwrap()
        .remove(0)
    else {
        panic!("SELECT expression")
    };
    ExpressionPlan::lower(query.projections.remove(0).expr).scalar
}

#[test]
fn numeric_comparison_binding_retains_selected_casts_without_masking_exact_columns() {
    let schema = RowSchema::with_types(
        vec!["n".into(), "f".into()],
        vec![
            Some(ColumnType::BigInteger),
            Some(ColumnType::DoublePrecision),
        ],
    );
    let scalar = crate::bind_type_introspection(expression("n = f"), &schema, &[]);
    let ScalarExpr::Binary { lhs, rhs, .. } = scalar else {
        panic!("binary comparison")
    };
    assert!(matches!(*lhs, ScalarExpr::Cast { ref ty, .. } if ty == "double precision"));
    assert!(matches!(*rhs, ScalarExpr::Column(ref name) if name == "f"));

    let scalar = crate::bind_type_introspection(expression("n = 1"), &schema, &[]);
    let ScalarExpr::Binary { lhs, rhs, .. } = scalar else {
        panic!("binary comparison")
    };
    assert!(matches!(*lhs, ScalarExpr::Column(ref name) if name == "n"));
    assert!(matches!(*rhs, ScalarExpr::Literal(_)));

    let scalar = crate::bind_type_introspection(expression("'1' = 1.0"), &schema, &[]);
    let ScalarExpr::Binary { lhs, .. } = scalar else {
        panic!("binary comparison")
    };
    assert!(matches!(*lhs, ScalarExpr::Cast { ref ty, .. } if ty == "numeric"));
}

#[test]
fn controlled_numeric_comparison_binding_preserves_operand_cast_ownership() {
    let schema = RowSchema::default();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let scalar = expression("9007199254740993::bigint = 9007199254740992::double precision");
    let expected = crate::bind_type_introspection(scalar.clone(), &schema, &[]);
    let crate::Statement::Select(mut query) =
        crate::compile("SELECT 9007199254740993::bigint = 9007199254740992::double precision")
            .unwrap()
            .remove(0)
    else {
        panic!("SELECT expression")
    };
    let input = ExpressionPlan::lower_column_budgeted(
        &query.projections.remove(0).expr,
        &budget,
        &token,
        &token,
    )
    .unwrap()
    .into();
    let bound = crate::bind_type_introspection_with_control(input, &schema, &[], &control).unwrap();
    assert_eq!(*bound, expected);
    assert!(budget.used() > 0);
    drop(bound);
    assert_eq!(budget.used(), 0);
}
