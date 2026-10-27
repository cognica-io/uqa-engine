//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` calendar arithmetic, session lookup and production controls.

use super::*;
use crate::{
    ast::BinaryOp,
    expr::{eval_binary_values_with_engine, EngineHook},
    SQLError,
};
use std::cell::Cell;
use uqa_core::{memory::MemoryBudget, CancellationToken, TemporalInfinity, Value};

struct Session<'a> {
    zone: &'a str,
    reads: Cell<usize>,
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
        self.reads.set(self.reads.get() + 1);
        if let Some(token) = self.cancel {
            token.cancel();
        }
        TemporalTimeZone::named(self.zone)
            .map(Some)
            .ok_or_else(|| SQLError::Internal("unexpected zone lookup".into()))
    }
}

fn bytes<const N: usize>(text: &str) -> [u8; N] {
    assert_eq!(text.len(), N * 2);
    std::array::from_fn(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
}

fn timestamp(text: &str, zoned: bool) -> Value {
    let pg = i64::from_be_bytes(bytes(text));
    let infinity = match pg {
        i64::MIN => Some(TemporalInfinity::Negative),
        i64::MAX => Some(TemporalInfinity::Positive),
        _ => None,
    };
    Value::Temporal(if let Some(infinity) = infinity {
        if zoned {
            infinity.timestamp_tz()
        } else {
            infinity.timestamp()
        }
    } else {
        let micros = pg;
        if zoned {
            TemporalValue::TimestampTz {
                postgres_micros: micros,
            }
        } else {
            TemporalValue::Timestamp {
                postgres_micros: micros,
            }
        }
    })
}

fn interval(text: &str) -> Value {
    let data = bytes::<16>(text);
    Value::Temporal(TemporalValue::Interval {
        micros: i64::from_be_bytes(data[..8].try_into().unwrap()),
        days: i32::from_be_bytes(data[8..12].try_into().unwrap()),
        months: i32::from_be_bytes(data[12..].try_into().unwrap()),
    })
}

#[test]
fn calendar_arithmetic_matches_postgresql_physical_results_and_diagnostics() {
    let reference: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("tests/oracle.json")).unwrap();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(0);
    let control = ProductionControl::new(&memory, &token, &token);
    let mut differences = Vec::new();
    for case in &reference {
        let session = Session {
            zone: case["zone"].as_str().unwrap(),
            reads: Cell::new(0),
            cancel: None,
        };
        let zoned = case["kind"] == "timestamptz";
        let base = timestamp(case["base"].as_str().unwrap(), zoned);
        let span = interval(case["span"].as_str().unwrap());
        let (left, right) = if case["reverse"] == true {
            (&span, &base)
        } else {
            (&base, &span)
        };
        let op = if case["op"] == "-" {
            BinaryOp::Subtract
        } else {
            BinaryOp::Add
        };
        let result =
            eval_binary_values_with_engine(op, left, right, None, Some(&session), &control);
        let matches = match &result {
            Ok(value) => {
                case["error"].is_null()
                    && **value == timestamp(case["result"].as_str().unwrap(), zoned)
            }
            Err(error) => {
                case["error"]["sqlstate"].as_str() == error.sqlstate()
                    && case["error"]["message"] == error.to_string()
            }
        };
        if !matches {
            differences.push(format!("{case}\nobserved: {result:?}"));
        }
        drop(result);
        assert_eq!(memory.used(), 0);
    }
    assert!(
        differences.is_empty(),
        "{} of {} differ:\n{}",
        differences.len(),
        reference.len(),
        differences.join("\n\n")
    );
}

#[test]
fn zone_lookup_follows_strictness_infinity_and_elapsed_time_and_retains_cancellation() {
    let session = Session {
        zone: "invalid",
        reads: Cell::new(0),
        cancel: None,
    };
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let finite = Value::Temporal(TemporalValue::TimestampTz {
        postgres_micros: 1_710_003_600_000_000,
    });
    let day = Value::Temporal(TemporalValue::Interval {
        months: 0,
        days: 1,
        micros: 0,
    });
    let elapsed = Value::Temporal(TemporalValue::Interval {
        months: 0,
        days: 0,
        micros: 86_400_000_000,
    });
    let infinity = Value::Temporal(TemporalInfinity::Positive.timestamp_tz());
    for (base, span) in [(&Value::Null, &day), (&infinity, &day), (&finite, &elapsed)] {
        eval_binary_values_with_engine(BinaryOp::Add, base, span, None, Some(&session), &control)
            .unwrap();
    }
    assert_eq!(session.reads.get(), 0);
    for original_scope in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let session = Session {
            zone: "America/New_York",
            reads: Cell::new(0),
            cancel: Some(if original_scope { &original } else { &invoking }),
        };
        let control = ProductionControl::new(&memory, &original, &invoking);
        let error = eval_binary_values_with_engine(
            BinaryOp::Add,
            &finite,
            &day,
            None,
            Some(&session),
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}
