//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered temporal keys retain the owner's comparator without caller-side normalization.

use super::TemporalValue;

fn equality_key(value: &TemporalValue) -> Vec<u8> {
    let mut bytes = Vec::new();
    value
        .write_equality_key(|part| {
            bytes.extend_from_slice(part);
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap();
    bytes
}

#[test]
fn temporal_order_and_keys_match_postgresql_in_both_directions() {
    use std::cmp::Ordering;
    use std::collections::BTreeSet;
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("pg18_temporal.json")).unwrap();
    for group in fixture["types"].as_array().unwrap() {
        let values: Vec<_> = group["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|text| {
                if group["kind"] == "time" {
                    TemporalValue::parse_time(text.as_str().unwrap())
                } else {
                    TemporalValue::parse_time_tz(text.as_str().unwrap())
                }
                .unwrap()
            })
            .collect();
        for pair in group["comparisons"].as_array().unwrap() {
            let left = &values[pair[0].as_u64().unwrap() as usize];
            let right = &values[pair[1].as_u64().unwrap() as usize];
            let expected = if pair[2] == true {
                Ordering::Equal
            } else if pair[3] == true {
                Ordering::Less
            } else {
                Ordering::Greater
            };
            assert_eq!(pair[4], expected == Ordering::Greater);
            assert_eq!(left.cmp(right), expected, "{left:?}, {right:?}");
            assert_eq!(key(left).cmp(&key(right)), expected, "{left:?}, {right:?}");
            assert_eq!(equality_key(left) == equality_key(right), pair[2] == true);
            assert_eq!(left == right, pair[2] == true);
            let control = crate::memory::ProductionControl::uncontrolled();
            assert_eq!(
                crate::Value::Temporal(left.clone())
                    .cmp_with_control(&crate::Value::Temporal(right.clone()), &control)
                    .unwrap(),
                expected
            );
        }
        for left in &values {
            for middle in &values {
                for right in &values {
                    if left <= middle && middle <= right {
                        assert!(left <= right, "{left:?} <= {middle:?} <= {right:?}");
                    }
                }
            }
        }
        let ascending: BTreeSet<_> = values.iter().cloned().collect();
        let descending: BTreeSet<_> = values.iter().rev().cloned().collect();
        assert_eq!(ascending, descending);
        for value in &values {
            assert!(ascending.contains(value));
        }
        let representatives = group["comparisons"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|pair| {
                pair[2] == true
                    && pair[0] == pair[1]
                    && !group["comparisons"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|earlier| {
                            earlier[0] == pair[0]
                                && earlier[1].as_u64() < pair[1].as_u64()
                                && earlier[2] == true
                        })
            })
            .count();
        assert_eq!(ascending.len(), representatives);
    }
}

fn key(value: &TemporalValue) -> Vec<u8> {
    let mut key = Vec::new();
    value
        .write_comparison_key(|part| {
            key.extend_from_slice(part);
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap();
    key
}

#[test]
fn temporal_comparison_keys_follow_native_order_and_sink_failure() {
    let values = [
        TemporalValue::Date { days: i32::MIN },
        TemporalValue::Date { days: 0 },
        TemporalValue::Date { days: i32::MAX },
        TemporalValue::Time { micros: 0 },
        TemporalValue::Time {
            micros: 86_400_000_000,
        },
        TemporalValue::TimeTz {
            micros: 0,
            offset_minutes: 60,
        },
        TemporalValue::TimeTz {
            micros: 43_200_000_000,
            offset_minutes: 0,
        },
        TemporalValue::TimeTz {
            micros: 46_800_000_000,
            offset_minutes: 60,
        },
        TemporalValue::Timestamp { micros: i64::MIN },
        TemporalValue::Timestamp { micros: 0 },
        TemporalValue::Timestamp { micros: i64::MAX },
        TemporalValue::TimestampTz { micros: i64::MIN },
        TemporalValue::TimestampTz { micros: i64::MAX },
        TemporalValue::Interval {
            months: i32::MIN,
            days: i32::MIN,
            micros: i64::MIN,
        },
        TemporalValue::Interval {
            months: 0,
            days: 0,
            micros: 0,
        },
        TemporalValue::Interval {
            months: 1,
            days: 0,
            micros: 0,
        },
        TemporalValue::Interval {
            months: 0,
            days: 30,
            micros: 0,
        },
        TemporalValue::Interval {
            months: i32::MAX,
            days: i32::MAX,
            micros: i64::MAX,
        },
    ];
    for left in &values {
        for right in &values {
            assert_eq!(
                key(left).cmp(&key(right)),
                left.cmp(right),
                "{left:?}, {right:?}"
            );
        }
    }
    let mut calls = 0;
    let error = values[0].write_comparison_key(|_| {
        calls += 1;
        Err::<(), _>("closed sink")
    });
    assert_eq!(error, Err("closed sink"));
    assert_eq!(calls, 1);
}
