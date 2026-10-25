//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::types::temporal::epoch_date;
use crate::{memory::MemoryBudget, CancellationToken};
use chrono::NaiveDate;

mod interval_limits;

#[test]
fn second_precision_zones_retain_postgresql_timetz_binary_fields_and_output() {
    // PostgreSQL 18.4 timetz_send returns local microseconds followed by seconds west of UTC.
    for (text, binary) in [
        ("12:34:56.123456+15:59:59", "0000000a8bdbfe40ffff1f01"),
        ("24:00:00-15:59:59", "000000141dd760000000e0ff"),
        ("00:00:00+00:00:01", "0000000000000000ffffffff"),
    ] {
        let expected = TemporalValue::TimeTz {
            micros: u64::from_str_radix(&binary[..16], 16).unwrap() as i64,
            offset_seconds: -(u32::from_str_radix(&binary[16..], 16).unwrap() as i32),
        };
        for limit in [0, 1024] {
            let memory = MemoryBudget::new(limit);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let actual = TemporalValue::parse_time_tz_with_control(text, &control)
                .unwrap()
                .unwrap();
            assert_eq!(actual, expected);
            let output = actual.to_sql_string_with_control(&control);
            if limit == 0 {
                assert!(output.is_err());
            } else {
                assert_eq!(output.unwrap().as_str(), text);
            }
            assert_eq!(memory.used(), 0);
        }
    }
    for text in ["00:00:00+16", "00:00:00-16", "00:00:00+00:00:60"] {
        assert_eq!(
            TemporalValue::time_tz_input(text, 0),
            Err(crate::TemporalInputError::ZoneDisplacement)
        );
    }
}

