//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::{eval_builtin_function_call, eval_function_call, EngineHook};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn timestamp(value: &str) -> Value {
    Value::Temporal(TemporalValue::parse_timestamp_tz(value).unwrap())
}

#[test]
fn local_calendar_and_subday_offsets_match_postgresql_transition_results() {
    // Independent date_trunc_timezone_oracle messages, including historical second offsets.
    for (input, zone, unit, expected_seconds) in [
        ("2024-01-02 03:04:05+00", "Asia/Seoul", "day", 1_704_121_200),
        (
            "2024-03-10 07:30:00+00",
            "America/New_York",
            "day",
            1_710_046_800,
        ),
        (
            "2024-11-03 05:30:00+00",
            "America/New_York",
            "hour",
            1_730_610_000,
        ),
        (
            "2024-11-03 06:30:00+00",
            "America/New_York",
            "hour",
            1_730_613_600,
        ),
        (
            "2024-10-05 15:45:12+00",
            "Australia/Lord_Howe",
            "hour",
            1_728_140_400,
        ),
        (
            "2024-09-08 04:30:00+00",
            "America/Santiago",
            "day",
            1_725_768_000,
        ),
        (
            "2024-11-03 05:30:00+00",
            "America/Havana",
            "day",
            1_730_610_000,
        ),
        (
            "1900-01-01 12:34:56+00",
            "Europe/Amsterdam",
            "day",
            -2_208_989_972,
        ),
        (
            "12000-07-01 12:34:56+00",
            "America/New_York",
            "day",
            316_531_944_000,
        ),
    ] {
        let value = truncate_explicit_zone(
            unit,
            &timestamp(input),
            zone,
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
        assert_eq!(
            value,
            Value::Temporal(TemporalValue::TimestampTz {
                micros: expected_seconds * 1_000_000
            }),
            "{input} {zone} {unit}"
        );
    }
}

#[test]
fn explicit_zone_diagnostics_precede_unit_validation_and_preserve_spelling() {
    let input = timestamp("2024-01-02 03:04:05+00");
    let control = ProductionControl::uncontrolled();
    let error = truncate_explicit_zone("nonesuch", &input, "Not/A_Zone", &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22023"));
    assert_eq!(error.to_string(), "time zone \"Not/A_Zone\" not recognized");
    let name = "가".repeat(86);
    let error = truncate_explicit_zone("day", &input, &name, &control).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("time zone \"{}\" not recognized", "가".repeat(85))
    );
    let minimum = timestamp("4714-11-24 00:00:00+00 BC");
    assert_eq!(
        truncate_explicit_zone("microseconds", &minimum, "UTC+12", &control).unwrap(),
        minimum
    );
    assert_eq!(
        truncate_explicit_zone("day", &minimum, "UTC+12", &control)
            .unwrap_err()
            .sqlstate(),
        Some("22008")
    );
}

struct Session {
    zone: &'static str,
    override_call: bool,
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
        Ok(Some(self.zone.to_string()))
    }
    fn call_scalar_function(&self, name: &str, _: &[Value]) -> Option<Result<Value>> {
        (self.override_call && name == "date_trunc").then_some(Ok(Value::Int(17)))
    }
}

#[test]
fn session_zone_obeys_selected_builtin_and_dynamic_callback_precedence() {
    let args = vec![
        (None, Value::Str("day".into())),
        (None, timestamp("2024-01-02 03:04:05+00")),
    ];
    let session = Session {
        zone: "Asia/Seoul",
        override_call: true,
    };
    let context = EvalContext::new(None, &[]).with_engine(&session);
    assert_eq!(
        eval_function_call("date_trunc", args.clone(), &context).unwrap(),
        Value::Int(17)
    );
    assert_eq!(
        eval_builtin_function_call("date_trunc", args, &context).unwrap(),
        Value::Temporal(TemporalValue::TimestampTz {
            micros: 1_704_121_200_000_000
        })
    );
    let invalid = Session {
        zone: "Invalid/Zone",
        override_call: false,
    };
    let context = EvalContext::new(None, &[]).with_engine(&invalid);
    assert_eq!(
        eval_builtin_function_call(
            "date_trunc",
            vec![
                (None, Value::Null),
                (None, timestamp("2024-01-02 00:00:00+00"))
            ],
            &context
        )
        .unwrap(),
        Value::Null
    );
}

#[test]
fn explicit_zone_strictness_and_control_preserve_nulls_and_resource_scopes() {
    let scalar = crate::expr::scalar_dispatch::eval_scalar_function;
    let values = [
        Value::Str("nonesuch".into()),
        timestamp("2024-01-02 00:00:00+00"),
        Value::Str("Invalid/Zone".into()),
    ];
    for index in 0..3 {
        let mut args = values.clone();
        args[index] = Value::Null;
        assert_eq!(scalar("date_trunc", &args).unwrap(), Value::Null);
    }
    let args = [
        Value::Str("day".into()),
        timestamp("2024-01-02 03:04:05+00"),
        Value::Str("Asia/Seoul".into()),
    ];
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let result =
        crate::expr::scalar_dispatch::eval_generated_scalar_function("date_trunc", &args, &control)
            .unwrap();
    assert_eq!(
        *result,
        Value::Temporal(TemporalValue::TimestampTz {
            micros: 1_704_121_200_000_000
        })
    );
    assert_eq!(result.reserved_bytes(), 0);
    assert_eq!(budget.used(), 0);
    let empty = MemoryBudget::new(0);
    let control = ProductionControl::new(&empty, &token, &token);
    assert_eq!(
        crate::expr::scalar_dispatch::eval_generated_scalar_function("date_trunc", &args, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    for original_cancelled in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert_eq!(
            truncate_explicit_zone("day", &args[1], "Asia/Seoul", &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
