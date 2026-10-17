//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::single_row_values_query;
use crate::ast::FunctionBinding;
use crate::plan::{CommandPlan, InsertPlan, RelationalPlan, UnifiedPlan};
use crate::{ColumnType, SQLError, ScalarExpr};
use std::collections::BTreeSet;

struct NoRoutines;

impl crate::FunctionTypeResolver for NoRoutines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl crate::routines::RoutineResolution for NoRoutines {}

fn insert(sql: &str) -> InsertPlan {
    let UnifiedPlan::Command(command) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("expected INSERT")
    };
    let CommandPlan::Insert(insert) = *command else {
        panic!("expected INSERT")
    };
    *insert
}

fn expand(sql: &str, columns: &[&str]) -> Option<InsertPlan> {
    let columns = columns.iter().map(ToString::to_string).collect::<Vec<_>>();
    single_row_values_query(
        &NoRoutines,
        &insert(sql),
        || Ok(Some(BTreeSet::new())),
        || Ok(Some(columns)),
    )
    .unwrap()
}

fn source_projections(insert: &InsertPlan) -> Vec<ScalarExpr> {
    let RelationalPlan::QueryBlock(block) = &insert.source.as_ref().unwrap().root else {
        panic!("expected a FROM-less query block")
    };
    assert!(block.from.is_none());
    block
        .projections
        .iter()
        .map(|projection| projection.expr.clone())
        .collect()
}

#[test]
fn single_values_rows_with_set_returning_calls_run_as_target_lists() {
    let expanded = expand("INSERT INTO n VALUES (generate_series(1, 3))", &["x"]).unwrap();
    assert!(expanded.rows.is_empty());
    assert!(expanded.columns.is_empty());
    assert_eq!(source_projections(&expanded).len(), 1);

    // DEFAULT leaves its column to the column default, as an omitted column does.
    let expanded = expand(
        "INSERT INTO m VALUES (DEFAULT, regexp_matches('aab', 'a', 'g'))",
        &["x", "y", "z"],
    )
    .unwrap();
    let columns = expanded
        .columns
        .iter()
        .map(|target| target.column.as_str())
        .collect::<Vec<_>>();
    assert_eq!(columns, ["y"]);
    assert_eq!(source_projections(&expanded).len(), 1);

    let expanded = expand(
        "INSERT INTO m (y, x) VALUES (generate_series(1, 2) + 1, DEFAULT) RETURNING *",
        &[],
    )
    .unwrap();
    let columns = expanded
        .columns
        .iter()
        .map(|target| target.column.as_str())
        .collect::<Vec<_>>();
    assert_eq!(columns, ["y"]);
    assert_eq!(expanded.returning.len(), 1);

    // A set-returning call inside a scalar subquery belongs to that subquery's own target list.
    let expanded = expand(
        "INSERT INTO n VALUES ((SELECT generate_series(1, 1)))",
        &["x"],
    );
    assert!(expanded.is_none());
    for sql in [
        "INSERT INTO n VALUES (1)",
        "INSERT INTO n VALUES (generate_series(1, 2)), (3)",
        "INSERT INTO n SELECT generate_series(1, 2)",
    ] {
        assert!(expand(sql, &["x"]).is_none(), "{sql}");
    }
    // A row without a set-returning call neither reads the target's rules nor its columns.
    let unexpanded = single_row_values_query(
        &NoRoutines,
        &insert("INSERT INTO n VALUES (1)"),
        || panic!("rules read for a row without a set-returning call"),
        || panic!("columns read for a row without a set-returning call"),
    )
    .unwrap();
    assert!(unexpanded.is_none());
}

#[test]
fn rewrite_rules_reading_a_set_returning_entry_fail_as_in_postgresql() {
    let statement = insert("INSERT INTO r VALUES (generate_series(1, 2), 5)");
    let columns = || Ok(Some(vec!["x".to_string(), "y".to_string()]));
    let error = |observed: Option<BTreeSet<String>>| {
        single_row_values_query(&NoRoutines, &statement, || Ok(observed), columns).unwrap_err()
    };
    // A rule that reads the whole NEW row, or the column a set-returning entry fills, evaluates that entry outside any target list.
    for observed in [None, Some(BTreeSet::from(["x".to_string()]))] {
        let error = error(observed);
        assert_eq!(error.sqlstate(), Some("0A000"));
        assert_eq!(
            error.to_string(),
            "set-valued function called in context that cannot accept a set"
        );
    }
    let unobserved = BTreeSet::from(["y".to_string()]);
    let expanded =
        single_row_values_query(&NoRoutines, &statement, || Ok(Some(unobserved)), columns)
            .unwrap()
            .unwrap();
    assert!(expanded.columns.is_empty());
    assert_eq!(source_projections(&expanded).len(), 2);
}
