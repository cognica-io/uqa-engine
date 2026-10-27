//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn calendar_constructor_fields_and_diagnostics_match_postgresql() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!("tests.json")).unwrap();
    for case in oracle["cases"].as_array().unwrap().iter().skip(1) {
        let integer = |field: &str, index: usize| {
            i32::try_from(case[field][index].as_i64().unwrap()).unwrap()
        };
        let year = integer("date_fields", 0);
        let month = integer("date_fields", 1);
        let day = integer("date_fields", 2);
        let actual = if case["constructor"] == "make_date" {
            make_date(year, month, day)
        } else {
            make_timestamp(
                year,
                month,
                day,
                integer("time_fields", 0),
                integer("time_fields", 1),
                case["second"].as_str().unwrap().parse().unwrap(),
            )
        };
        if case["error"].is_null() {
            let Value::Temporal(value) = actual.unwrap() else {
                panic!("temporal constructor");
            };
            assert_eq!(
                value.to_sql_string(),
                case["results"][0]["rows"][0][0].as_str().unwrap(),
                "{}",
                case["sql"]
            );
        } else {
            let error = actual.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                case["error"]["sqlstate"].as_str(),
                "{}",
                case["sql"]
            );
            assert_eq!(
                error.to_string(),
                case["error"]["message"].as_str().unwrap(),
                "{}",
                case["sql"]
            );
        }
    }
}
