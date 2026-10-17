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
        let (year, month, day) = to_calendar(days);
        proptest::prop_assert_eq!(from_calendar(year, u32::try_from(month).unwrap(), u32::try_from(day).unwrap()), Some(days));
    }
}
