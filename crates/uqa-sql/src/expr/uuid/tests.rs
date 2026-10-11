//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UUID shifts preserve the temporal semantics and timestamp bits of `PostgreSQL`.

use super::*;
use std::cell::Cell;
use uqa_core::TemporalInfinity;

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
    fn temporal_time_zone(&self) -> Result<Option<uqa_core::TemporalTimeZone>> {
        self.reads.set(self.reads.get() + 1);
        Ok(Some(
            uqa_core::TemporalTimeZone::named(self.zone.get()).unwrap(),
        ))
    }
}

#[test]
fn infinite_and_pre_epoch_shifts_match_postgresql_uuid_results() {
    // PostgreSQL 18.4 uuid_extract_timestamp(uuidv7('+/-infinity'::interval)).
    let expected =
        Value::Temporal(TemporalValue::parse_timestamp_tz("4069-05-05 10:29:56.873+00").unwrap());
    for infinity in [TemporalInfinity::Positive, TemporalInfinity::Negative] {
        let uuid =
            generate_uuid_v7_at_clock(1_710_003_600_123_456_789, Some(&infinity.interval()), None)
                .unwrap();
        assert_eq!(
            extract_uuid_timestamp(&Value::Str(uuid), &ProductionControl::uncontrolled()).unwrap(),
            expected
        );
    }
    let before_epoch = TemporalValue::Interval {
        months: -1_200,
        days: 0,
        micros: 0,
    };
    assert!(
        generate_uuid_v7_at_clock(1_710_003_600_123_456_789, Some(&before_epoch), None).is_ok()
    );
}

#[test]
fn shifted_uuid_uses_live_calendar_steps_and_preserves_elapsed_hours() {
    // Fixed clock and PostgreSQL calendar results from the zoned interval oracle.
    let session = Session {
        zone: Cell::new("UTC"),
        reads: Cell::new(0),
    };
    let clock = 1_710_003_600_123_456_789;
    for (zone, days, micros, expected) in [
        ("UTC", 1, 0, "2024-03-10 17:00:00.123+00"),
        ("America/New_York", 1, 0, "2024-03-10 16:00:00.123+00"),
        (
            "America/New_York",
            0,
            86_400_000_000,
            "2024-03-10 17:00:00.123+00",
        ),
    ] {
        session.zone.set(zone);
        let shift = TemporalValue::Interval {
            months: 0,
            days,
            micros,
        };
        let uuid = generate_uuid_v7_at_clock(clock, Some(&shift), Some(&session)).unwrap();
        assert_eq!(
            extract_uuid_timestamp(&Value::Str(uuid), &ProductionControl::uncontrolled()).unwrap(),
            Value::Temporal(TemporalValue::parse_timestamp_tz(expected).unwrap())
        );
    }
    assert_eq!(session.reads.get(), 2);
}

#[test]
fn bound_and_qualified_uuid_calls_retain_session_context_and_strict_nulls() {
    let session = Session {
        zone: Cell::new("America/New_York"),
        reads: Cell::new(0),
    };
    let context = EvalContext::new(None, &[]).with_engine(&session);
    for name in ["uuidv7", "pg_catalog.uuidv7"] {
        let argument = Value::Temporal(TemporalValue::Interval {
            months: 1,
            days: 0,
            micros: 0,
        });
        let uuid = crate::expr::eval_builtin_function_call(name, vec![(None, argument)], &context)
            .unwrap();
        assert_eq!(
            extract_uuid_version(&uuid, &ProductionControl::uncontrolled()).unwrap(),
            Value::Int(7)
        );
    }
    assert_eq!(session.reads.get(), 2);
    session.zone.set("invalid");
    assert_eq!(
        eval_uuid_v7(&[Value::Null], Some(&context)).unwrap(),
        Value::Null
    );
    assert_eq!(session.reads.get(), 2);
}
