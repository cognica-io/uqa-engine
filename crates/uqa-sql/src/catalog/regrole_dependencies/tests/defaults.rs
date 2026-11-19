//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_parameter_roles_follow_scalar_scope_and_window_order() {
    for (sql, expected) in [
        ("(SELECT 'nested'::regrole)", &[][..]),
        ("'left'::regrole IN (SELECT 'nested'::regrole)", &["left"][..]),
        ("ARRAY['first'::regrole, ('23'::text)::regrole, 'last'::regrole]", &["first", "last"][..]),
        ("count('arg'::regrole) FILTER (WHERE 'filter'::regrole IS NOT NULL) OVER (PARTITION BY 'partition'::regrole ORDER BY 'order'::regrole)", &["arg", "filter", "partition", "order"][..]),
    ] {
        let default = expression(sql);
        let mut lowered = StoredRegroleConstants::default();
        lowered.collect_scalar_expression(&crate::plan::ExpressionPlan::lower(default.clone()).scalar);
        assert_eq!(lowered.inputs, expected, "{sql}");
        let mut original = StoredRegroleConstants::default();
        original.collect_expression(&default, None);
        assert_eq!(original.inputs, expected, "{sql}");
    }
}

#[test]
fn wide_stored_parameter_role_observation_borrows_the_original_default() {
    let mut children = vec![Expr::Literal(Value::Int(23)); 4095];
    children.push(expression("'last_role'::regrole"));
    let default = Expr::Array(children);
    let mut constants = StoredRegroleConstants::default();
    let allocations = allocation_counter::measure(|| constants.collect_expression(&default, None));
    assert_eq!(constants.inputs, ["last_role"]);
    assert!(allocations.bytes_total < 16 * 1024, "{allocations:?}");
}
