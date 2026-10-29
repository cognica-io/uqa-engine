//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn all_scalar_copy_modes_preserve_child_shapes_and_empty_optional_groups() {
    let mut sources: Vec<_> = fixtures::child_shapes()
        .into_iter()
        .map(|(source, _)| source)
        .chain(
            fixtures::wide_keyed_calls()
                .into_iter()
                .map(|(source, _)| source),
        )
        .collect();
    sources.extend([
        ScalarExpr::Array(Vec::new()),
        ScalarExpr::Row(Vec::new()),
        ScalarExpr::And(Vec::new()),
        ScalarExpr::Or(Vec::new()),
        ScalarExpr::Case {
            base: None,
            when: Vec::new(),
            else_branch: None,
        },
    ]);
    // Empty intermediate groups must not skip a later filter, frame or CASE branch.
    for sql in [
        "SELECT sum(a) FILTER (WHERE b) OVER (ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)",
        "SELECT count(*) FILTER (WHERE b)",
        "SELECT CASE WHEN a THEN b END",
        "SELECT row_number() OVER ()",
    ] {
        let crate::Statement::Select(mut query) = crate::compile(sql).unwrap().remove(0) else {
            panic!("SELECT")
        };
        sources.push(crate::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar);
    }
    for source in sources {
        let source = ScalarExpr::Row(vec![source]);
        let expected = source.clone();
        let budget = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        for control in [
            ProductionControl::uncontrolled(),
            ProductionControl::new(&budget, &token, &token),
        ] {
            let mutable = source.copy_for_rewrite(&control).unwrap();
            let immutable = source.clone_with_control(&control).unwrap();
            assert_eq!(*mutable, expected);
            assert_eq!(*immutable, expected);
            assert_eq!(
                budget.used(),
                mutable.reserved_bytes() + immutable.reserved_bytes()
            );
            let frozen = mutable.finish(&control).unwrap();
            assert_eq!(*frozen, expected);
            assert_eq!(frozen.reserved_bytes(), immutable.reserved_bytes());
            drop(frozen);
            drop(immutable);
            assert_eq!(budget.used(), 0);
        }
        assert_eq!(source, expected);
    }
}
