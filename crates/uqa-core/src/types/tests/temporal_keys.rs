//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered temporal keys retain the owner's comparator without caller-side normalization.

use super::TemporalValue;

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
