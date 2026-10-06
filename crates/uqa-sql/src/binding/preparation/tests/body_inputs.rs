//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source-body analysis reads inputs in syntax order without running expressions.

use super::*;
use uqa_core::Value;

struct Aliases;

impl OidAliasInput for Aliases {
    fn resolve_oid_alias_input(
        &self,
        ty: &ColumnType,
        text: &str,
    ) -> Result<Option<i64>, SQLError> {
        assert_eq!(ty, &ColumnType::Regclass);
        Err(SQLError::Routine {
            sqlstate: "42P01".into(),
            message: format!("relation \"{text}\" does not exist"),
        })
    }
}

#[test]
fn body_input_errors_follow_relations_and_expression_positions() {
    let parameters = crate::binding::RoutineParameterScope::new(
        "f",
        vec!["v".into()],
        vec![Some(ColumnType::Integer)],
    );
    for (sql, state, message) in [
        (
            "SELECT 'absent'::regclass",
            "42P01",
            "relation \"absent\" does not exist",
        ),
        (
            "SELECT 'absent'::regclass, missing_column",
            "42P01",
            "relation \"absent\" does not exist",
        ),
        (
            "SELECT missing_column, 'absent'::regclass",
            "42703",
            "column \"missing_column\" does not exist",
        ),
        (
            "SELECT 'absent'::regclass FROM missing_source",
            "42P01",
            "relation \"missing_source\" does not exist",
        ),
        (
            "SELECT $2 FROM missing_source",
            "42P01",
            "relation \"missing_source\" does not exist",
        ),
        (
            "SELECT 'bad'::integer, $2",
            "22P02",
            "invalid input syntax for type integer: \"bad\"",
        ),
        (
            "SELECT $2, 'bad'::integer",
            "42P02",
            "there is no parameter $2",
        ),
        (
            "SELECT f.v + 'bad'",
            "22P02",
            "invalid input syntax for type integer: \"bad\"",
        ),
        (
            "SELECT CASE WHEN false THEN 'bad'::integer ELSE 1 END",
            "22P02",
            "invalid input syntax for type integer: \"bad\"",
        ),
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let error = analyze_routine_body_inputs(
            &NoRoutines,
            &plan,
            &[crate::SQLParam::typed_scalar(
                Value::Null,
                ColumnType::Integer,
            )],
            &assignment_context(),
            &Aliases,
            Some(&parameters),
        )
        .unwrap_err();
        assert_eq!(
            (error.sqlstate(), error.to_string()),
            (Some(state), message.into()),
            "{sql}"
        );
    }
}

#[test]
fn body_validation_keeps_source_constants_and_runtime_expressions_unexecuted() {
    let sql = "SELECT '12'::integer / 0, ('bad'::text)::integer";
    let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
    let original = serde_json::to_value(&plan).unwrap();
    analyze_routine_body_inputs(
        &NoRoutines,
        &plan,
        &[],
        &assignment_context(),
        &Aliases,
        None,
    )
    .unwrap();
    assert_eq!(serde_json::to_value(&plan).unwrap(), original);
}

fn analyze_with_named_parameter(
    sql: &str,
) -> Result<crate::binding::statements::AnalyzedResult, SQLError> {
    let parameters = crate::binding::RoutineParameterScope::new(
        "f",
        vec!["id".into()],
        vec![Some(ColumnType::Integer)],
    );
    let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
    analyze_routine_body_inputs(
        &NoRoutines,
        &plan,
        &[crate::SQLParam::typed_scalar(
            Value::Null,
            ColumnType::Integer,
        )],
        &assignment_context(),
        &Aliases,
        Some(&parameters),
    )
}

#[test]
fn body_limit_names_resolve_columns_before_parameters_at_the_same_query_level() {
    for (sql, clause) in [
        ("SELECT 1 FROM assignment_target LIMIT id", "LIMIT"),
        ("SELECT 1 FROM assignment_target OFFSET id", "OFFSET"),
        (
            "SELECT 1 FROM (SELECT 1 FROM assignment_target LIMIT id) s",
            "LIMIT",
        ),
    ] {
        let error = analyze_with_named_parameter(sql).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P10"), "{sql}: {error}");
        assert_eq!(
            error.to_string(),
            format!("argument of {clause} must not contain variables"),
        );
    }
    for sql in [
        "SELECT 1 LIMIT id",
        "SELECT 1 FROM assignment_target LIMIT f.id",
        "SELECT 1 FROM assignment_target outer_target WHERE EXISTS (SELECT 1 LIMIT outer_target.id)",
        "SELECT 1 AS id UNION ALL SELECT 2 LIMIT id",
    ] {
        analyze_with_named_parameter(sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn body_update_sources_retain_the_parameter_scope_without_seeing_target_columns() {
    let sql = "UPDATE assignment_target SET id=s.x FROM (SELECT id AS x) s WHERE assignment_target.id=7 RETURNING assignment_target.id";
    let result = analyze_with_named_parameter(sql).unwrap();
    let crate::binding::statements::AnalyzedResult::Schema(schema) = result else {
        panic!("UPDATE RETURNING schema");
    };
    assert_eq!(schema.column_type(0), Some(&ColumnType::Integer));
    let error = analyze_with_named_parameter(
        "UPDATE assignment_target SET id=1 FROM (SELECT legacy AS x) s RETURNING assignment_target.id",
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42703"));
    assert_eq!(error.to_string(), "column \"legacy\" does not exist");
}

#[test]
fn body_merge_sources_and_unmatched_actions_keep_the_routine_parameter_scope() {
    for sql in [
        "MERGE INTO assignment_target AS t USING (SELECT 99 AS k) s ON t.id=s.k WHEN NOT MATCHED THEN INSERT (id) VALUES (id) RETURNING t.id",
        "MERGE INTO assignment_target AS t USING (SELECT id AS k) s ON t.id=s.k WHEN NOT MATCHED THEN INSERT (id) VALUES (s.k) RETURNING t.id",
        "MERGE INTO assignment_target AS t USING (SELECT 99 AS k) s ON t.id=s.k WHEN NOT MATCHED BY SOURCE THEN UPDATE SET id=f.id RETURNING t.id",
    ] {
        analyze_with_named_parameter(sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}
