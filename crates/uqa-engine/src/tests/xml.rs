//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::providers::Provider;
use crate::{Engine, Value};
use serde_json::{json, Value as Json};
use uqa_sql::{
    catalog::result_type::postgres_result_type, result::format_postgres_text, ColumnType,
};

fn reference() -> Json {
    serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/xml_input.expected.json"
    ))
    .unwrap()
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn xml_input_casts_and_statement_lifecycle_match_postgresql(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("xml.db");
    let engine = provider.open(&path);
    let reference = reference();
    for case in reference["cases"].as_array().unwrap() {
        verify_input(&engine, case);
    }
    for case in reference["statements"].as_array().unwrap() {
        verify_statement(&engine, case);
    }
}

fn verify_input(engine: &Engine, case: &Json) {
    engine
        .sql(
            &format!("SET xmloption TO {}", case["mode"].as_str().unwrap()),
            &[],
        )
        .unwrap();
    let input = case["input"].as_str().unwrap();
    engine.take_sql_notices();
    let sql = format!("SELECT '{}'::xml AS value", input.replace('\'', "''"));
    match engine.sql(&sql, &[]) {
        Ok(result) => {
            assert!(case.get("sqlstate").is_none(), "{case}");
            assert_eq!(result.column_types, vec![Some(ColumnType::Xml)]);
            let value = result.value_at(0, 0).unwrap();
            assert_eq!(value, &Value::Str(input.into()));
            let output = format_postgres_text(value, &ColumnType::Xml, Some(engine)).unwrap();
            assert_eq!(output, case["output"].as_str().unwrap(), "{case}");
        }
        Err(error) => {
            assert_eq!(
                error.sqlstate(),
                case["sqlstate"].as_str(),
                "{case}: {error}"
            );
            assert_eq!(
                error.to_string(),
                case["message"].as_str().unwrap(),
                "{case}"
            );
        }
    }
    let notices = engine
        .take_sql_notices()
        .into_iter()
        .map(|notice| {
            assert_eq!(notice.level, uqa_sql::NoticeLevel::Warning);
            json!({"sqlstate": notice.sqlstate, "message": notice.message})
        })
        .collect::<Vec<_>>();
    assert_eq!(json!(notices), case["notices"], "{case}");
}

fn verify_statement(engine: &Engine, case: &Json) {
    let sql = case["sql"].as_str().unwrap();
    let mut observed = match engine.sql(sql, &[]) {
        Ok(result) => {
            let types = result
                .column_types
                .iter()
                .map(|ty| postgres_result_type(ty.as_ref().unwrap()).type_oid)
                .collect::<Vec<_>>();
            let rows = (0..result.rows.len())
                .map(|row| {
                    (0..result.columns.len())
                        .map(|column| {
                            let value = result.value_at(row, column).unwrap();
                            if matches!(value, Value::Null) {
                                None
                            } else {
                                Some(
                                    format_postgres_text(
                                        value,
                                        result.column_types[column].as_ref().unwrap(),
                                        Some(engine),
                                    )
                                    .unwrap(),
                                )
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            json!({"types": types, "rows": rows})
        }
        Err(error) => {
            json!({"sqlstate": error.sqlstate().unwrap(), "message": error.to_string()})
        }
    };
    observed["sql"] = json!(sql);
    observed["notices"] = json!(engine
        .take_sql_notices()
        .into_iter()
        .map(|notice| { json!({"sqlstate": notice.sqlstate, "message": notice.message}) })
        .collect::<Vec<_>>());
    observed["transaction_status"] = json!(if engine.transaction_failed() {
        3
    } else if engine.transaction_depth() != 0 {
        2
    } else {
        0
    });
    assert_eq!(observed, *case, "{sql}");
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn xml_types_and_admitted_values_survive_reopen_and_rollback(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("xml-durable.db");
    {
        let engine = provider.open(&path);
        engine.sql("CREATE TABLE xml_durable (id integer, value xml, items xml[]); INSERT INTO xml_durable VALUES (1, 'text<a/>', ARRAY['<a/>'::xml, 'text'::xml])", &[]).unwrap();
        engine
            .sql(
                "BEGIN; INSERT INTO xml_durable VALUES (2, '<b/>', NULL); ROLLBACK",
                &[],
            )
            .unwrap();
        engine.close().unwrap();
    }
    for _ in 0..2 {
        let engine = provider.open(&path);
        engine.sql("SET xmloption TO document", &[]).unwrap();
        let result = engine
            .sql("SELECT id, value, items FROM xml_durable ORDER BY id", &[])
            .unwrap();
        assert_eq!(result.rows.len(), 1, "{provider:?}");
        assert_eq!(
            result.column_types,
            vec![
                Some(ColumnType::Integer),
                Some(ColumnType::Xml),
                Some(ColumnType::Array(Box::new(ColumnType::Xml)))
            ]
        );
        assert_eq!(result.value_at(0, 1), Some(&Value::Str("text<a/>".into())));
        assert_eq!(
            format_postgres_text(
                result.value_at(0, 2).unwrap(),
                result.column_types[2].as_ref().unwrap(),
                Some(&engine)
            )
            .unwrap(),
            "{<a/>,text}"
        );
        engine.sql("BEGIN; INSERT INTO xml_durable SELECT id + 1, value, items FROM xml_durable; ROLLBACK", &[]).unwrap();
        engine.close().unwrap();
    }
}
