//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::key_value::catalog::{column_stats_key, StoredColumnStats};

fn ordinary(key: &[u8], value: &[u8]) -> ColumnStatsRow {
    let mut offset = 1;
    let _table = read_str_ref(key, &mut offset).unwrap();
    let column_name = read_str_ref(key, &mut offset).unwrap().to_owned();
    let stored: StoredColumnStats = serde_json::from_slice(value).unwrap();
    ColumnStatsRow {
        column_name,
        distinct_count: stored.distinct_count,
        null_count: stored.null_count,
        min_value: stored.min_value,
        max_value: stored.max_value,
        row_count: stored.row_count,
        histogram_json: stored.histogram_json,
        mcv_values_json: stored.mcv_values_json,
        mcv_frequencies_json: stored.mcv_frequencies_json,
    }
}

fn bytes(row: &ColumnStatsRow) -> usize {
    row.column_name.capacity()
        + row.histogram_json.capacity()
        + row.mcv_values_json.capacity()
        + row.mcv_frequencies_json.capacity()
        + row.min_value.as_ref().map_or(0, String::capacity)
        + row.max_value.as_ref().map_or(0, String::capacity)
}

#[test]
fn column_statistics_input_preserves_native_struct_forms_and_opaque_distributions() {
    let payloads = [
        r#"{"distinct_count":-1,"null_count":2,"row_count":9223372036854775807,"histogram_json":" [1.00, -0.0] ","mcv_values_json":"invalid JSON retained for Planner","mcv_frequencies_json":"[0.25]"}"#,
        r#"[-9223372036854775808,0,null," null ",3,"[]","[]","[]"]"#,
        r#"{"distinct_\u0063ount":1,"null_count":2,"min_value":"\u0000한글","max_value":"\\u0000","row_count":3,"histogram_json":"[]","mcv_values_json":"[]","mcv_frequencies_json":"[]","unknown":[1e9999,"\ud800"]}"#,
    ];
    let deep = format!(
        r#"{{"distinct_count":1,"null_count":0,"row_count":3,"histogram_json":"[]","mcv_values_json":"[]","mcv_frequencies_json":"[]","ignored":{}0{}}}"#,
        "[".repeat(256),
        "]".repeat(256)
    );
    let key = column_stats_key("\"Schema.Name\".\"Table.Name\"", "Field\"한글").unwrap();
    let mut trailing = key.clone();
    trailing.extend_from_slice(b"legacy suffix");
    for key in [&key, &trailing] {
        for payload in payloads.into_iter().chain([deep.as_str()]) {
            let expected = ordinary(key, payload.as_bytes());
            let control = StorageReadControl::with_limit(65536);
            let actual = decode(key, payload.as_bytes(), &control).unwrap().unwrap();
            assert_eq!(*actual, expected);
            assert_eq!(control.memory().used(), bytes(&actual));
            drop(actual);
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn column_statistics_input_rejects_invalid_shapes_and_preserves_error_precedence() {
    let key = column_stats_key("t", "c").unwrap();
    for (payload, fragment) in [
        (
            r#"{"distinct_count":1,"distinct_count":!}"#,
            "duplicate field `distinct_count`",
        ),
        ("{} 1", "missing field `distinct_count`"),
    ] {
        assert!(serde_json::from_str::<StoredColumnStats>(payload)
            .unwrap_err()
            .to_string()
            .contains(fragment));
        let control = StorageReadControl::with_limit(65536);
        assert!(decode(&key, payload.as_bytes(), &control)
            .unwrap_err()
            .to_string()
            .contains(fragment));
        assert_eq!(control.memory().used(), 0);
    }
    for payload in [
        "null",
        "[]",
        "{}",
        "[1,2,null,null,3,\"[]\",\"[]\"]",
        "[1,2,null,null,3,\"[]\",\"[]\",\"[]\",null]",
        "[1.0,2,null,null,3,\"[]\",\"[]\",\"[]\"]",
        "[9223372036854775808,2,null,null,3,\"[]\",\"[]\",\"[]\"]",
        "[1,2,42,null,3,\"[]\",\"[]\",\"[]\"]",
        "[1,2,null,null,3,[],\"[]\",\"[]\"]",
        "[1,2,null,null,3,\"[]\",\"[]\",\"[]\"] 1",
        r#"[1,2,"\ud800",null,3,"[]","[]","[]"]"#,
    ] {
        assert!(
            serde_json::from_str::<StoredColumnStats>(payload).is_err(),
            "{payload}"
        );
        let control = StorageReadControl::with_limit(65536);
        assert!(
            decode(&key, payload.as_bytes(), &control).is_err(),
            "{payload}"
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn column_statistics_input_admits_every_field_and_retires_denied_prefixes() {
    let key = column_stats_key("t", "column").unwrap();
    let payload = br#"[1,2,"minimum","maximum",3,"[1,2,3]","[4,5]","[0.25,0.75]"]"#;
    let mut failures = 0;
    let mut successes = 0;
    for limit in 0..512 {
        let control = StorageReadControl::with_limit(limit);
        match decode(&key, payload, &control) {
            Ok(Some(row)) => {
                successes += 1;
                assert_eq!(control.memory().used(), bytes(&row));
                drop(row);
            }
            Err(StorageBackendError::Memory(_)) => failures += 1,
            other => panic!("unexpected field admission result: {other:?}"),
        }
        assert_eq!(control.memory().used(), 0, "limit {limit}");
    }
    assert!(failures > 0 && successes > 0);
    let control = StorageReadControl::with_limit(65536);
    control.cancellation().cancel();
    assert!(decode(&key, payload, &control).is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn column_statistics_input_preserves_key_errors_and_empty_completion() {
    let key = column_stats_key("table", "column").unwrap();
    for end in 0..key.len() {
        let control = StorageReadControl::with_limit(65536);
        assert!(decode(&key[..end], b"!", &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(0);
    let limits = CatalogInputLimits {
        maximum_key_bytes: 1,
        maximum_value_bytes: 1,
    };
    let mut input = CatalogInput::<ColumnStatsRow>::empty(limits, &control).unwrap();
    control.cancellation().cancel();
    for _ in 0..2 {
        assert!(matches!(
            input.advance().unwrap(),
            crate::catalog::input::CatalogInputStep::Complete
        ));
    }
    assert!(CatalogInput::<ColumnStatsRow>::empty(limits, &control).is_err());
    let control = StorageReadControl::with_limit(0);
    for limits in [
        CatalogInputLimits {
            maximum_key_bytes: 0,
            maximum_value_bytes: 1,
        },
        CatalogInputLimits {
            maximum_key_bytes: 1,
            maximum_value_bytes: 0,
        },
    ] {
        assert!(CatalogInput::<ColumnStatsRow>::empty(limits, &control).is_err());
    }
    assert_eq!(control.memory().used(), 0);
}
