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
            bound_condition_queries: None,
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
            bound_condition_queries: None,
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
    assert_collation_references(&NoRoutines, sources);
}

pub(super) fn assert_collation_references(
    routines: &dyn RoutineResolution,
    sources: &[(&str, usize)],
) {
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
            bound_condition_queries: None,
            routines,
            scope: &mut SchemaScope::for_analysis(&binding).unwrap(),
            parameters: ParameterTypes::new(&[]),
            schema_expression: None,
        };
        let prepared = preparation.query(&query, None);
        let ordinary =
            crate::binding::analyze_query_plan_schema(routines, &query, &[], &binding, None);
        let mut results = vec![("prepared", prepared), ("ordinary", ordinary)];
        if case.error.is_none() {
            results.push((
                "bound",
                crate::binding::bind_query_plan_schema(routines, &query, &[], &binding, None),
            ));
        }
        for (entry, result) in results {
            if let Some(expected) = &case.error {
                let error = result
                    .err()
                    .unwrap_or_else(|| panic!("{entry}: expected PostgreSQL error: {}", case.sql));
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
        bound_condition_queries: None,
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
        for (sql, copies) in [
            (format!("SELECT {expression}"), 1),
            (format!("SELECT {expression} AS value ORDER BY value"), 1),
            (format!("SELECT {expression} ORDER BY 1"), 1),
            (format!("SELECT DISTINCT {expression}"), 1),
            (
                format!("SELECT DISTINCT ON (value) {expression} AS value ORDER BY value"),
                1,
            ),
            (format!("VALUES ({expression}),({expression})"), 2),
            (
                format!("SELECT {expression} UNION ALL SELECT {expression}"),
                2,
            ),
            (
                format!(
                    "SELECT {expression} UNION ALL SELECT {expression} UNION SELECT {expression}"
                ),
                3,
            ),
        ] {
            let UnifiedPlan::Query(query) =
                UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0))
            else {
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
            assert_eq!(
                calls.0.load(Ordering::Relaxed),
                copies * expected_calls,
                "{sql}"
            );
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
                2 * copies * expected_calls,
                "{sql}"
            );
        }
    }
}

#[test]
fn observed_call_types_preserve_reference_validation() {
    assert_computed_collation_references(&[(include_str!("call_reference_errors.json"), 24)]);
}

#[test]
fn values_collations_and_column_diagnostics_match_postgresql() {
    assert_computed_collation_references(&[(include_str!("values_collations.json"), 19)]);
}

#[test]
fn set_collations_and_column_diagnostics_match_postgresql() {
    assert_computed_collation_references(&[(include_str!("set_collations.json"), 61)]);
}

#[test]
fn sort_group_key_collations_and_diagnostic_order_match_postgresql() {
    assert_computed_collation_references(&[(include_str!("key_collations.json"), 39)]);
}

#[test]
fn executable_collation_selections_survive_reanalysis() {
    let context = binding();
    let references: BTreeMap<String, Vec<Reference>> =
        serde_json::from_str(include_str!("../../tests/query_collations.json")).unwrap();
    for case in references.values().flatten() {
        let mut plan = UnifiedPlan::lower(crate::compile(&case.sql).unwrap().remove(0));
        for _ in 0..2 {
            read_prepared_inputs(&NoRoutines, &mut plan, &[], &context, None)
                .unwrap_or_else(|error| panic!("{}: {error}", case.sql));
            let schema = crate::prepared::analyze_prepared_plan(&NoRoutines, &plan, &[], &context)
                .unwrap_or_else(|error| panic!("{}: {error}", case.sql))
                .unwrap();
            assert_eq!(
                (0..schema.len())
                    .map(|i| schema.column_collation(i).unwrap())
                    .collect::<Vec<_>>(),
                case.collations,
                "{}",
                case.sql
            );
        }
    }
}

