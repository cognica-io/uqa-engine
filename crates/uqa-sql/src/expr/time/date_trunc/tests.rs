//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn interval(months: i32, days: i32, micros: i64) -> Value {
    Value::Temporal(TemporalValue::Interval {
        months,
        days,
        micros,
    })
}

fn interval_fields(value: Value) -> (i32, i32, i64) {
    let Value::Temporal(TemporalValue::Interval {
        months,
        days,
        micros,
    }) = value
    else {
        panic!("truncation must preserve the interval carrier");
    };
    (months, days, micros)
}

fn evaluate(unit: &str, value: &Value) -> Value {
    truncate(unit, value, &ProductionControl::uncontrolled()).unwrap()
}

#[test]
fn intervals_preserve_fields_and_truncate_both_signs_toward_zero() {
    // Independently captured interval_positive/negative observations in the PostgreSQL 18 oracle.
    let cases = [
        ("millennium", (24_000, 0, 0)),
        ("century", (24_000, 0, 0)),
        ("decade", (24_120, 0, 0)),
        ("year", (24_168, 0, 0)),
        ("quarter", (24_171, 0, 0)),
        ("month", (24_173, 0, 0)),
        ("day", (24_173, 35, 0)),
        ("hour", (24_173, 35, 176_400_000_000)),
        ("minute", (24_173, 35, 177_780_000_000)),
        ("second", (24_173, 35, 177_825_000_000)),
        ("milliseconds", (24_173, 35, 177_825_678_000)),
        ("microseconds", (24_173, 35, 177_825_678_901)),
    ];
    for sign in [-1, 1] {
        let input = interval(24_173 * sign, 35 * sign, 177_825_678_901 * i64::from(sign));
        for (unit, (months, days, micros)) in cases {
            assert_eq!(
                interval_fields(evaluate(unit, &input)),
                (months * sign, days * sign, micros * i64::from(sign)),
                "{unit}: {input:?}"
            );
        }
    }
    for (unit, input, expected) in [
        ("quarter", interval(-19, -3, -177_825_678_901), (-18, 0, 0)),
        ("quarter", interval(-1, 7, 1), (0, 0, 0)),
        ("milliseconds", interval(0, 0, -1), (0, 0, 0)),
        ("second", interval(0, 0, -999_999), (0, 0, 0)),
        (
            "hour",
            interval(1, -3, -179_999_999_999),
            (1, -3, -176_400_000_000),
        ),
    ] {
        assert_eq!(interval_fields(evaluate(unit, &input)), expected, "{unit}");
    }
}

#[test]
fn interval_truncation_does_not_overflow_extreme_finite_fields() {
    for (unit, input, expected) in [
        ("microseconds", interval(i32::MAX, 0, 0), (i32::MAX, 0, 0)),
        ("year", interval(i32::MIN, 0, 0), (-2_147_483_640, 0, 0)),
        ("day", interval(0, i32::MAX, 0), (0, i32::MAX, 0)),
        ("day", interval(0, i32::MIN, 0), (0, i32::MIN, 0)),
    ] {
        assert_eq!(interval_fields(evaluate(unit, &input)), expected, "{unit}");
    }
}

fn timestamp(input: &str) -> Value {
    Value::Temporal(TemporalValue::parse_timestamp(input).unwrap())
}

#[test]
fn timestamp_calendar_truncation_matches_postgresql() {
    let input = timestamp("2024-05-19 15:23:45.678901");
    for (unit, expected) in [
        ("millennium", "2001-01-01 00:00:00"),
        ("century", "2001-01-01 00:00:00"),
        ("decade", "2020-01-01 00:00:00"),
        ("year", "2024-01-01 00:00:00"),
        ("quarter", "2024-04-01 00:00:00"),
        ("month", "2024-05-01 00:00:00"),
        ("week", "2024-05-13 00:00:00"),
        ("day", "2024-05-19 00:00:00"),
        ("hour", "2024-05-19 15:00:00"),
        ("minute", "2024-05-19 15:23:00"),
        ("second", "2024-05-19 15:23:45"),
        ("milliseconds", "2024-05-19 15:23:45.678"),
        ("microseconds", "2024-05-19 15:23:45.678901"),
    ] {
        assert_eq!(evaluate(unit, &input), timestamp(expected), "{unit}");
    }
}

#[test]
fn timestamp_bc_year_zero_and_pre_epoch_fractions_follow_calendar_boundaries() {
    for (unit, input, expected) in [
        (
            "millennium",
            "0001-05-19 15:23:45.678901 BC",
            "1000-01-01 00:00:00 BC",
        ),
        (
            "century",
            "0001-05-19 15:23:45.678901 BC",
            "0100-01-01 00:00:00 BC",
        ),
        (
            "decade",
            "0001-05-19 15:23:45.678901 BC",
            "0001-01-01 00:00:00 BC",
        ),
        (
            "decade",
            "0011-01-02 03:04:05.678901 BC",
            "0011-01-01 00:00:00 BC",
        ),
        (
            "week",
            "0001-05-19 15:23:45.678901 BC",
            "0001-05-15 00:00:00 BC",
        ),
        ("week", "1969-12-31 23:59:59.999999", "1969-12-29 00:00:00"),
        (
            "milliseconds",
            "1969-12-31 23:59:59.999999",
            "1969-12-31 23:59:59.999",
        ),
        (
            "milliseconds",
            "0011-01-02 03:04:05.678901 BC",
            "0011-01-02 03:04:05.678 BC",
        ),
    ] {
        assert_eq!(
            evaluate(unit, &timestamp(input)),
            timestamp(expected),
            "{unit}: {input}"
        );
    }
}

