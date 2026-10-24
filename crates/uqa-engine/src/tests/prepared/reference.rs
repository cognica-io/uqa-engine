//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{Engine, PreparedStatement};
use serde_json::{json, Value as Json};
use std::collections::BTreeMap;
use uqa_core::Value;
use uqa_sql::{ColumnType, SQLError, SQLParam, SQLResult, SQLResultKind};

fn empty_observation(status: u32) -> Json {
    json!({"status": status, "sqlstate": "", "message": "", "tag": "", "columns": [], "parameters": [], "rows": []})
}

fn observe_result(result: SQLResult) -> Result<Json, SQLError> {
    let mut observed = empty_observation(if result.kind == SQLResultKind::Rows {
        2
    } else {
        u32::from(result.command_tag.is_some())
    });
    observed["tag"] = json!(result.command_tag.unwrap_or_default());
    observed["columns"] = result
        .columns
        .iter()
        .zip(&result.column_types)
        .map(|(name, ty)| {
            json!([
                name,
                uqa_sql::catalog::result_type::postgres_result_type(ty.as_ref().unwrap()).type_oid
            ])
        })
        .collect();
    observed["rows"] = result
        .rows
        .iter()
        .map(|row| {
            result
                .columns
                .iter()
                .zip(&result.column_types)
                .map(|(name, ty)| match &row[name] {
                    Value::Null => Ok(Json::Null),
                    value => {
                        uqa_sql::result::format_postgres_text(value, ty.as_ref().unwrap(), None)
                            .map(Json::from)
                    }
                })
                .collect::<Result<Json, SQLError>>()
        })
        .collect::<Result<Json, SQLError>>()?;
    Ok(observed)
}

fn observe(
    engine: &Engine,
    handles: &mut BTreeMap<String, PreparedStatement>,
    case: &Json,
) -> Result<Json, SQLError> {
    match case["action"].as_str().unwrap() {
        "sql" => observe_result(engine.sql(case["sql"].as_str().unwrap(), &[])?),
        "prepare" => {
            let types = case["types"]
                .as_array()
                .unwrap()
                .iter()
                .map(|oid| {
                    assert_eq!(oid, 23);
                    ColumnType::Integer
                })
                .collect::<Vec<_>>();
            let handle = engine.prepare_statement(case["sql"].as_str().unwrap(), &types)?;
            handles.insert(case["name"].as_str().unwrap().into(), handle);
            Ok(empty_observation(1))
        }
        "describe" => {
            let description =
                engine.describe_prepared_statement(&handles[case["name"].as_str().unwrap()])?;
            let mut observed = empty_observation(1);
            if let Some(schema) = &description.result_schema {
                observed["columns"] = schema
                    .columns()
                    .iter()
                    .zip(schema.column_types())
                    .map(|(name, ty)| {
                        json!([
                            name,
                            uqa_sql::catalog::result_type::postgres_result_type(
                                ty.as_ref().unwrap()
                            )
                            .type_oid
                        ])
                    })
                    .collect();
            }
            observed["parameters"] = description
                .parameter_types
                .iter()
                .map(|ty| {
                    json!(ty.as_ref().map_or(0, |ty| {
                        uqa_sql::catalog::result_type::postgres_result_type(ty).type_oid
                    }))
                })
                .collect();
            Ok(observed)
        }
        "execute" => {
            let parameters = case["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| {
                    SQLParam::Scalar(
                        value
                            .as_str()
                            .map_or(Value::Null, |value| Value::Int(value.parse().unwrap())),
                    )
                })
                .collect::<Vec<_>>();
            observe_result(engine.execute_prepared_statement(
                &handles[case["name"].as_str().unwrap()],
                &parameters,
            )?)
        }
        other => panic!("unknown oracle operation: {other}"),
    }
}

pub(super) fn verify(engine: &Engine, reference: &Json) {
    let mut handles = BTreeMap::new();
    for (index, case) in reference["cases"].as_array().unwrap().iter().enumerate() {
        let mut actual = observe(engine, &mut handles, case).unwrap_or_else(|error| {
            let mut actual = empty_observation(7);
            actual["sqlstate"] = json!(error.sqlstate().unwrap());
            actual["message"] = json!(error.to_string());
            actual
        });
        actual["transaction_status"] = json!(if engine.transaction_failed() {
            3
        } else if engine.transaction_depth() != 0 {
            2
        } else {
            0
        });
        assert_eq!(actual, case["result"], "reference case {index}: {case}");
    }
    for handle in handles.values() {
        engine.finalize_prepared_statement(handle);
    }
    assert!(engine.session.prepared.read().is_empty());
}
