//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::materialized_rows;
use uqa_core::Value;
use uqa_sql::SQLResult;

#[test]
fn materialized_rows_preserve_duplicate_query_values_by_position() {
    let result = SQLResult::from_rows_with_positions(
        vec!["value".into(), "value".into()],
        vec![[("value".into(), Value::Int(2))].into()],
        Some(vec![vec![Value::Int(1), Value::Int(2)]]),
    );
    let rows = materialized_rows(&result, &["left".into(), "right".into()]).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["left"], Value::Int(1));
    assert_eq!(rows[0]["right"], Value::Int(2));
}

#[test]
fn materialized_row_schema_drift_is_reported_even_without_rows() {
    let result = SQLResult::from_rows(vec!["only".into()], Vec::new());
    let error = materialized_rows(&result, &["first".into(), "second".into()]).unwrap_err();
    assert!(error.to_string().contains("schema width 2 changed to 1"));
    let malformed = SQLResult::from_rows(
        vec!["first".into(), "second".into()],
        vec![[("first".into(), Value::Int(1))].into()],
    );
    let error = materialized_rows(&malformed, &["first".into(), "second".into()]).unwrap_err();
    assert!(error.to_string().contains("row 0 is missing column 1"));
}
