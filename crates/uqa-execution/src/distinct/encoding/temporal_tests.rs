//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` expectations cover hash, spill and ordered-index key domains.

use super::*;
use crate::serializable::index_key::ScalarIndexDomain;
use uqa_sql::ColumnType;

#[test]
fn temporal_keys_and_hashes_match_postgresql_equality_and_index_order() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-core/src/types/tests/pg18_temporal.json"
    )))
    .unwrap();
    let control = StorageReadControl::with_limit(64 * 1024);
    let hasher = std::collections::hash_map::RandomState::new();
    for group in fixture["types"].as_array().unwrap() {
        let time = group["kind"] == "time";
        let domain = ScalarIndexDomain::from_column_type(if time {
            &ColumnType::Time
        } else {
            &ColumnType::TimeTz
        })
        .unwrap();
        let values: Vec<_> = group["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|text| {
                Value::Temporal(
                    if time {
                        TemporalValue::parse_time(text.as_str().unwrap())
                    } else {
                        TemporalValue::parse_time_tz(text.as_str().unwrap())
                    }
                    .unwrap(),
                )
            })
            .collect();
        for pair in group["comparisons"].as_array().unwrap() {
            let left = &values[pair[0].as_u64().unwrap() as usize];
            let right = &values[pair[1].as_u64().unwrap() as usize];
            let equal = pair[2] == true;
            let left_key = canonical_row_key(std::slice::from_ref(left)).unwrap();
            let right_key = canonical_row_key(std::slice::from_ref(right)).unwrap();
            assert_eq!(left_key == right_key, equal, "{left:?}, {right:?}");
            let controlled =
                canonical_row_key_budgeted(std::iter::once(Some(left)), &control).unwrap();
            assert_eq!(&*controlled, left_key);
            drop(controlled);
            if equal {
                assert_eq!(
                    hash_canonical_row(&hasher, std::iter::once(Some(left))).unwrap(),
                    hash_canonical_row(&hasher, std::iter::once(Some(right))).unwrap()
                );
            }
            let expected = if equal {
                std::cmp::Ordering::Equal
            } else if pair[3] == true {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
            assert_eq!(
                domain
                    .encode(left, &control)
                    .unwrap()
                    .as_ref()
                    .cmp(domain.encode(right, &control).unwrap().as_ref()),
                expected
            );
            let row = |value: &Value| [Value::Row(vec![value.clone(), Value::Null])];
            assert_eq!(
                canonical_row_key(&row(left)).unwrap() == canonical_row_key(&row(right)).unwrap(),
                equal
            );
            assert_eq!(control.memory().used(), 0);
        }
    }
}
