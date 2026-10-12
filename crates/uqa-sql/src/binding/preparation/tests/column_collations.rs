//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared output metadata remains independent of the presence of a result row.

use super::*;

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    collations: Vec<u32>,
}

fn binding() -> BindingContext<'static> {
    let crate::Statement::CreateTable(mut table) =
        crate::compile("CREATE TABLE t (c text, p text, n integer)")
            .unwrap()
            .remove(0)
    else {
        panic!("table declaration")
    };
    table.columns[0].collation_oid = Some(950);
    table.columns[1].collation_oid = Some(951);
    let table = crate::binding::fixture::table_definition(table.columns);
    BindingContext {
        catalog: crate::binding::fixture::catalog(BTreeMap::from([
            (RelationIdentity::new("public", "local"), table.clone()),
            (RelationIdentity::new("public", "remote"), table.clone()),
            (RelationIdentity::new("public", "t"), table),
        ])),
        ..crate::binding::fixture::empty_binding_context()
    }
}

#[test]
fn prepared_outputs_retain_direct_and_explicit_collations_after_aliasing() {
    let binding = binding();
    let references: BTreeMap<String, Vec<Reference>> =
        serde_json::from_str(include_str!("../../tests/query_collations.json")).unwrap();
    for case in references.values().flatten() {
        let sql = &case.sql;
        let expected = &case.collations;
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let mut preparation = Preparation {
            routines: &NoRoutines,
            scope: &mut SchemaScope::for_analysis(&binding).unwrap(),
            parameters: ParameterTypes::new(&[]),
            schema_expression: None,
        };
        let output = preparation
            .query(&query, None)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(output.len(), expected.len(), "{sql}");
        for (position, oid) in expected.iter().enumerate() {
            assert_eq!(output.column_collation(position), Some(*oid), "{sql}");
        }
    }
}

fn assert_dml_collations(reference: &str) {
    let binding = binding();
    let references: BTreeMap<String, Vec<Reference>> = serde_json::from_str(reference).unwrap();
    for case in references.values().flatten() {
        let plan = UnifiedPlan::lower(crate::compile(&case.sql).unwrap().remove(0));
        let mut preparation = Preparation {
            routines: &NoRoutines,
            scope: &mut SchemaScope::for_analysis(&binding).unwrap(),
            parameters: ParameterTypes::new(&[]),
            schema_expression: None,
        };
        let outputs = match &plan {
            UnifiedPlan::Command(command) => vec![
                preparation.command(command).map(Option::unwrap),
                crate::binding::analyze_prepared_command_schema(
                    &NoRoutines,
                    command,
                    &[],
                    &binding,
                )
                .map(Option::unwrap),
            ],
            UnifiedPlan::Query(query) => vec![
                preparation.query(query, None),
                crate::binding::analyze_query_plan_schema(&NoRoutines, query, &[], &binding, None),
                crate::binding::bind_query_plan_schema(&NoRoutines, query, &[], &binding, None),
            ],
        };
        for output in outputs {
            let output = output.unwrap_or_else(|error| panic!("{}: {error}", case.sql));
            assert_eq!(
                output.len(),
                case.collations.len(),
                "{}: {:?}",
                case.sql,
                output.columns()
            );
            for (position, oid) in case.collations.iter().enumerate() {
                assert_eq!(
                    output.column_collation(position),
                    Some(*oid),
                    "{}",
                    case.sql
                );
            }
        }
    }
}

#[test]
fn prepared_dml_output_collations_match_postgresql_with_null_row_images() {
    assert_dml_collations(include_str!("../../tests/dml_collations.json"));
}

#[test]
fn computed_returning_collations_match_postgresql() {
    assert_dml_collations(include_str!("computed_dml_collations.json"));
}

