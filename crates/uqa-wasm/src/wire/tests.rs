//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::RecordValue;

#[test]
fn records_check_each_declared_value_and_write_the_last_value_for_each_name() {
    let record = |fields| Value::Record(RecordValue::new(fields));
    let value = record(vec![
        ("z".into(), Value::Int(1)),
        ("a".into(), Value::Bytes(vec![0, 255])),
        ("z".into(), Value::Json(r#"{"v":[true,null]}"#.into())),
    ]);
    assert_eq!(
        success(WireValue(&value)).unwrap(),
        r#"{"ok":{"a":{"$bytes":"AP8="},"z":{"v":[true,null]}}}"#
    );
    let invalid = record(vec![
        ("z".into(), Value::Float(f64::INFINITY)),
        ("z".into(), Value::Null),
    ]);
    assert_eq!(
        success(WireValue(&invalid)).unwrap_err(),
        "SQL float is not a finite JSON number: inf"
    );
}

#[test]
fn row_conversion_preserves_safe_integers_and_error_precedence() {
    let count = crate::JS_MAX_SAFE_INTEGER + 1;
    let mut row = BTreeMap::new();
    row.insert("n".into(), Value::Int(i64::MAX));
    let rows = [row];
    let result = WireResult {
        affected_rows: count,
        columns: &[],
        rows: &rows,
    };
    assert_eq!(
        success(result).unwrap_err(),
        format!(
            "SQL integer exceeds JavaScript's safe integer range: {}",
            i64::MAX
        )
    );
    let result = WireResult {
        affected_rows: count,
        columns: &[],
        rows: &[],
    };
    assert_eq!(
        success(result).unwrap_err(),
        format!(
            "affected row count exceeds JavaScript's maximum safe integer ({}): {count}",
            crate::JS_MAX_SAFE_INTEGER
        )
    );
}

#[cfg(not(target_family = "wasm"))]
#[test]
fn thousands_of_projected_rows_allocate_only_the_growing_output_buffer() {
    let columns = ["n".to_owned()];
    let rows = (0..4096)
        .map(|n| BTreeMap::from([("n".to_owned(), Value::Int(n))]))
        .collect::<Vec<_>>();
    let mut encoded = String::new();
    let allocated = allocation_counter::measure(|| {
        encoded = success(WireResult {
            columns: &columns,
            rows: &rows,
            affected_rows: 0,
        })
        .unwrap();
    });
    let body: JSON = serde_json::from_str(&encoded).unwrap();
    let rows = body["ok"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4096);
    assert_eq!(rows[4095]["n"], 4095);
    assert!(
        allocated.count_total < 128,
        "{} allocations for 4096 rows",
        allocated.count_total
    );
}

#[test]
fn search_hits_validate_identity_before_score_and_keep_field_names() {
    use retrieval::SearchHits;
    use uqa_core::ScoredEntry;
    let entry = ScoredEntry {
        doc_id: 7,
        score: 1.25,
    };
    assert_eq!(
        success(SearchHits(&[entry])).unwrap(),
        r#"{"ok":[{"docId":7,"score":1.25}]}"#
    );
    let entry = ScoredEntry {
        doc_id: 3,
        score: f64::NAN,
    };
    assert_eq!(
        success(SearchHits(&[entry])).unwrap_err(),
        "search score is not a finite JSON number: NaN"
    );
    let entry = ScoredEntry {
        doc_id: u64::MAX,
        score: f64::NAN,
    };
    assert_eq!(
        success(SearchHits(&[entry])).unwrap_err(),
        format!(
            "search document id exceeds JavaScript's maximum safe integer ({}): {}",
            crate::JS_MAX_SAFE_INTEGER,
            u64::MAX
        )
    );
}
