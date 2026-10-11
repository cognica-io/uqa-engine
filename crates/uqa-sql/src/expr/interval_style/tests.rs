//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live interval input/output scopes preserve the caller and its admission controls.

use super::*;
use crate::expr::{
    cast_value, cast_value_with_type_resolution, eval_function_call, temporal_interval_style,
    EvalContext,
};
use std::cell::Cell;
use uqa_core::{memory::MemoryBudget, CancellationToken, TemporalIntervalStyle};

struct Session<'a> {
    style: Cell<&'static str>,
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
    fn temporal_interval_style(&self) -> Result<Option<TemporalIntervalStyle>> {
        if let Some(token) = self.cancel {
            token.cancel();
        }
        Ok(crate::semantics::parameters::intervalstyle::style(
            self.style.get(),
        ))
    }
}

#[test]
fn live_cast_and_output_scopes_follow_setting_changes_and_restore_the_caller() {
    let _caller = IntervalStyleScope::enter(TemporalIntervalStyle::Postgres);
    let session = Session {
        style: Cell::new("sql_standard"),
        cancel: None,
    };
    let source = Value::Str("-1 01:00:00".into());
    let value =
        cast_value_with_type_resolution(&source, Some("text"), "interval", Some(&session)).unwrap();
    let Value::Temporal(TemporalValue::Interval {
        months,
        days,
        micros,
    }) = value
    else {
        panic!("interval")
    };
    assert_eq!((months, days, micros), (0, -1, -3_600_000_000));
    assert_eq!(temporal_interval_style(), TemporalIntervalStyle::Postgres);
    let value = cast_value(&source, "interval").unwrap();
    session.style.set("iso_8601");
    assert_eq!(
        cast_value_with_type_resolution(&value, Some("interval"), "text", Some(&session)).unwrap(),
        Value::Str("P-1DT1H".into())
    );
    let context = EvalContext::new(None, &[]).with_engine(&session);
    assert_eq!(
        eval_function_call("to_json", vec![(None, value)], &context).unwrap(),
        Value::Json("\"P-1DT1H\"".into())
    );
    assert_eq!(temporal_interval_style(), TemporalIntervalStyle::Postgres);
    // The internal JSON carrier has a stable spelling independent of SQL presentation.
    let _style = IntervalStyleScope::enter(TemporalIntervalStyle::Iso8601);
    assert_eq!(
        crate::expr::core_value_to_json(&cast_value(&source, "interval").unwrap()).unwrap(),
        serde_json::json!("-1 days +01:00:00")
    );
}

#[test]
fn qualified_calls_and_assignment_text_keep_session_output_and_admission() {
    let _caller = IntervalStyleScope::enter(TemporalIntervalStyle::Postgres);
    let session = Session {
        style: Cell::new("iso_8601"),
        cancel: None,
    };
    let value = Value::Temporal(TemporalValue::Interval {
        months: 0,
        days: -1,
        micros: 3_600_000_000,
    });
    let context = EvalContext::new(None, &[]).with_engine(&session);
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    // PostgreSQL 18.4's interval output under iso_8601, through its quoting functions.
    for (name, expected) in [
        ("pg_catalog.quote_literal", Value::Str("'P-1DT1H'".into())),
        ("pg_catalog.quote_nullable", Value::Str("'P-1DT1H'".into())),
    ] {
        let mut arguments = uqa_core::memory::ProductionVec::new(control);
        let (argument, reservation) = control.copy_value(&value).unwrap().into_parts();
        arguments
            .push_produced(control.finish((None, argument), reservation).unwrap())
            .unwrap();
        let output = crate::expr::eval_builtin_function_call_with_control(
            name,
            None,
            arguments.finish().unwrap(),
            &control,
            &context,
        )
        .unwrap();
        assert_eq!(*output, expected);
        drop(output);
        assert_eq!(memory.used(), 0);
        assert_eq!(temporal_interval_style(), TemporalIntervalStyle::Postgres);
    }
    let _style = IntervalStyleScope::enter(TemporalIntervalStyle::Iso8601);
    let output = crate::expr::value_to_text_with_control(&value, &control).unwrap();
    assert_eq!(output.as_str(), "P-1DT1H");
    drop(output);
    assert_eq!(memory.used(), 0);
}

#[test]
fn style_lookup_needs_no_allocation_and_preserves_both_cancellation_scopes() {
    let _caller = IntervalStyleScope::enter(TemporalIntervalStyle::PostgresVerbose);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let empty = MemoryBudget::new(0);
    let session = Session {
        style: Cell::new("iso_8601"),
        cancel: None,
    };
    {
        let _style = live_scope(
            Some(&session),
            &ProductionControl::new(&empty, &original, &invoking),
        )
        .unwrap();
        assert_eq!(temporal_interval_style(), TemporalIntervalStyle::Iso8601);
        assert_eq!(empty.used(), 0);
    }
    assert_eq!(
        temporal_interval_style(),
        TemporalIntervalStyle::PostgresVerbose
    );
    for original_scope in [true, false] {
        let memory = MemoryBudget::new(1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let session = Session {
            style: Cell::new("iso_8601"),
            cancel: Some(if original_scope { &original } else { &invoking }),
        };
        let error = live_scope(
            Some(&session),
            &ProductionControl::new(&memory, &original, &invoking),
        )
        .err()
        .expect("cancelled lookup");
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
        assert_eq!(
            temporal_interval_style(),
            TemporalIntervalStyle::PostgresVerbose
        );
    }
}
