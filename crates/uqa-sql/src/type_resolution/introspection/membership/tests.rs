//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    ast::{ColumnType, FunctionDispatch},
    plan::ExpressionPlan,
    RowSchema, ScalarExpr,
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn expression(sql: &str) -> ScalarExpr {
    let crate::Statement::Select(mut query) =
        crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        panic!("SELECT")
    };
    ExpressionPlan::lower(query.projections.remove(0).expr).scalar
}

fn schema() -> RowSchema {
    RowSchema::with_types(vec!["c".into()], vec![Some(ColumnType::Regclass)])
}

#[test]
fn membership_preserves_selected_comparisons_and_rebinding() {
    for (sql, comparisons) in [
        ("c IN ('pg_class'::regclass)", 1),
        ("c IN ('pg_class'::regclass, 'pg_type'::regclass)", 1),
        ("c IN (c, 'pg_class'::regclass)", 2),
        ("c NOT IN ('pg_class'::regclass, c, 'pg_type'::regclass)", 2),
    ] {
        let scalar = crate::bind_type_introspection(expression(sql), &schema(), &[]);
        let mut count = 0;
        scalar.visit(&mut |node| {
            count += usize::from(matches!(node, ScalarExpr::Binary { .. }));
            count += usize::from(matches!(node, ScalarExpr::Func { binding: Some(binding), .. } if matches!(binding.dispatch, Some(FunctionDispatch::AnyOperator | FunctionDispatch::AllOperator))));
            assert!(!matches!(node, ScalarExpr::InList { .. }), "{sql}: {node:?}");
        });
        assert_eq!(count, comparisons, "{sql}: {scalar:?}");
        let again = crate::bind_type_introspection(scalar.clone(), &schema(), &[]);
        assert_eq!(again, scalar, "{sql}");
        let json = serde_json::to_string(&scalar).unwrap();
        assert_eq!(serde_json::from_str::<ScalarExpr>(&json).unwrap(), scalar);
    }
    let scalar = crate::bind_type_introspection(
        expression("c = ANY(ARRAY['pg_class'::regclass])"),
        &schema(),
        &[],
    );
    let ScalarExpr::Func { args, .. } = scalar else {
        panic!("quantified comparison")
    };
    assert!(matches!(&args[0], ScalarExpr::Cast { ty, implicit: true, .. } if ty == "oid"));
    assert!(
        matches!(&args[1], ScalarExpr::Cast { ty, implicit: true, expr } if ty == "oid[]" && matches!(**expr, ScalarExpr::Array(_)))
    );
}

#[test]
fn dynamic_membership_keeps_undeclared_carriers() {
    let scalar = expression("c IN (1, 2)");
    let bound = crate::bind_type_introspection(scalar.clone(), &RowSchema::default(), &[]);
    assert_eq!(scalar, bound);
}

#[test]
fn enclosing_query_columns_remain_array_candidates() {
    let outer =
        RowSchema::with_qualified_types("s", vec!["c".into()], vec![Some(ColumnType::Integer)]);
    let input = RowSchema::with_outer_schema(&RowSchema::default(), &outer);
    let scalar = crate::bind_type_introspection(expression("1 IN (s.c, s.c)"), &input, &[]);
    assert!(
        matches!(scalar, ScalarExpr::Func { binding: Some(ref binding), .. } if binding.dispatch == Some(FunctionDispatch::AnyOperator))
    );
    let scalar = crate::bind_type_introspection(expression("1 IN (s.c, s.c)"), &outer, &[]);
    assert!(matches!(scalar, ScalarExpr::Or(_)));
}

#[test]
fn membership_copies_remain_admitted_and_release_on_failure_or_cancellation() {
    let source = expression("('pg_class'::regclass) IN (c, 'pg_type'::regclass, c)");
    let expected = crate::bind_type_introspection(source.clone(), &schema(), &[]);
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.clone_with_control(&control).unwrap();
    let admitted_input = budget.used();
    let bound =
        crate::bind_type_introspection_with_control(input, &schema(), &[], &control).unwrap();
    assert_eq!(*bound, expected);
    assert!(budget.used() > admitted_input);
    drop(bound);
    assert_eq!(budget.used(), 0);

    let budget = MemoryBudget::new(admitted_input);
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.clone_with_control(&control).unwrap();
    let error =
        crate::bind_type_introspection_with_control(input, &schema(), &[], &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    token.cancel();
    assert!(source.clone_with_control(&control).is_err());
    assert_eq!(budget.used(), 0);
}
