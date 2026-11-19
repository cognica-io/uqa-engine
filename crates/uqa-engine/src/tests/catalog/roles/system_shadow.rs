//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Login catalog projection and ACL behavior against independently captured `PostgreSQL` 18.4 results.

use crate::Engine;
use uqa_core::Value;

#[rstest::rstest]
#[case::memory(None)]
#[case::sqlite_native(Some(0))]
#[case::sqlite_kv(Some(1))]
#[case::redb(Some(2))]
fn private_login_catalog_matches_postgresql_rows_and_column_authority(
    #[case] provider: Option<usize>,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shadow");
    let engine = provider.map_or_else(Engine::new, |provider| {
        super::identity::reopen(provider, &path)
    });
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("system_shadow.pg18.json")).unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let result = engine.sql(sql, &[]);
        if let Some(state) = case.get("sqlstate") {
            assert_eq!(result.unwrap_err().sqlstate(), state.as_str(), "{sql}");
        } else {
            let rows: Vec<serde_json::Value> = result
                .unwrap_or_else(|error| panic!("{sql}: {error:?}"))
                .rows
                .into_iter()
                .map(|row| {
                    let values = row
                        .into_iter()
                        .map(|(name, value)| {
                            let value = match value {
                                Value::Null => serde_json::Value::Null,
                                Value::Str(value) => value.into(),
                                Value::Bool(value) => value.into(),
                                Value::Int(value) => value.into(),
                                value => panic!("unexpected login catalog value {value:?}"),
                            };
                            (name, value)
                        })
                        .collect();
                    serde_json::Value::Object(values)
                })
                .collect();
            assert_eq!(serde_json::Value::Array(rows), case["rows"], "{sql}");
        }
    }
}
