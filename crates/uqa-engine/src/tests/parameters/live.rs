//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{rows, Json, Provider};
use crate::{Engine, PreparedStatement};
use std::collections::BTreeMap;
use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::{ColumnType, SQLError, SQLParam};

fn cursor_rows(engine: &Engine, sql: &str, parameters: &[SQLParam]) -> Result<Json, SQLError> {
    let mut cursor = engine.sql_cursor(sql, parameters)?;
    let types = cursor.column_types().to_vec();
    let mut rows = Vec::new();
    for batch in &mut cursor {
        for row in batch?.into_positional_rows() {
            rows.push(
                row.iter()
                    .zip(&types)
                    .map(|(value, ty)| match value {
                        Value::Null => Ok(Json::Null),
                        value => uqa_sql::result::format_postgres_text(
                            value,
                            ty.as_ref().unwrap(),
                            Some(engine),
                        )
                        .map(Json::from),
                    })
                    .collect::<Result<Json, SQLError>>()?,
            );
        }
    }
    Ok(Json::from(rows))
}

fn execute(
    engine: &Engine,
    prepared: &PreparedStatement,
    retained: &mut BTreeMap<String, SQLParam>,
    declarations: &mut BTreeMap<String, PreparedStatement>,
    case: &Json,
) -> Result<Json, SQLError> {
    if let Some(name) = case["retain"].as_str() {
        let value = engine.create_typed_parameter_with_control(
            case["type"].as_str().unwrap(),
            None,
            &ProductionControl::uncontrolled(),
        )?;
        retained.insert(name.into(), value.into_uncontrolled().unwrap());
        return Ok(serde_json::json!([[null]]));
    }
    if let Some(name) = case["prepare"].as_str() {
        let ty = retained[case["type_parameter"].as_str().unwrap()]
            .declared_scalar_type()
            .unwrap()
            .clone();
        let statement = engine.prepare_statement(case["sql"].as_str().unwrap(), &[ty])?;
        declarations.insert(name.into(), statement);
        return Ok(serde_json::json!([]));
    }
    if let Some(name) = case["execute_prepared"].as_str() {
        return engine
            .execute_prepared_statement(&declarations[name], &[SQLParam::Scalar(Value::Null)])
            .and_then(|result| rows(engine, &result));
    }
    let sql = case["sql"].as_str().unwrap();
    let params = case["parameter"]
        .as_str()
        .map(|name| std::slice::from_ref(&retained[name]))
        .unwrap_or_default();
    match case["method"].as_str() {
        Some("cursor") => cursor_rows(engine, sql, params),
        Some("prepared") => engine
            .execute_prepared_statement(prepared, params)
            .and_then(|result| rows(engine, &result)),
        _ => engine
            .sql(sql, params)
            .and_then(|result| rows(engine, &result)),
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn retained_parameter_declarations_match_live_postgresql_catalog(#[case] provider: Provider) {
    let reference: Json = serde_json::from_str(include_str!("live.pg18.json")).unwrap();
    assert!(reference["provenance"]["version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18."));
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("live.db"));
    let prepared = engine
        .prepare_statement("SELECT $1::text AS value", &[ColumnType::Text])
        .unwrap();
    let mut retained = BTreeMap::new();
    let mut declarations = BTreeMap::new();
    for (index, case) in reference["cases"].as_array().unwrap().iter().enumerate() {
        let actual = execute(&engine, &prepared, &mut retained, &mut declarations, case);
        match actual {
            Ok(rows) => {
                assert_eq!(case["sqlstate"], "", "case {index}: {case}");
                assert_eq!(rows, case["rows"], "case {index}: {case}");
            }
            Err(error) => {
                assert_eq!(
                    error.sqlstate().unwrap_or_default(),
                    case["sqlstate"].as_str().unwrap(),
                    "case {index}: {case}: {error}"
                );
                let mut expected = case["message"].as_str().unwrap().to_string();
                if let Some(name) = case["parameter"]
                    .as_str()
                    .or_else(|| case["type_parameter"].as_str())
                {
                    let ty = retained[name].declared_scalar_type().unwrap();
                    expected = expected.replace(
                        "{parameter_oid}",
                        &uqa_sql::catalog::type_metadata::pg_type_oid(ty).to_string(),
                    );
                }
                assert_eq!(error.to_string(), expected, "case {index}: {case}");
            }
        }
        let state = if engine.transaction_failed() {
            3
        } else if engine.transaction_depth() > 0 {
            2
        } else {
            0
        };
        assert_eq!(
            serde_json::json!(state),
            case["transaction_status"],
            "case {index}: {case}"
        );
    }
    engine.finalize_prepared_statement(&prepared);
    for statement in declarations.values() {
        engine.finalize_prepared_statement(statement);
    }
}