#[test]
fn integer_calendar_timestamp_input_and_output_match_postgresql() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("tests/timestamps.json")).unwrap();
    let budget = MemoryBudget::new(1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for case in reference["cases"].as_array().unwrap() {
        let text = case["input"].as_str().unwrap();
        let micros =
            u64::from_str_radix(case["postgresql_binary"].as_str().unwrap(), 16).unwrap() as i64;
        let expected = match case["type"].as_str().unwrap() {
            "timestamp" => TemporalValue::Timestamp {
                postgres_micros: micros,
            },
            "timestamptz" => TemporalValue::TimestampTz {
                postgres_micros: micros,
            },
            other => panic!("unexpected PostgreSQL type {other}"),
        };
        for input in [
            expected.parse_same_kind(text),
            expected
                .parse_same_kind_with_control(text, &control)
                .unwrap(),
        ] {
            assert_eq!(input.as_ref(), Some(&expected), "{text}");
            assert_eq!(budget.used(), 0);
        }
        let output = expected.to_sql_string_with_control(&control).unwrap();
        assert_eq!(output.as_str(), case["output"].as_str().unwrap(), "{text}");
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    for text in ["400000-01-01", "294000-02-30"] {
        assert!(TemporalValue::parse_timestamp(text).is_none());
        assert!(TemporalValue::parse_timestamp_tz(text).is_none());
    }
}

#[test]
fn wide_timestamp_production_preserves_quotas_and_both_cancellation_scopes() {
    let value = TemporalValue::Timestamp {
        // PostgreSQL 18.4 timestamp_send('294000-01-01').
        postgres_micros: 9_214_629_984_000_000_000,
    };
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        value
            .parse_same_kind_with_control("294000-01-01", &control)
            .unwrap(),
        Some(value.clone())
    );
    assert!(matches!(
        value.to_sql_string_with_control(&control),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(budget.used(), 0);
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert!(matches!(
            value.parse_same_kind_with_control("294000-01-01", &control),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert!(matches!(
            value.to_sql_string_with_control(&control),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(budget.used(), 0);
    }
}

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
    // Reading a time allocates no scratch, so it succeeds under an empty allowance.
    assert_eq!(
        TemporalValue::parse_time_with_control("24:00:00", &control).unwrap(),
        Some(TemporalValue::Time {
            micros: 86_400_000_000
        })
    );
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
fn same_kind_input_uses_the_supplied_clock_and_retains_cancellation() {
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let kinds = [
        TemporalValue::Date { days: 1 },
        TemporalValue::Time {
            micros: 3_723_456_789,
        },
        TemporalValue::TimeTz {
            micros: 3_723_456_789,
            offset_seconds: 0,
        },
        TemporalValue::Timestamp {
            postgres_micros: (90_123_456_789) - crate::POSTGRES_EPOCH_MICROS,
        },
        TemporalValue::TimestampTz {
            postgres_micros: (90_123_456_789) - crate::POSTGRES_EPOCH_MICROS,
        },
    ];
    for expected in &kinds {
        assert_eq!(
            expected
                .parse_same_kind_at_with_control("now", 90_123_456_789, &control)
                .unwrap(),
            Some(expected.clone())
        );
        assert_eq!(budget.used(), 0);
    }
    original.cancel();
    for kind in &kinds {
        assert!(matches!(
            kind.parse_same_kind_at_with_control("now", 90_123_456_789, &control),
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
                offset_seconds: 0,
            },
            "12:34:56.123+09",
        ),
        (
            TemporalValue::Timestamp {
                postgres_micros: (0) - crate::POSTGRES_EPOCH_MICROS,
            },
            "2024-02-29 12:34:56",
        ),
        (
            TemporalValue::TimestampTz {
                postgres_micros: (0) - crate::POSTGRES_EPOCH_MICROS,
            },
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

#[test]
fn years_print_without_a_sign_and_with_the_bc_era() {
    let days = |year, month, day| {
        i32::try_from(
            NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .signed_duration_since(epoch_date())
                .num_days(),
        )
        .unwrap()
    };
    let micros = |year, month, day, seconds: i64| {
        i64::from(days(year, month, day)) * MICROS_PER_DAY + seconds * MICROS_PER_SECOND
    };
    assert_eq!(
        TemporalValue::Date {
            days: days(99_999, 1, 1)
        }
        .to_sql_string(),
        "99999-01-01"
    );
    assert_eq!(
        TemporalValue::Date {
            days: days(0, 12, 31)
        }
        .to_sql_string(),
        "0001-12-31 BC"
    );
    assert_eq!(
        TemporalValue::Date {
            days: days(-1, 6, 15)
        }
        .to_sql_string(),
        "0002-06-15 BC"
    );
    assert_eq!(
        TemporalValue::Timestamp {
            postgres_micros: (micros(99_999, 1, 1, 0)) - crate::POSTGRES_EPOCH_MICROS
        }
        .to_sql_string(),
        "99999-01-01 00:00:00"
    );
    assert_eq!(
        TemporalValue::Timestamp {
            postgres_micros: (micros(0, 12, 31, 86_399)) - crate::POSTGRES_EPOCH_MICROS
        }
        .to_sql_string(),
        "0001-12-31 23:59:59 BC"
    );
    assert_eq!(
        TemporalValue::TimestampTz {
            postgres_micros: (micros(-1, 6, 15, 36_000)) - crate::POSTGRES_EPOCH_MICROS
        }
        .to_sql_string(),
        "0002-06-15 10:00:00+00 BC"
    );
}

#[test]
fn controlled_date_order_preserves_cancellation_and_releases_scratch() {
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let value = TemporalValue::date_input_in_order_with_control(
        "01/02/03",
        0,
        TemporalDateOrder::DayMonthYear,
        &control,
    )
    .unwrap()
    .unwrap();
    assert_eq!(value.to_sql_string(), "2003-02-01");
    assert_eq!(budget.used(), 0);
    invoking.cancel();
    assert!(matches!(
        TemporalValue::date_input_in_order_with_control(
            "01/02/03",
            0,
            TemporalDateOrder::YearMonthDay,
            &control
        ),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(budget.used(), 0);
}
