//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn check(field: &str, value: &Value, numeric: &str, float: f64) {
    let control = ProductionControl::uncontrolled();
    let output = extract_from_value(field, value, true, &control).unwrap();
    let Value::Decimal(decimal) = &*output else {
        panic!("EXTRACT returns numeric: {output:?}");
    };
    assert_eq!(decimal.to_sql_string(), numeric, "{field}: {value:?}");
    let output = extract_from_value(field, value, false, &control).unwrap();
    let Value::Float(actual) = *output else {
        panic!("date_part returns float8: {output:?}");
    };
    assert_eq!(actual.to_bits(), float.to_bits(), "{field}: {value:?}");
}

fn timestamp(text: &str) -> Value {
    Value::Temporal(TemporalValue::parse_timestamp(text).unwrap())
}

fn interval(months: i32, days: i32, micros: i64) -> Value {
    Value::Temporal(TemporalValue::Interval {
        months,
        days,
        micros,
    })
}

#[test]
fn interval_epoch_uses_separate_year_month_day_and_time_fields() {
    // PostgreSQL 18 interval_part_common reference, including cancellation and the numeric overflow branch.
    for (value, numeric, float) in [
        (interval(12, 0, 0), "31557600.000000", 31_557_600.0),
        (interval(1, 0, 0), "2592000.000000", 2_592_000.0),
        (
            interval(14, 3, 14_706_000_007),
            "37015506.000007",
            37_015_506.000_007,
        ),
        (
            interval(-14, -3, -14_706_000_007),
            "-37015506.000007",
            -37_015_506.000_007,
        ),
        (
            interval(10, 3, -14_706_000_007),
            "26164493.999993",
            26_164_493.999_993,
        ),
        (interval(0, 0, -1), "-0.000001", -0.000_001),
        (
            interval(1_200_001, 0, 1),
            "3155762592000.000001",
            3_155_762_592_000.0,
        ),
        (interval(1_200_000, -36_525_000, 1), "0.000001", 0.0),
        (
            interval(i32::MAX, i32::MAX, 9_223_372_036_800_000_000),
            "5842218453753600.000000",
            5_842_218_453_753_600.0,
        ),
        (
            interval(i32::MIN, i32::MIN, -9_223_372_036_800_000_000),
            "-5842218456432000.000000",
            -5_842_218_456_432_000.0,
        ),
    ] {
        check("epoch", &value, numeric, float);
    }
}

#[test]
fn interval_fields_truncate_toward_zero_without_calendar_carry() {
    let value = interval(-24_026, -10, -92_096_123_456);
    for (unit, expected) in [
        ("millennium", "-2"),
        ("century", "-20"),
        ("decade", "-200"),
        ("year", "-2002"),
        ("quarter", "-1"),
        ("month", "-2"),
        ("week", "-1"),
        ("day", "-10"),
        ("hour", "-25"),
        ("minute", "-34"),
        ("second", "-56.123456"),
        ("milliseconds", "-56123.456"),
        ("microseconds", "-56123456"),
    ] {
        check(unit, &value, expected, expected.parse().unwrap());
    }
    check("quarter", &interval(i32::MIN, 0, 0), "1", 1.0);
    check("quarter", &interval(-12, 0, 0), "-1", -1.0);
    check("quarter", &interval(0, 0, 0), "1", 1.0);
}

#[test]
fn timestamp_numeric_epoch_retains_microseconds_beyond_float_precision() {
    for (input, numeric, float) in [
        (
            "99999-12-31 23:59:59.123456",
            "3093527980799.123456",
            3_093_527_980_799.123_5,
        ),
        (
            "3000-01-01 00:00:00.000001",
            "32503680000.000001",
            32_503_680_000.0,
        ),
        ("1969-12-31 23:59:59.999999", "-0.000001", -0.000_001),
    ] {
        check("epoch", &timestamp(input), numeric, float);
    }
    let value = timestamp("1969-12-31 23:59:59.999999");
    check("second", &value, "59.999999", 59.999_999);
    check("milliseconds", &value, "59999.999", 59_999.999);
    check("microseconds", &value, "59999999", 59_999_999.0);
}

#[test]
fn second_and_millisecond_float_results_follow_postgresql_operation_order() {
    check(
        "second",
        &Value::Temporal(TemporalValue::Time { micros: 1_003_691 }),
        "1.003691",
        1.003_690_999_999_999_9,
    );
    check(
        "milliseconds",
        &Value::Temporal(TemporalValue::Time { micros: 1_016_036 }),
        "1016.036",
        1_016.036_000_000_000_1,
    );
    check(
        "second",
        &interval(0, 0, -1_003_691),
        "-1.003691",
        -1.003_690_999_999_999_9,
    );
}

