//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn row_stars_expand_before_retained_analysis_without_flattening_whole_rows() {
    for (sql, widths) in [
        ("SELECT ROW(s.*) FROM (SELECT)s", vec![0]),
        ("SELECT ROW(1,s.*,s) FROM (SELECT)s", vec![2]),
        ("SELECT ROW(s.*) FROM (VALUES(2,5))s(k,z)", vec![2]),
        ("SELECT ROW(s) FROM (VALUES(2,5))s(k,z)", vec![1]),
        (
            "SELECT ROW(7,s.*,ROW(s.*),s) FROM (VALUES(2,5))s(k,z)",
            vec![2, 5],
        ),
        (
            "SELECT ROW(s.*) AS r FROM (VALUES(2,5))s(k,z) GROUP BY r ORDER BY r",
            vec![2],
        ),
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        read_prepared_inputs(&NoRoutines, &mut plan, &[], &assignment_context(), None)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let mut actual = Vec::new();
        plan.rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::Row(items) = expression {
                assert!(
                    !items
                        .iter()
                        .any(|item| matches!(item, ScalarExpr::QualifiedStar(_))),
                    "{sql}"
                );
                actual.push(items.len());
            }
        });
        assert_eq!(actual, widths, "{sql}");
    }
}

#[test]
fn expanded_row_fields_retain_neighboring_parameter_and_literal_selections() {
    let mut plan = UnifiedPlan::lower(
        crate::compile("SELECT ROW(s.*,$1::int,'1'::int)=ROW(2,5,$1,1) FROM (VALUES(2,5))s(k,z)")
            .unwrap()
            .remove(0),
    );
    let types =
        infer_prepared_parameter_types(&NoRoutines, &plan, &[None], &assignment_context()).unwrap();
    assert_eq!(types, [Some(ColumnType::Integer)]);
    read_prepared_inputs(&NoRoutines, &mut plan, &types, &assignment_context(), None).unwrap();
    let mut parameters = 0;
    plan.rewrite_scalar_expressions(&mut |node| {
        parameters += usize::from(matches!(node, ScalarExpr::Param(1)));
    });
    assert_eq!(parameters, 2);
}
