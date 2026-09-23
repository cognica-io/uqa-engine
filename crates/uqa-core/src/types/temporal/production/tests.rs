//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};

#[test]
fn controlled_temporal_production_preserves_fractional_and_interval_spelling() {
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (source, expected) in [
        (
            "1 year 2 mons 3 days 04:05:06.1200",
            "1 year 2 mons 3 days 04:05:06.12",
        ),
        ("-1 days 3 hours", "-1 days +03:00:00"),
        ("1.5 MONS AGO", "-1 mons -15 days"),
    ] {
        let value = TemporalValue::parse_interval_with_control(source, &control)
            .unwrap()
            .unwrap();
        assert_eq!(budget.used(), 0);
        let text = value.to_sql_string_with_control(&control).unwrap();
        assert_eq!(&**text, expected);
        assert_eq!(budget.used(), text.capacity());
        drop(text);
        assert_eq!(budget.used(), 0);
    }
    let time = TemporalValue::parse_time_with_control("24:00:00.00000", &control)
        .unwrap()
        .unwrap();
    assert_eq!(time.to_sql_string(), "24:00:00");
    let time = TemporalValue::parse_time_with_control("01:02:03.000120", &control)
        .unwrap()
        .unwrap();
    assert_eq!(time.to_sql_string(), "01:02:03.00012");
}

#[test]
fn temporal_quota_and_both_cancellation_scopes_leave_no_scratch() {
    let budget = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    assert!(matches!(
        TemporalValue::parse_interval_with_control("1 year", &control),
        Err(ValueRetentionError::Memory(_))
    ));
    assert!(matches!(
        TemporalValue::parse_time_with_control("24:00:00", &control),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(budget.used(), 0);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            TemporalValue::parse_timestamp_tz_with_control("2026-01-01 00:00:00+00", &control),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn same_kind_parser_preserves_each_temporal_variant_and_cancellation() {
    let fixtures = [
        (TemporalValue::Date { days: 0 }, "2024-02-29"),
        (TemporalValue::Time { micros: 0 }, "24:00:00"),
        (
            TemporalValue::TimeTz {
                micros: 0,
                offset_minutes: 0,
            },
            "12:34:56.123+09",
        ),
        (
            TemporalValue::Timestamp { micros: 0 },
            "2024-02-29 12:34:56",
        ),
        (
            TemporalValue::TimestampTz { micros: 0 },
            "2024-02-29 12:34:56+09",
        ),
        (
            TemporalValue::Interval {
                months: 0,
                days: 0,
                micros: 0,
            },
            "1.5 months 3 days ago",
        ),
    ];
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for (kind, text) in &fixtures {
        let output = kind.parse_same_kind_with_control(text, &control).unwrap();
        assert!(output.is_some(), "{text}");
        assert_eq!(output, kind.parse_same_kind(text));
        assert!(kind
            .parse_same_kind_with_control("invalid", &control)
            .unwrap()
            .is_none());
        assert_eq!(budget.used(), 0);
    }
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        for (kind, text) in &fixtures {
            assert!(matches!(
                kind.parse_same_kind_with_control(text, &control),
                Err(ValueRetentionError::Cancelled(_))
            ));
            assert_eq!(budget.used(), 0);
        }
    }
}