#[test]
fn timestamp_truncation_checks_the_postgresql_julian_result_boundary() {
    for (unit, input) in [
        ("millennium", "4713-01-01 BC"),
        ("century", "4713-01-01 BC"),
        ("decade", "4713-01-01 BC"),
        ("year", "4714-11-24 BC"),
    ] {
        let error =
            truncate(unit, &timestamp(input), &ProductionControl::uncontrolled()).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22008"), "{unit}: {input}");
        assert_eq!(error.to_string(), "timestamp out of range");
        assert_eq!(error.detail(), None);
        assert_eq!(error.hint(), None);
    }
    for (unit, input) in [
        ("year", "4713-01-01 BC"),
        ("day", "4713-01-01 BC"),
        ("microseconds", "4714-11-24 BC"),
        ("week", "4714-11-24 BC"),
    ] {
        let input = timestamp(input);
        assert_eq!(evaluate(unit, &input), input, "{unit}");
    }
}

#[test]
fn unit_aliases_use_interval_tokens_and_their_ten_byte_comparison() {
    for (expected, aliases) in [
        (
            Unit::Millennium,
            &["mil", "mils", "millennia", "millennium", "millennium_extra"][..],
        ),
        (Unit::Century, &["c", "cent", "century", "centuries"]),
        (Unit::Decade, &["dec", "decs", "decade", "decades"]),
        (Unit::Year, &["y", "yr", "yrs", "year", "years"]),
        (Unit::Quarter, &["qtr", "quarter"]),
        (Unit::Month, &["mon", "mons", "month", "months"]),
        (Unit::Day, &["d", "day", "days"]),
        (Unit::Hour, &["h", "hr", "hrs", "hour", "hours"]),
        (Unit::Minute, &["m", "min", "mins", "minute", "minutes"]),
        (Unit::Second, &["s", "sec", "secs", "second", "seconds"]),
        (
            Unit::Milliseconds,
            &[
                "ms",
                "msec",
                "msecs",
                "msecond",
                "mseconds",
                "millisecond",
                "MILLISECONDS_EXTRA",
            ],
        ),
        (
            Unit::Microseconds,
            &[
                "us",
                "usec",
                "usecs",
                "usecond",
                "useconds",
                "microsecond",
                "microseconds_extra",
            ],
        ),
    ] {
        for alias in aliases {
            assert_eq!(decode_unit(alias, "interval").unwrap(), expected, "{alias}");
        }
    }
    for alias in ["w", "week", "weeks"] {
        assert_eq!(
            decode_unit(alias, "timestamp without time zone").unwrap(),
            Unit::Week
        );
    }
}

#[test]
fn unit_diagnostics_distinguish_unknown_from_unsupported_without_trimming() {
    for type_name in [
        "interval",
        "timestamp without time zone",
        "timestamp with time zone",
    ] {
        for unit in [
            "EPOCH",
            "dow",
            "doy",
            "isoyear",
            "quarters",
            "fortnight",
            "",
            " day",
            "day ",
        ] {
            let error = decode_unit(unit, type_name).unwrap_err();
            assert_eq!(error.sqlstate(), Some("22023"));
            assert_eq!(
                error.to_string(),
                format!(
                    "unit \"{}\" not recognized for type {type_name}",
                    unit.to_ascii_lowercase()
                )
            );
            assert_eq!(error.detail(), None);
        }
        for unit in ["timezone", "timezone_hour", "timezone_minute"] {
            let error = decode_unit(unit, type_name).unwrap_err();
            assert_eq!(error.sqlstate(), Some("0A000"));
            assert_eq!(
                error.to_string(),
                format!("unit \"{unit}\" not supported for type {type_name}")
            );
            assert_eq!(error.detail(), None);
        }
    }
    let error = decode_unit("WEEKS", "interval").unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(
        error.to_string(),
        "unit \"weeks\" not supported for type interval"
    );
    assert_eq!(
        error.detail(),
        Some("Months usually have fractional weeks.")
    );
    assert_eq!(error.hint(), None);
    let error = decode_unit(&"X".repeat(80), "interval").unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "unit \"{}\" not recognized for type interval",
            "x".repeat(63)
        )
    );
    let error = decode_unit(&"é".repeat(40), "interval").unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "unit \"{}\" not recognized for type interval",
            "é".repeat(31)
        )
    );
}

#[test]
fn strict_nulls_and_retained_production_preserve_the_existing_scope() {
    let scalar = crate::expr::scalar_dispatch::eval_scalar_function;
    for args in [
        vec![Value::Null, interval(0, 1, 0)],
        vec![Value::Str("invalid".into()), Value::Null],
    ] {
        assert_eq!(scalar("date_trunc", &args).unwrap(), Value::Null);
    }
    let args = [Value::Str("msecs".into()), interval(1, -2, -1_234_567)];
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output =
        crate::expr::scalar_dispatch::eval_generated_scalar_function("date_trunc", &args, &control)
            .unwrap();
    assert_eq!(*output, interval(1, -2, -1_234_000));
    assert_eq!(output.reserved_bytes(), 0);
    assert_eq!(budget.used(), 0);
    let empty = MemoryBudget::new(0);
    let control = ProductionControl::new(&empty, &token, &token);
    let error =
        crate::expr::scalar_dispatch::eval_generated_scalar_function("date_trunc", &args, &control)
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = truncate("day", &args[1], &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}
