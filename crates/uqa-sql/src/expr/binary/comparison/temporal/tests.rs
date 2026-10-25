//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{eval_comparison_truth_with_engine, BinaryOp};
use super::*;
use std::cell::Cell;
use uqa_core::{memory::MemoryBudget, CancellationToken, DatumValue, Value};

struct Session<'a> {
    zone: &'a str,
    calls: Cell<usize>,
    cancel: Option<&'a CancellationToken>,
}

impl EngineHook for Session<'_> {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }
    fn temporal_time_zone(&self) -> Result<Option<TemporalTimeZone>> {
        self.calls.set(self.calls.get() + 1);
        if let Some(token) = self.cancel {
            token.cancel();
        }
        Ok(TemporalTimeZone::named(self.zone))
    }
}

fn temporal(value: &serde_json::Value) -> Value {
    let text = value[1].as_str().unwrap();
    Value::Temporal(match value[0].as_str().unwrap() {
        "DATE" => TemporalValue::parse_date(text).unwrap(),
        "TIMESTAMP" => TemporalValue::parse_timestamp(text).unwrap(),
        "TIMESTAMPTZ" => TemporalValue::parse_timestamp_tz(text).unwrap(),
        other => panic!("unexpected reference type {other}"),
    })
}

#[test]
fn date_timestamp_operators_match_postgresql_in_each_session_zone() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    for case in fixture["cases"].as_array().unwrap() {
        let session = Session {
            zone: case["zone"].as_str().unwrap(),
            calls: Cell::new(0),
            cancel: None,
        };
        let left = temporal(&case["left"]);
        let right = temporal(&case["right"]);
        for control in [
            ProductionControl::uncontrolled(),
            ProductionControl::new(&budget, &token, &token),
        ] {
            for (index, op) in [
                BinaryOp::Equal,
                BinaryOp::NotEqual,
                BinaryOp::Less,
                BinaryOp::LessEqual,
                BinaryOp::Greater,
                BinaryOp::GreaterEqual,
            ]
            .into_iter()
            .enumerate()
            {
                assert_eq!(
                    eval_comparison_truth_with_engine(
                        op,
                        &left,
                        &right,
                        &control,
                        Some(&session),
                        None,
                    )
                    .unwrap(),
                    case["expected"][index].as_bool(),
                    "{}: {op:?}",
                    case["id"]
                );
                assert_eq!(budget.used(), 0);
            }
        }
    }
}

#[test]
fn temporal_comparison_observes_only_reached_civil_time_and_keeps_carrier_identity() {
    let session = Session {
        zone: "Asia/Seoul",
        calls: Cell::new(0),
        cancel: None,
    };
    let control = ProductionControl::uncontrolled();
    let date = Value::Temporal(TemporalValue::Date { days: 0 });
    let timestamp = Value::Temporal(TemporalValue::Timestamp {
        postgres_micros: (0) - uqa_core::POSTGRES_EPOCH_MICROS,
    });
    assert_ne!(date, timestamp);
    assert!(
        crate::expr::compare_typed_values_with_control(&date, &timestamp, &control)
            .unwrap()
            .is_lt()
    );
    for (left, right, expected) in [
        (date, timestamp, Some(true)),
        (
            Value::Temporal(TemporalInfinity::Positive.date()),
            Value::Temporal(TemporalInfinity::Positive.timestamp_tz()),
            Some(true),
        ),
        (
            Value::Null,
            Value::Temporal(TemporalValue::TimestampTz {
                postgres_micros: (0) - uqa_core::POSTGRES_EPOCH_MICROS,
            }),
            None,
        ),
    ] {
        assert_eq!(
            eval_comparison_truth_with_engine(
                BinaryOp::Equal,
                &left,
                &right,
                &control,
                Some(&session),
                None,
            )
            .unwrap(),
            expected
        );
    }
    assert_eq!(session.calls.get(), 0);
}

#[test]
fn retained_date_and_row_comparisons_keep_the_invoking_zone() {
    let session = Session {
        zone: "Asia/Seoul",
        calls: Cell::new(0),
        cancel: None,
    };
    let control = ProductionControl::uncontrolled();
    let date = Value::Datum(DatumValue::new(1082, 0, 7305_i32.to_le_bytes().to_vec()));
    let timestamp = Value::Datum(DatumValue::new(
        1184,
        0,
        631_152_000_000_000_i64.to_le_bytes().to_vec(),
    ));
    for (left, right) in [
        (date.clone(), timestamp.clone()),
        (
            Value::Row(vec![date].into()),
            Value::Row(vec![timestamp].into()),
        ),
    ] {
        assert_eq!(
            eval_comparison_truth_with_engine(
                BinaryOp::Less,
                &left,
                &right,
                &control,
                Some(&session),
                None,
            )
            .unwrap(),
            Some(true)
        );
    }
    assert_eq!(session.calls.get(), 2);
}

#[test]
fn date_timestamp_comparison_preserves_original_and_invoking_cancellation() {
    for (original_cancelled, cancel_in_zone) in [(true, false), (false, false), (false, true)] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else if !cancel_in_zone {
            invoking.cancel();
        }
        let budget = MemoryBudget::new(0);
        let control = ProductionControl::new(&budget, &original, &invoking);
        let session = Session {
            zone: "America/New_York",
            calls: Cell::new(0),
            cancel: cancel_in_zone.then_some(&invoking),
        };
        let error = eval_comparison_truth_with_engine(
            BinaryOp::Equal,
            &Value::Temporal(TemporalValue::Date { days: 0 }),
            &Value::Temporal(TemporalValue::TimestampTz {
                postgres_micros: (0) - uqa_core::POSTGRES_EPOCH_MICROS,
            }),
            &control,
            Some(&session),
            None,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
        assert_eq!(session.calls.get(), usize::from(cancel_in_zone));
    }
}