#[test]
fn bc_calendar_and_iso_fields_do_not_expose_year_or_century_zero() {
    for (input, values) in [
        ("0001-01-01 BC", ["-1", "-1", "0", "-1"]),
        ("0100-01-01 BC", ["-100", "-1", "-10", "-1"]),
        ("0101-01-01 BC", ["-101", "-2", "-10", "-1"]),
        ("1000-01-01 BC", ["-1000", "-10", "-100", "-1"]),
        ("1001-01-01 BC", ["-1001", "-11", "-100", "-2"]),
        ("0001-01-01", ["1", "1", "0", "1"]),
    ] {
        for value in [
            timestamp(input),
            Value::Temporal(TemporalValue::parse_date(input).unwrap()),
        ] {
            for (field, expected) in ["year", "century", "decade", "millennium"]
                .into_iter()
                .zip(values)
            {
                check(field, &value, expected, expected.parse().unwrap());
            }
        }
    }
    let value = timestamp("0001-01-01 BC");
    for (field, expected) in [
        ("isoyear", "-2"),
        ("week", "52"),
        ("dow", "6"),
        ("doy", "1"),
    ] {
        check(field, &value, expected, expected.parse().unwrap());
    }
}

#[test]
fn julian_numeric_uses_postgresql_division_scale() {
    for (input, expected, float) in [
        (
            "2001-01-01 12:34:56.123456",
            "2451911.52426068814814814815",
            2_451_911.524_260_688,
        ),
        (
            "2001-01-01",
            "2451911.0000000000000000000000000000",
            2_451_911.0,
        ),
        (
            "2001-01-01 00:00:00.000001",
            "2451911.0000000000115740740740740741",
            2_451_911.0,
        ),
    ] {
        check("julian", &timestamp(input), expected, float);
    }
    check(
        "julian",
        &Value::Temporal(TemporalValue::parse_date("4714-11-24 BC").unwrap()),
        "0",
        0.0,
    );
}

#[test]
fn time_and_timetz_keep_24_hour_components_and_unwrapped_epoch() {
    check(
        "hour",
        &Value::Temporal(TemporalValue::Time {
            micros: MICROS_PER_DAY,
        }),
        "24",
        24.0,
    );
    check(
        "epoch",
        &Value::Temporal(TemporalValue::Time {
            micros: MICROS_PER_DAY,
        }),
        "86400.000000",
        86_400.0,
    );
    for (value, epoch, float, hour, offset, tz_hour, tz_minute) in [
        (
            Value::Temporal(TemporalValue::TimeTz {
                micros: 45_296_123_456,
                offset_minutes: 330,
            }),
            "25496.123456",
            25_496.123_456,
            "12",
            "19800",
            "5",
            "30",
        ),
        (
            Value::Temporal(TemporalValue::TimeTz {
                micros: 1,
                offset_minutes: -210,
            }),
            "12600.000001",
            12_600.000_001,
            "0",
            "-12600",
            "-3",
            "-30",
        ),
        (
            Value::Temporal(TemporalValue::TimeTz {
                micros: 0,
                offset_minutes: 840,
            }),
            "-50400.000000",
            -50_400.0,
            "0",
            "50400",
            "14",
            "0",
        ),
    ] {
        check("epoch", &value, epoch, float);
        for (field, expected) in [
            ("hour", hour),
            ("timezone", offset),
            ("timezone_hour", tz_hour),
            ("timezone_minute", tz_minute),
        ] {
            check(field, &value, expected, expected.parse().unwrap());
        }
    }
}

#[test]
fn timestamp_zone_offset_changes_calendar_fields_but_not_epoch() {
    let value = Value::Temporal(
        TemporalValue::parse_timestamp_tz("2024-01-02 03:04:05.123456+00").unwrap(),
    );
    let control = ProductionControl::uncontrolled();
    for (offset, field, expected) in [
        (32_400, "hour", "12"),
        (20_700, "hour", "8"),
        (20_700, "minute", "49"),
        (-18_000, "day", "1"),
        (-18_000, "hour", "22"),
        (20_700, "timezone", "20700"),
        (20_700, "timezone_minute", "45"),
        (-12_600, "timezone_minute", "-30"),
    ] {
        let output = extract_from_value_with_offset(field, &value, true, offset, &control).unwrap();
        let Value::Decimal(actual) = &*output else {
            panic!("numeric result");
        };
        assert_eq!(actual.to_sql_string(), expected, "{field}: {offset}");
    }
    for as_numeric in [false, true] {
        let utc = extract_from_value("epoch", &value, as_numeric, &control).unwrap();
        let shifted =
            extract_from_value_with_offset("epoch", &value, as_numeric, 32_400, &control).unwrap();
        assert_eq!(*utc, *shifted);
    }
}

