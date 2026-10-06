//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ast::ColumnType, plan::ExpressionPlan, ScalarExpr};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

#[test]
fn admitted_scalar_copy_preserves_window_frames_bindings_and_typed_payloads() {
    let crate::Statement::Select(mut query) = crate::compile("SELECT CASE WHEN a > 1 THEN sum(a) FILTER (WHERE b IS NOT NULL) OVER (PARTITION BY a ORDER BY b ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING) ELSE 4 END").unwrap().remove(0) else { panic!("SELECT") };
    let window = ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
    let source = ScalarExpr::Row(vec![
        window,
        ScalarExpr::TypedLiteral {
            value: Value::Str("retained scalar payload".repeat(100)),
            ty: "character varying[]".into(),
            bound_type: Some(ColumnType::Array(Box::new(ColumnType::Varchar(Some(32))))),
            parameter_index: Some(1),
        },
        ScalarExpr::ScalarSubquery(7),
    ]);
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&budget, &token, &token);
    let copied = source.clone_with_control(&control).unwrap();
    assert_eq!(*copied, source);
    assert!(budget.used() > 2_000);
    drop(copied);
    assert_eq!(budget.used(), 0);
    let small = MemoryBudget::new(2_000);
    let control = ProductionControl::new(&small, &token, &token);
    assert!(source.clone_with_control(&control).is_err());
    assert_eq!(small.used(), 0);
    token.cancel();
    assert!(source.clone_with_control(&control).is_err());
    assert_eq!(small.used(), 0);
}
