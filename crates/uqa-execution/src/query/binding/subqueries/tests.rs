//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_scope, NoRoutines};
use uqa_sql::plan::{QueryPlan, UnifiedPlan};

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("expected query")
    };
    *query
}

#[test]
fn missing_scalar_slot_is_reported_before_missing_binding_metadata() {
    let scope: CteScope = CteScope::new();
    let error = resolve_scalar_subquery_type(&NoRoutines, 3, &RowSchema::default(), &[], &scope)
        .unwrap_err();
    assert!(
        matches!(error, SQLError::Internal(message) if message == "physical scalar subquery slot 3 is out of bounds")
    );
}

#[test]
fn scalar_subquery_type_retains_outer_qualifiers_and_declared_parameter_types() {
    let mut scope = empty_scope();
    scope
        .scalar_subqueries
        .push(query("SELECT outer_row.label"));
    scope.scalar_subqueries.push(query("SELECT $1"));
    let outer = RowSchema::with_qualified_types(
        "outer_row",
        vec!["label".into()],
        vec![Some(ColumnType::Text)],
    );
    assert_eq!(
        resolve_scalar_subquery_type(&NoRoutines, 0, &outer, &[], &scope).unwrap(),
        Some(ColumnType::Text)
    );
    let params = [SQLParam::TypedScalar {
        value: uqa_core::Value::Int(7),
        ty: ColumnType::BigInteger,
    }];
    assert_eq!(
        resolve_scalar_subquery_type(&NoRoutines, 1, &outer, &params, &scope).unwrap(),
        Some(ColumnType::BigInteger)
    );
}
