//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered temporal keys retain the owner's comparator without caller-side normalization.

use super::TemporalValue;

#[test]
fn predecessor_reservations_retain_unix_epochs_and_omit_unrepresentable_values() {
    use crate::TemporalInfinity;
    for (value, unix) in [
        (TemporalValue::parse_timestamp("1970-01-01").unwrap(), 0_i64),
        (
            TemporalValue::parse_timestamp("2000-01-01").unwrap(),
            crate::POSTGRES_EPOCH_MICROS,
        ),
        (TemporalInfinity::Negative.timestamp(), i64::MIN),
        (TemporalInfinity::Positive.timestamp(), i64::MIN + 1),
    ] {
        for wrapped in [false, true] {
            let mut bytes = Vec::new();
            let write = |part: &[u8]| {
                bytes.extend_from_slice(part);
                Ok::<_, ()>(())
            };
            let represented = if wrapped {
                value.write_legacy_reservation_key(write)
            } else {
                value.write_unix_reservation_key(write)
            }
            .unwrap();
            assert!(represented);
            let mut expected = vec![3];
            expected.extend_from_slice(&unix.to_be_bytes());
            assert_eq!(bytes, expected);
        }
    }
    for value in [
        TemporalValue::parse_timestamp("294276-12-31 23:59:59.999999").unwrap(),
        TemporalValue::parse_time_tz("00:00:00+00:00:01").unwrap(),
    ] {
        for wrapped in [false, true] {
            let mut emitted = false;
            let write = |_: &[u8]| {
                emitted = true;
                Ok::<_, ()>(())
            };
            let represented = if wrapped {
                value.write_legacy_reservation_key(write)
            } else {
                value.write_unix_reservation_key(write)
            }
            .unwrap();
            assert!(!represented && !emitted);
        }
    }
    let value = TemporalValue::parse_time_tz("13:00:00+01").unwrap();
    let mut bytes = Vec::new();
    assert!(value
        .write_unix_reservation_key(|part| {
            bytes.extend_from_slice(part);
            Ok::<_, ()>(())
        })
        .unwrap());
    let mut expected = vec![2];
    expected.extend_from_slice(&43_200_000_000_i128.to_be_bytes());
    expected.extend_from_slice(&(-60_i64).to_be_bytes());
    assert_eq!(bytes, expected);
}

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
            offset_seconds: 3600,
        },
        TemporalValue::TimeTz {
            micros: 43_200_000_000,
            offset_seconds: 0,
        },
        TemporalValue::TimeTz {
            micros: 46_800_000_000,
            offset_seconds: 3600,
        },
        TemporalValue::Timestamp {
            postgres_micros: i64::MIN,
        },
        TemporalValue::Timestamp { postgres_micros: 0 },
        TemporalValue::Timestamp {
            postgres_micros: i64::MAX,
        },
        TemporalValue::TimestampTz {
            postgres_micros: i64::MIN,
        },
        TemporalValue::TimestampTz {
            postgres_micros: i64::MAX,
        },
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
