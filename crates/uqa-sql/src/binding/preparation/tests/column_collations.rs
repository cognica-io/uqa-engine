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
            scope: SchemaScope::for_analysis(&binding).unwrap(),
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

#[test]
fn prepared_dml_output_collations_match_postgresql_with_null_row_images() {
    let binding = binding();
    let references: BTreeMap<String, Vec<Reference>> =
        serde_json::from_str(include_str!("../../tests/dml_collations.json")).unwrap();
    for case in references.values().flatten() {
        let plan = UnifiedPlan::lower(crate::compile(&case.sql).unwrap().remove(0));
        let mut preparation = Preparation {
            routines: &NoRoutines,
            scope: SchemaScope::for_analysis(&binding).unwrap(),
            parameters: ParameterTypes::new(&[]),
            schema_expression: None,
        };
        let output = match plan {
            UnifiedPlan::Command(command) => preparation.command(&command).map(Option::unwrap),
            UnifiedPlan::Query(query) => preparation.query(&query, None),
        }
        .unwrap_or_else(|error| panic!("{}: {error}", case.sql));
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

#[test]
fn computed_collations_and_deferred_diagnostics_match_postgresql() {
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
    let mut references: Vec<Case> =
        serde_json::from_str(include_str!("computed_collations.json")).unwrap();
    assert_eq!(references.len(), 24);
    let errors: Vec<Case> =
        serde_json::from_str(include_str!("computed_collation_errors.json")).unwrap();
    assert_eq!(errors.len(), 14);
    references.extend(errors);
    for case in references {
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(crate::compile(&case.sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let mut preparation = Preparation {
            routines: &NoRoutines,
            scope: SchemaScope::for_analysis(&binding).unwrap(),
            parameters: ParameterTypes::new(&[]),
            schema_expression: None,
        };
        let result = preparation.query(&query, None);
        if let Some(expected) = case.error {
            let error = result.unwrap_err();
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
        scope: SchemaScope::for_analysis(&binding).unwrap(),
        parameters: ParameterTypes::new(&[]),
        schema_expression: None,
    };
    let output = preparation.query(&query, None).unwrap();
    assert_eq!(output.column_collation(0), Some(0));
    let expected: crate::schema::RecordFields =
        vec![Some(ColumnType::Integer), Some(ColumnType::Text)].into();
    assert_eq!(output.record_fields(0), Some(&expected));
}