#[test]
fn returning_wildcards_keep_original_inputs_and_declared_scopes() {
    let context = binding();
    for (sql, expected) in [
        (
            "INSERT INTO t VALUES ('a','b',1) RETURNING *, upper(c), lower('literal'), old.*, new.*",
            vec![950, 951, 0, 950, 100, 950, 951, 0, 950, 951, 0],
        ),
        (
            "UPDATE t SET n=n RETURNING *, upper(c), lower('literal'), old.*, new.*",
            vec![950, 951, 0, 950, 100, 950, 951, 0, 950, 951, 0],
        ),
        (
            "DELETE FROM t RETURNING *, upper(c), lower('literal'), old.*, new.*",
            vec![950, 951, 0, 950, 100, 950, 951, 0, 950, 951, 0],
        ),
        (
            "MERGE INTO local t USING remote s ON t.n=s.n WHEN MATCHED THEN UPDATE SET n=t.n RETURNING *, merge_action(), upper(t.c), old.*, new.*",
            vec![950, 951, 0, 950, 951, 0, 100, 950, 950, 951, 0, 950, 951, 0],
        ),
        (
            "MERGE INTO local t USING remote s ON t.n=s.n WHEN MATCHED THEN UPDATE SET n=t.n RETURNING WITH (OLD AS before, NEW AS after) s.*, before.*, after.*, merge_action(), upper(t.c)",
            vec![950, 951, 0, 950, 951, 0, 950, 951, 0, 100, 950],
        ),
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        for _ in 0..2 {
            read_prepared_inputs(&NoRoutines, &mut plan, &[], &context, None)
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            let schema = crate::prepared::analyze_prepared_plan(&NoRoutines, &plan, &[], &context)
                .unwrap_or_else(|error| panic!("{sql}: {error}"))
                .unwrap();
            assert_eq!(
                (0..schema.len())
                    .map(|position| schema.column_collation(position).unwrap())
                    .collect::<Vec<_>>(),
                expected,
                "{sql}: {:?}",
                schema.columns()
            );
        }
    }
}

#[test]
fn returning_wildcards_preserve_cte_output_and_zero_column_diagnostics() {
    let context = binding();
    let mut plan = UnifiedPlan::lower(
        crate::compile(
            "WITH changed AS (
                UPDATE t SET n=n RETURNING *, upper(c) AS u, lower('literal') AS literal
             ) SELECT * FROM changed",
        )
        .unwrap()
        .remove(0),
    );
    read_prepared_inputs(&NoRoutines, &mut plan, &[], &context, None).unwrap();
    let schema = crate::prepared::analyze_prepared_plan(&NoRoutines, &plan, &[], &context)
        .unwrap()
        .unwrap();
    assert_eq!(schema.columns(), ["c", "p", "n", "u", "literal"]);
    assert_eq!(
        (0..schema.len())
            .map(|position| schema.column_collation(position).unwrap())
            .collect::<Vec<_>>(),
        [950, 951, 0, 950, 100]
    );

    let context = BindingContext {
        catalog: crate::binding::fixture::catalog(BTreeMap::from([(
            RelationIdentity::new("public", "empty_target"),
            crate::binding::fixture::table_definition(Vec::new()),
        )])),
        ..crate::binding::fixture::empty_binding_context()
    };
    for command in [
        "INSERT INTO empty_target DEFAULT VALUES",
        "DELETE FROM empty_target",
        "MERGE INTO empty_target t USING empty_target s ON true WHEN MATCHED THEN DELETE",
    ] {
        for (returning, valid) in [("*", false), ("*, 1 AS one", true)] {
            let sql = format!("{command} RETURNING {returning}");
            let mut plan = UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0));
            let input = read_prepared_inputs(&NoRoutines, &mut plan, &[], &context, None);
            let result = crate::prepared::analyze_prepared_plan(&NoRoutines, &plan, &[], &context);
            if valid {
                input.unwrap_or_else(|error| panic!("{sql}: {error}"));
                let schema = result
                    .unwrap_or_else(|error| panic!("{sql}: {error}"))
                    .unwrap();
                assert_eq!(schema.columns(), ["one"], "{sql}");
                assert_eq!(schema.column_type(0), Some(&ColumnType::Integer), "{sql}");
            } else {
                for error in [input.unwrap_err(), result.unwrap_err()] {
                    assert_eq!(error.sqlstate(), Some("42601"), "{sql}: {error}");
                    assert_eq!(error.to_string(), "RETURNING must have at least one column");
                }
            }
        }
    }
}

#[test]
fn returning_collation_selections_keep_original_plan_addresses() {
    let context = binding();
    for source in [
        include_str!("../../tests/dml_collations.json"),
        include_str!("computed_dml_collations.json"),
    ] {
        let references: BTreeMap<String, Vec<Reference>> = serde_json::from_str(source).unwrap();
        for case in references.values().flatten() {
            let mut plan = UnifiedPlan::lower(crate::compile(&case.sql).unwrap().remove(0));
            for _ in 0..2 {
                read_prepared_inputs(&NoRoutines, &mut plan, &[], &context, None)
                    .unwrap_or_else(|error| panic!("{}: {error}", case.sql));
                let schema =
                    crate::prepared::analyze_prepared_plan(&NoRoutines, &plan, &[], &context)
                        .unwrap_or_else(|error| panic!("{}: {error}", case.sql))
                        .unwrap();
                assert_eq!(
                    (0..schema.len())
                        .map(|i| schema.column_collation(i).unwrap())
                        .collect::<Vec<_>>(),
                    case.collations,
                    "{}",
                    case.sql
                );
            }
        }
    }
}

