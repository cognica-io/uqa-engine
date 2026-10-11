//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sleep duration semantics without blocking or measuring the host clock.

use super::*;
use crate::expr::EngineHook;
use std::cell::Cell;
use uqa_core::TemporalInfinity;

struct Session<'a> {
    duration: Cell<Option<Duration>>,
    zone: &'a str,
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
    fn sleep(&self, duration: Duration) -> Result<()> {
        self.duration.set(Some(duration));
        Ok(())
    }
    fn temporal_time_zone(&self) -> Result<Option<uqa_core::TemporalTimeZone>> {
        Ok(Some(uqa_core::TemporalTimeZone::named(self.zone).unwrap()))
    }
}

#[test]
fn infinite_sleep_inputs_preserve_direction_and_do_not_overflow() {
    for (infinity, expected) in [
        (TemporalInfinity::Positive, Duration::MAX),
        (TemporalInfinity::Negative, Duration::ZERO),
    ] {
        for (name, argument) in [
            ("pg_sleep_for", infinity.interval()),
            ("pg_sleep_until", infinity.timestamp_tz()),
        ] {
            let session = Session {
                duration: Cell::new(None),
                zone: "UTC",
            };
            let context = EvalContext::new(None, &[]).with_engine(&session);
            assert_eq!(
                eval_session_sleep(name, &[Value::Temporal(argument)], &context)
                    .unwrap()
                    .unwrap(),
                Value::Void
            );
            assert_eq!(
                session.duration.get(),
                Some(expected),
                "{name} {infinity:?}"
            );
        }
    }
}

fn bytes<const N: usize>(text: &str) -> [u8; N] {
    assert_eq!(text.len(), N * 2);
    std::array::from_fn(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
}

#[test]
fn duration_matches_postgresql_calendar_and_numeric_epoch_with_two_clock_reads() {
    let reference: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("tests/oracle.json")).unwrap();
    for case in reference {
        let session = Session {
            duration: Cell::new(None),
            zone: case["zone"].as_str().unwrap(),
        };
        let mut clocks = ["before", "after"]
            .map(|field| {
                i64::from_be_bytes(bytes(case[field].as_str().unwrap())) + 946_684_800_000_000
            })
            .into_iter();
        let span: [u8; 16] = bytes(case["span"].as_str().unwrap());
        let span = Value::Temporal(TemporalValue::Interval {
            micros: i64::from_be_bytes(span[..8].try_into().unwrap()),
            days: i32::from_be_bytes(span[8..12].try_into().unwrap()),
            months: i32::from_be_bytes(span[12..].try_into().unwrap()),
        });
        let actual = sleep_seconds("pg_sleep_for", &span, Some(&session), &mut || {
            clocks.next().unwrap()
        })
        .unwrap();
        assert_eq!(
            actual.to_bits(),
            u64::from_be_bytes(bytes(case["seconds"].as_str().unwrap())),
            "{case}"
        );
        assert!(clocks.next().is_none());
        assert_eq!(session.duration.get(), None);
    }
}

#[test]
fn null_sleep_arguments_do_not_enter_the_session_wait() {
    let session = Session {
        duration: Cell::new(None),
        zone: "invalid",
    };
    let context = EvalContext::new(None, &[]).with_engine(&session);
    for name in ["pg_sleep", "pg_sleep_for", "pg_sleep_until"] {
        assert_eq!(
            eval_session_sleep(name, &[Value::Null], &context)
                .unwrap()
                .unwrap(),
            Value::Null
        );
        assert_eq!(session.duration.get(), None);
    }
}