#[test]
fn unit_aliases_share_decode_units_and_extraction_special_tokens() {
    let value = timestamp("2001-01-01 12:34:56.123456");
    for (field, expected) in [
        ("Y", "2001"),
        ("yrs", "2001"),
        ("c", "21"),
        ("decs", "200"),
        ("mil", "3"),
        ("qtr", "1"),
        ("mons", "1"),
        ("W", "1"),
        ("D", "1"),
        ("hr", "12"),
        ("m", "34"),
        ("mm", "34"),
        ("secs", "56.123456"),
        ("msec", "56123.456"),
        ("usec", "56123456"),
        ("milliseconds_extra", "56123.456"),
        ("microseconds_extra", "56123456"),
    ] {
        check(field, &value, expected, expected.parse().unwrap());
    }
    for field in ["j", "jd"] {
        check(
            field,
            &value,
            "2451911.52426068814814814815",
            2_451_911.524_260_688,
        );
    }
}

#[test]
fn extraction_diagnostics_distinguish_type_support_and_reserved_tokens() {
    let control = ProductionControl::uncontrolled();
    let values = [
        (
            Value::Temporal(TemporalValue::Date { days: 0 }),
            "date",
            "0A000",
        ),
        (
            timestamp("2001-01-01"),
            "timestamp without time zone",
            "0A000",
        ),
        (
            Value::Temporal(TemporalValue::TimestampTz { micros: 0 }),
            "timestamp with time zone",
            "0A000",
        ),
        (
            Value::Temporal(TemporalValue::Time { micros: 0 }),
            "time without time zone",
            "22023",
        ),
        (
            Value::Temporal(TemporalValue::TimeTz {
                micros: 0,
                offset_minutes: 0,
            }),
            "time with time zone",
            "22023",
        ),
        (interval(0, 0, 0), "interval", "22023"),
    ];
    for (value, type_name, state) in values {
        let error = extract_from_value("now", &value, true, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state));
        let action = if state == "0A000" {
            "supported"
        } else {
            "recognized"
        };
        assert_eq!(
            error.to_string(),
            format!("unit \"now\" not {action} for type {type_name}")
        );
        assert_eq!(error.detail(), None);
        assert_eq!(error.hint(), None);
    }
    let value = Value::Temporal(TemporalValue::Date { days: 0 });
    let error = extract_from_value("hour", &value, true, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(
        error.to_string(),
        "unit \"hour\" not supported for type date"
    );
    assert_eq!(
        *extract_from_value("hour", &value, false, &control).unwrap(),
        Value::Float(0.0)
    );
    for (raw, normalized) in [
        ("YEAR ".to_string(), "year ".to_string()),
        ("A".repeat(70), "a".repeat(63)),
        ("가".repeat(23), "가".repeat(21)),
    ] {
        let error = extract_from_value(&raw, &timestamp("2001-01-01"), true, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22023"));
        assert_eq!(
            error.to_string(),
            format!("unit \"{normalized}\" not recognized for type timestamp without time zone")
        );
    }
}

#[test]
fn numeric_results_retain_only_their_admitted_memory_and_propagate_cancellation() {
    let budget = MemoryBudget::new(16_384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let value = timestamp("2001-01-01 12:34:56.123456");
    for field in ["year", "second", "epoch", "julian"] {
        let output = extract_from_value(field, &value, true, &control).unwrap();
        assert!(output.reserved_bytes() > 0);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    let zero = MemoryBudget::new(0);
    let control = ProductionControl::new(&zero, &token, &token);
    for field in ["year", "second", "epoch", "julian"] {
        let error = extract_from_value(field, &value, true, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(zero.used(), 0);
        assert!(matches!(
            *extract_from_value(field, &value, false, &control).unwrap(),
            Value::Float(_)
        ));
    }
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = extract_from_value("julian", &value, true, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn strict_nulls_precede_unit_conversion_and_diagnostics() {
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for name in ["extract", "date_part"] {
        for arguments in [
            [Value::Null, timestamp("2001-01-01")],
            [Value::Str("not a temporal unit".into()), Value::Null],
            [Value::Null, Value::Null],
        ] {
            let output = crate::expr::scalar_temporal::eval_temporal_functions_with_control(
                name, &arguments, &control,
            )
            .unwrap()
            .unwrap();
            assert_eq!(*output, Value::Null);
            assert_eq!(budget.used(), 0);
        }
    }
}
