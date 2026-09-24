//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{RelationalPlan, UnifiedPlan};
use crate::routines::RoutineResolution;

struct Catalog;

impl FunctionTypeResolver for Catalog {
    fn resolve_function_type(
        &self,
        _name: &str,
        _binding: Option<&crate::ast::FunctionBinding>,
        _argument_names: &[Option<String>],
        _argument_types: &[Option<ColumnType>],
        _explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl RoutineResolution for Catalog {}

fn block(sql: &str) -> QueryBlockPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("query block")
    };
    *block
}

fn input() -> RowSchema {
    RowSchema::with_types(
        vec!["id".into(), "n".into()],
        vec![Some(ColumnType::Integer); 2],
    )
}

#[test]
fn grouping_input_names_precede_aggregate_and_scalar_output_aliases() {
    for sql in [
        "SELECT min(id) AS first_id, count(*) AS n FROM t GROUP BY n",
        "SELECT min(id) AS first_id, 0 AS n FROM t GROUP BY n",
    ] {
        let original = block(sql);
        assert!(prepare_grouping_sets(&Catalog, &original, &input(), &[])
            .unwrap()
            .is_none());
        assert!(matches!(original.group_by.as_slice(), [ScalarExpr::Column(name)] if name == "n"));
    }
}

#[test]
fn grouping_aliases_resolve_before_distinct_sets_and_storage() {
    let original = block("SELECT n + 1 AS shifted, count(*) FROM t GROUP BY DISTINCT GROUPING SETS ((shifted), (n + 1))");
    let prepared = prepare_grouping_sets(&Catalog, &original, &input(), &[])
        .unwrap()
        .unwrap();
    assert_eq!(prepared.grouping_sets.len(), 1);
    assert_eq!(
        prepared.grouping_sets[0],
        [original.projections[0].expr.clone()]
    );
    let mut stored = original;
    assert!(bind_grouping_names(&Catalog, &mut stored, &input(), &[]).unwrap());
    assert!(!bind_grouping_names(&Catalog, &mut stored, &input(), &[]).unwrap());
    assert_eq!(stored.grouping_sets[0], stored.grouping_sets[1]);
}

#[test]
fn grouping_duplicate_aliases_use_analyzed_expression_identity() {
    let identical = block("SELECT n AS x, n AS x FROM t GROUP BY x");
    assert!(prepare_grouping_sets(&Catalog, &identical, &input(), &[])
        .unwrap()
        .is_some());
    for sql in [
        "SELECT id AS x, n AS x FROM t GROUP BY x",
        "SELECT 1 AS x, 1.0 AS x GROUP BY x",
    ] {
        assert_eq!(
            prepare_grouping_sets(&Catalog, &block(sql), &input(), &[])
                .unwrap_err()
                .sqlstate(),
            Some("42702"),
            "{sql}"
        );
    }
}

#[test]
fn grouping_aliases_retain_aggregate_and_window_context_errors() {
    for (sql, state) in [
        ("SELECT count(*) AS tally FROM t GROUP BY tally", "42803"),
        (
            "SELECT row_number() OVER () AS rank FROM t GROUP BY rank",
            "42P20",
        ),
    ] {
        assert_eq!(
            prepare_grouping_sets(&Catalog, &block(sql), &input(), &[])
                .unwrap_err()
                .sqlstate(),
            Some(state),
            "{sql}"
        );
    }
}

#[test]
fn grouping_names_inside_expressions_are_never_output_aliases() {
    let original = block("SELECT n + 1 AS shifted FROM t GROUP BY shifted + 1");
    assert!(prepare_grouping_sets(&Catalog, &original, &input(), &[])
        .unwrap()
        .is_none());
}