#[test]
fn stored_view_coercions_preserve_prepared_result_collations() {
    struct Aliases;
    impl OidAliasInput for Aliases {
        fn resolve_oid_alias_input(
            &self,
            _: &ColumnType,
            _: &str,
        ) -> Result<Option<i64>, SQLError> {
            panic!("view has no OID alias constants")
        }
    }
    let context = binding();
    for (input, oid, ty, has_cast) in [
        (
            "(c COLLATE \"C\")::varchar(5)",
            950,
            ColumnType::Varchar(Some(5)),
            true,
        ),
        (
            "((c COLLATE \"C\") COLLATE \"POSIX\")::varchar(5)",
            951,
            ColumnType::Varchar(Some(5)),
            true,
        ),
        (
            "((c COLLATE \"C\") COLLATE \"POSIX\")::text",
            951,
            ColumnType::Text,
            false,
        ),
    ] {
        let sql = format!("CREATE VIEW v AS SELECT {input} AS value FROM t");
        let mut plan = UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0));
        read_executable_inputs(&NoRoutines, &mut plan, &[], &context, &Aliases).unwrap();
        let UnifiedPlan::Command(command) = &mut plan else {
            panic!("view command")
        };
        let crate::plan::CommandPlan::CreateView { query, .. } = command.as_mut() else {
            panic!("view query")
        };
        let (_, selected_type, selection) = retained_view_projection(query);
        assert_eq!(selected_type, &ty);
        let selection = *selection;
        for _ in 0..2 {
            let schema = crate::binding::bind_query_plan_routines_for_storage(
                &NoRoutines,
                query,
                &[],
                &context,
                None,
            )
            .unwrap();
            assert_eq!(schema.column_type(0), Some(&ty), "{input}");
            assert_eq!(schema.column_collation(0), Some(oid), "{input}");
            let (inner, selected_type, retained) = retained_view_projection(query);
            assert_eq!((selected_type, retained), (&ty, &selection), "{input}");
            let clause = crate::catalog::collations::expression::scalar_clause(inner)
                .unwrap()
                .unwrap_or_else(|| panic!("outer COLLATE: {input}: {inner:?}"));
            assert_eq!(clause.oid, Some(oid), "{input}");
            assert_eq!(
                matches!(clause.argument, ScalarExpr::Cast { .. }),
                has_cast,
                "{input}"
            );
            clause.argument.visit(&mut |node| {
                assert!(
                    crate::catalog::collations::expression::scalar_clause(node)
                        .unwrap()
                        .is_none(),
                    "nested COLLATE: {input}"
                );
            });
            let serialized = serde_json::to_vec(query).unwrap();
            *query = serde_json::from_slice(&serialized).unwrap();
        }
    }
}

fn retained_view_projection(
    query: &QueryPlan,
) -> (&ScalarExpr, &ColumnType, &crate::ir::ResultCollation) {
    let crate::plan::RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("view query block")
    };
    let ScalarExpr::Collated {
        expr,
        ty,
        collation,
    } = &block.projections[0].expr
    else {
        panic!("prepared result collation")
    };
    (expr, ty, collation)
}

#[test]
fn retained_case_output_modifiers_follow_every_coerced_result_arm() {
    #[derive(serde::Deserialize)]
    struct Case {
        sql: String,
        oid: i64,
        modifier: i64,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!("case_modifiers.json")).unwrap();
    assert_eq!(cases.len(), 9);
    let context = crate::binding::fixture::empty_binding_context();
    for case in cases {
        let mut plan = UnifiedPlan::lower(crate::compile(&case.sql).unwrap().remove(0));
        for _ in 0..2 {
            read_prepared_inputs(&NoRoutines, &mut plan, &[], &context, None)
                .unwrap_or_else(|error| panic!("{}: {error}", case.sql));
            let schema = crate::prepared::analyze_prepared_plan(&NoRoutines, &plan, &[], &context)
                .unwrap_or_else(|error| panic!("{}: {error}", case.sql))
                .unwrap();
            let ty = schema.column_type(0).expect("CASE result type");
            assert_eq!(
                crate::catalog::type_metadata::pg_type_oid(ty),
                case.oid,
                "{}",
                case.sql
            );
            assert_eq!(
                crate::catalog::type_metadata::pg_type_modifier(ty),
                case.modifier,
                "{}",
                case.sql
            );
            if crate::schema::composites::type_is_collatable(ty) {
                assert_eq!(
                    schema.column_collation(0),
                    Some(if case.sql.contains("COLLATE") {
                        950
                    } else {
                        100
                    }),
                    "{}",
                    case.sql
                );
            }
            let serialized = serde_json::to_vec(&plan).unwrap();
            plan = serde_json::from_slice(&serialized).unwrap();
        }
    }
}
