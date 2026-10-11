//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::{eval_builtin_function_call, eval_function_call, EngineHook};
use std::cell::Cell;
use uqa_core::{memory::MemoryBudget, CancellationToken, TemporalInfinity};

struct Session {
    zone: Cell<&'static str>,
    reads: Cell<usize>,
}

impl EngineHook for Session {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }
    fn runtime_parameter(&self, name: &str) -> Result<Option<String>> {
        assert_eq!(name, "TimeZone");
        self.reads.set(self.reads.get() + 1);
        Ok(Some(self.zone.get().into()))
    }
    fn call_scalar_function(&self, name: &str, _: &[Value]) -> Option<Result<Value>> {
        (name == "timezone").then_some(Ok(Value::Int(17)))
    }
}

#[test]
fn local_conversion_reads_current_session_only_after_strictness_and_infinity() {
    let session = Session {
        zone: Cell::new("Asia/Seoul"),
        reads: Cell::new(0),
    };
    let context = EvalContext::new(None, &[]).with_engine(&session);
    let value = Value::Temporal(
        TemporalValue::parse_timestamp_tz("2024-07-15 12:34:56.123456+00").unwrap(),
    );
    assert_eq!(
        eval_function_call("timezone", vec![(None, value.clone())], &context).unwrap(),
        Value::Int(17)
    );
    assert_eq!(session.reads.get(), 0);
    for (zone, expected) in [
        ("Asia/Seoul", "2024-07-15 21:34:56.123456"),
        ("America/New_York", "2024-07-15 08:34:56.123456"),
    ] {
        session.zone.set(zone);
        assert_eq!(
            eval_builtin_function_call("timezone", vec![(None, value.clone())], &context).unwrap(),
            Value::Temporal(TemporalValue::parse_timestamp(expected).unwrap())
        );
    }
    assert_eq!(session.reads.get(), 2);
    session.zone.set("Invalid/Zone");
    for (input, expected) in [
        (Value::Null, Value::Null),
        (
            Value::Temporal(TemporalInfinity::Positive.timestamp()),
            Value::Temporal(TemporalInfinity::Positive.timestamp_tz()),
        ),
        (
            Value::Temporal(TemporalInfinity::Negative.timestamp_tz()),
            Value::Temporal(TemporalInfinity::Negative.timestamp()),
        ),
    ] {
        assert_eq!(
            eval_builtin_function_call("timezone", vec![(None, input)], &context).unwrap(),
            expected
        );
    }
    assert_eq!(session.reads.get(), 2);
}

#[test]
fn controlled_zone_conversion_preserves_inline_results_and_cancellation() {
    use crate::expr::scalar_dispatch::{
        eval_generated_scalar_function, eval_generated_scalar_function_with_context,
    };
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(0);
    let control = ProductionControl::new(&budget, &token, &token);
    let value =
        Value::Temporal(TemporalValue::parse_timestamp("2024-07-15 12:34:56.123456").unwrap());
    let expected = Value::Temporal(
        TemporalValue::parse_timestamp_tz("2024-07-15 03:34:56.123456+00").unwrap(),
    );
    let args = [Value::Str("Asia/Seoul".into()), value.clone()];
    let result = eval_generated_scalar_function("timezone", &args, &control).unwrap();
    assert_eq!(*result, expected);
    assert_eq!(result.reserved_bytes(), 0);
    assert_eq!(budget.used(), 0);

    let session = Session {
        zone: Cell::new("Asia/Seoul"),
        reads: Cell::new(0),
    };
    let context = EvalContext::new(None, &[]).with_engine(&session);
    let result =
        eval_generated_scalar_function_with_context("timezone", &[value], &control, &context)
            .unwrap();
    assert_eq!(*result, expected);
    assert_eq!(result.reserved_bytes(), 0);
    assert_eq!(session.reads.get(), 1);
    assert_eq!(budget.used(), 0);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert_eq!(
            eval_generated_scalar_function("timezone", &args, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
    }
}

#[test]
fn finite_conversion_rejects_overflow_without_creating_an_infinite_value() {
    let args = [
        Value::Temporal(TemporalValue::Interval {
            months: 0,
            days: 0,
            micros: -1_000_000,
        }),
        Value::Temporal(TemporalValue::Timestamp { micros: i64::MAX }),
    ];
    assert_eq!(
        eval_timezone(&args, None, &ProductionControl::uncontrolled())
            .unwrap_err()
            .sqlstate(),
        Some("22008")
    );
}