fn assert_computed_collation_references(sources: &[(&str, usize)]) {
    #[derive(serde::Deserialize)]
    struct Case {
        sql: String,
        #[serde(default)]
        collations: Vec<u32>,
        error: Option<serde_json::Value>,
    }
    let crate::Statement::CreateTable(mut table) =
        crate::compile("CREATE TABLE t (c text, p text, plain text, n integer)")
            .unwrap()
            .remove(0)
    else {
        panic!("table")
    };
    table.columns[0].collation_oid = Some(950);
    table.columns[1].collation_oid = Some(951);
    let binding = BindingContext {
        catalog: crate::binding::fixture::catalog(BTreeMap::from([(
            RelationIdentity::new("public", "t"),
            crate::binding::fixture::table_definition(table.columns),
        )])),
        ..crate::binding::fixture::empty_binding_context()
    };
    let references = sources.iter().flat_map(|(source, count)| {
        let references: Vec<Case> = serde_json::from_str(source).unwrap();
        assert_eq!(references.len(), *count);
        references
    });
    for case in references {
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(crate::compile(&case.sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let mut preparation = Preparation {
            routines: &NoRoutines,
            scope: &mut SchemaScope::for_analysis(&binding).unwrap(),
            parameters: ParameterTypes::new(&[]),
            schema_expression: None,
        };
        let prepared = preparation.query(&query, None);
        let ordinary =
            crate::binding::analyze_query_plan_schema(&NoRoutines, &query, &[], &binding, None);
        let mut results = vec![prepared, ordinary];
        if case.error.is_none() {
            results.push(crate::binding::bind_query_plan_schema(
                &NoRoutines,
                &query,
                &[],
                &binding,
                None,
            ));
        }
        for result in results {
            if let Some(expected) = &case.error {
                let error = result
                    .err()
                    .unwrap_or_else(|| panic!("expected PostgreSQL error: {}", case.sql));
                assert_eq!(
                    error.sqlstate(),
                    expected["sqlstate"].as_str(),
                    "{}",
                    case.sql
                );
                assert_eq!(
                    error.to_string(),
                    expected["message"].as_str().unwrap(),
                    "{}",
                    case.sql
                );
                assert_eq!(error.detail(), expected["detail"].as_str(), "{}", case.sql);
                assert_eq!(error.hint(), expected["hint"].as_str(), "{}", case.sql);
            } else {
                let output = result.unwrap_or_else(|error| panic!("{}: {error}", case.sql));
                assert_eq!(output.len(), case.collations.len(), "{}", case.sql);
                for (position, oid) in case.collations.iter().enumerate() {
                    assert_eq!(
                        output.column_collation(position),
                        Some(*oid),
                        "{}",
                        case.sql
                    );
                }
            }
        }
    }
}

#[test]
fn computed_collations_and_deferred_diagnostics_match_postgresql() {
    assert_computed_collation_references(&[
        (include_str!("computed_collations.json"), 24),
        (include_str!("computed_collation_errors.json"), 14),
    ]);
}

#[test]
fn window_collations_and_independent_filters_match_postgresql() {
    assert_computed_collation_references(&[(include_str!("window_collations.json"), 19)]);
}

#[test]
fn computed_collations_keep_anonymous_record_field_metadata() {
    let binding = binding();
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        crate::compile("SELECT CASE WHEN n=1 THEN ROW(n,c) ELSE ROW(n,p) END AS r FROM t")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let mut preparation = Preparation {
        routines: &NoRoutines,
        scope: &mut SchemaScope::for_analysis(&binding).unwrap(),
        parameters: ParameterTypes::new(&[]),
        schema_expression: None,
    };
    let output = preparation.query(&query, None).unwrap();
    assert_eq!(output.column_collation(0), Some(0));
    let expected: crate::schema::RecordFields =
        vec![Some(ColumnType::Integer), Some(ColumnType::Text)].into();
    assert_eq!(output.record_fields(0), Some(&expected));
}

#[test]
fn output_metadata_visits_nested_call_arguments_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Calls(AtomicUsize);
    impl FunctionTypeResolver for Calls {
        fn resolve_function_type(
            &self,
            name: &str,
            _: Option<&FunctionBinding>,
            _: &[Option<String>],
            _: &[Option<ColumnType>],
            _: bool,
        ) -> Result<Option<ColumnType>, SQLError> {
            if name == "application.identity" {
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok(Some(ColumnType::Integer))
            } else {
                Ok(None)
            }
        }
    }
    impl RoutineResolution for Calls {}
    for (wrapper, expected_type, expected_calls) in [
        ("round", ColumnType::DoublePrecision, 1),
        ("application.identity", ColumnType::Integer, 17),
    ] {
        let mut expression = "application.identity(1)".to_owned();
        for _ in 0..16 {
            expression = format!("{wrapper}({expression})");
        }
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(
            crate::compile(&format!("SELECT {expression}"))
                .unwrap()
                .remove(0),
        ) else {
            panic!("query");
        };
        let calls = Calls(AtomicUsize::new(0));
        let output = crate::binding::bind_query_plan_schema(
            &calls,
            &query,
            &[],
            &crate::binding::fixture::empty_binding_context(),
            None,
        )
        .unwrap();
        assert_eq!(output.column_type(0), Some(&expected_type));
        assert_eq!(calls.0.load(Ordering::Relaxed), expected_calls, "{wrapper}");
        calls.0.store(0, Ordering::Relaxed);
        let output = crate::binding::analyze_query_plan_schema(
            &calls,
            &query,
            &[],
            &crate::binding::fixture::empty_binding_context(),
            None,
        )
        .unwrap();
        assert_eq!(output.column_type(0), Some(&expected_type));
        // Each call resolves its own signature and result; already analyzed children do not resolve again.
        assert_eq!(
            calls.0.load(Ordering::Relaxed),
            2 * expected_calls,
            "{wrapper}"
        );
    }
}

#[test]
fn observed_call_types_preserve_reference_validation() {
    assert_computed_collation_references(&[(include_str!("call_reference_errors.json"), 24)]);
}
