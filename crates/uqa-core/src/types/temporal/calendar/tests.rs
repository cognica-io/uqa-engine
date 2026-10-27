//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, TemporalValue,
};

#[test]
fn civil_fields_match_postgresql_across_the_full_date_domain() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("tests/civil.json")).unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let date = TemporalDate::from_unix_days(case["unix_days"].as_i64().unwrap() as i32);
        let sql_year = |year: i32| if year <= 0 { year - 1 } else { year };
        for (field, actual) in [
            ("year", sql_year(date.year())),
            ("month", date.month() as i32),
            ("day", date.day() as i32),
            ("doy", date.ordinal() as i32),
            ("dow", date.weekday_sunday() as i32),
            ("isodow", date.weekday_monday() as i32 + 1),
            ("isoyear", sql_year(date.iso_week().0)),
            ("week", date.iso_week().1 as i32),
        ] {
            assert_eq!(
                i64::from(actual),
                case[field].as_i64().unwrap(),
                "{field}: {case}"
            );
        }
        assert_eq!(
            TemporalDate::from_calendar(date.year(), date.month(), date.day()),
            Some(date)
        );
    }
    for days in [i32::MIN, i32::MIN + 1, i32::MAX - 1, i32::MAX] {
        assert!((1..=53).contains(&TemporalDate::from_unix_days(days).iso_week().1));
    }
    for micros in [
        i64::MIN,
        -211_813_488_000_000_000,
        -1,
        0,
        1,
        9_223_371_331_199_999_999,
        i64::MAX,
    ] {
        assert_eq!(
            TemporalDateTime::from_postgres_micros(micros).postgres_micros(),
            Some(micros)
        );
    }
}

#[test]
fn full_date_input_and_output_match_postgresql_boundaries() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("tests/postgresql.json")).unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let value = TemporalValue::date_input(input, 0);
        if case["expected"].get("error").is_some() {
            assert!(value.is_err(), "{input}");
        } else {
            let value = value.unwrap_or_else(|error| panic!("{input}: {error:?}"));
            let expected_days = case["expected"]["days"]
                .as_str()
                .unwrap()
                .parse::<i32>()
                .unwrap();
            assert_eq!(
                value,
                TemporalValue::Date {
                    days: expected_days
                },
                "{input}"
            );
            assert_eq!(
                value.to_sql_string(),
                case["expected"]["output"].as_str().unwrap()
            );
        }
    }
}

#[test]
fn full_date_output_preserves_memory_and_both_cancellation_limits() {
    let value = TemporalValue::date_input("5874897-12-31", 0).unwrap();
    for limit in [0, 1024] {
        let budget = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let result = value.to_sql_string_with_control(&control);
        if limit == 0 {
            assert!(result.is_err());
        } else {
            let result = result.unwrap();
            assert_eq!(*result, "5874897-12-31");
            assert_eq!(budget.used(), result.reserved_bytes());
        }
        assert_eq!(budget.used(), 0);
    }
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let budget = MemoryBudget::new(1024);
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert!(value.to_sql_string_with_control(&control).is_err());
        assert!(TemporalValue::date_input_with_control("5874897-12-31", 0, &control).is_err());
        assert_eq!(budget.used(), 0);
    }
}

proptest::proptest! {
    #[test]
    fn gregorian_day_conversion_roundtrips_the_integer_carrier(days in proptest::prelude::any::<i32>()) {
        let (year, month, day) = gregorian_reference(days);
        proptest::prop_assert_eq!(from_calendar(year, u32::try_from(month).unwrap(), u32::try_from(day).unwrap()), Some(days));
    }

    #[test]
    fn physical_date_output_preserves_the_admitted_gregorian_calendar(days in -2_440_588_i32..2_145_042_906_i32) {
        proptest::prop_assert!(valid_date(days));
        proptest::prop_assert_eq!(to_output_calendar(days), gregorian_reference(days));
    }
}

#[test]
fn retained_date_output_matches_postgresql_after_same_width_descriptor_change() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("tests/retained-postgresql.json")).unwrap();
    let days = reference["observation"]["unix_days"].as_i64().unwrap() as i32;
    let expected = reference["observation"]["output"].as_str().unwrap();
    for limit in [0, 1024] {
        let budget = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let value = TemporalValue::Date { days }.to_sql_string_with_control(&control);
        if limit == 0 {
            assert!(value.is_err());
        } else {
            let value = value.unwrap();
            assert_eq!(value.as_str(), expected);
            assert_eq!(budget.used(), value.reserved_bytes());
        }
        assert_eq!(budget.used(), 0);
    }
}

fn gregorian_reference(days: i32) -> (i64, i64, i64) {
    let days = i64::from(days) + 719_468;
    let era = days.div_euclid(146_097);
    let day_in_era = days - era * 146_097;
    let year_in_era =
        (day_in_era - day_in_era / 1460 + day_in_era / 36_524 - day_in_era / 146_096) / 365;
    let day_in_year = day_in_era - (365 * year_in_era + year_in_era / 4 - year_in_era / 100);
    let month = (5 * day_in_year + 2) / 153;
    let day = day_in_year - (153 * month + 2) / 5 + 1;
    let month = month + if month < 10 { 3 } else { -9 };
    let year = year_in_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
