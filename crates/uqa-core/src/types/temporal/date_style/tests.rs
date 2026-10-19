//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};

#[test]
fn date_output_styles_match_postgresql_without_changing_the_carrier() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("tests/postgresql.json")).unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 96);
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for case in cases {
        let value = TemporalValue::parse_date(case["input"].as_str().unwrap()).unwrap();
        let original = value.clone();
        let style = match case["style"].as_str().unwrap() {
            "ISO" => TemporalDateStyle::Iso,
            "SQL" => TemporalDateStyle::Sql,
            "Postgres" => TemporalDateStyle::Postgres,
            "German" => TemporalDateStyle::German,
            _ => unreachable!(),
        };
        let order = match case["order"].as_str().unwrap() {
            "MDY" => TemporalDateOrder::MonthDayYear,
            "DMY" => TemporalDateOrder::DayMonthYear,
            "YMD" => TemporalDateOrder::YearMonthDay,
            _ => unreachable!(),
        };
        let output = value
            .to_sql_string_in_date_style_with_control(style, order, &control)
            .unwrap();
        assert_eq!(&**output, case["expected"].as_str().unwrap(), "{case}");
        assert_eq!(value, original);
        assert_eq!(budget.used(), output.capacity());
        if style == TemporalDateStyle::Iso {
            assert_eq!(value.to_sql_string(), **output, "canonical output: {case}");
        }
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn date_output_style_retains_quota_and_both_cancellation_scopes() {
    let value = TemporalValue::parse_date("0001-02-03 BC").unwrap();
    for style in [
        TemporalDateStyle::Iso,
        TemporalDateStyle::Sql,
        TemporalDateStyle::Postgres,
        TemporalDateStyle::German,
    ] {
        let empty = MemoryBudget::new(0);
        let token = CancellationToken::new();
        assert!(matches!(
            value.to_sql_string_in_date_style_with_control(
                style,
                TemporalDateOrder::DayMonthYear,
                &ProductionControl::new(&empty, &token, &token)
            ),
            Err(ValueRetentionError::Memory(_))
        ));
        assert_eq!(empty.used(), 0);
        for cancel_original in [true, false] {
            let budget = MemoryBudget::new(4096);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
            assert!(matches!(
                value.to_sql_string_in_date_style_with_control(
                    style,
                    TemporalDateOrder::DayMonthYear,
                    &ProductionControl::new(&budget, &original, &invoking)
                ),
                Err(ValueRetentionError::Cancelled(_))
            ));
            assert_eq!(budget.used(), 0);
        }
    }
}
