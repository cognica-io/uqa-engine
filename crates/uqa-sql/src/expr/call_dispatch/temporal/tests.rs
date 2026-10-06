//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::{eval_builtin_function_call, eval_function_call, EngineHook};
use std::cell::Cell;

struct Session {
    zone: Cell<&'static str>,
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
        Ok(Some(self.zone.get().to_string()))
    }
    fn call_scalar_function(&self, name: &str, _: &[Value]) -> Option<Result<Value>> {
        (self.override_call && matches!(name, "extract" | "date_part"))
            .then_some(Ok(Value::Int(17)))
    }
}

fn arguments(field: Value) -> Vec<(Option<String>, Value)> {
    vec![
        (None, field),
        (
            None,
            Value::Temporal(TemporalValue::parse_timestamp_tz("2024-01-02 03:04:05+00").unwrap()),
        ),
    ]
}

#[test]
fn selected_extraction_uses_invoking_zone_and_retains_dynamic_callback_precedence() {
    let session = Session {
        zone: Cell::new("Asia/Seoul"),
        override_call: true,
    };
    let context = EvalContext::new(None, &[]).with_engine(&session);
    for name in ["extract", "date_part"] {
        assert_eq!(
            eval_function_call(name, arguments(Value::Str("hour".into())), &context).unwrap(),
            Value::Int(17)
        );
        for (zone, expected_hour) in [
            ("Asia/Seoul", "12"),
            ("UTC", "3"),
            ("America/New_York", "22"),
        ] {
            session.zone.set(zone);
            let result =
                eval_builtin_function_call(name, arguments(Value::Str("hour".into())), &context)
                    .unwrap();
            assert_eq!(
                value_to_string(&result).unwrap(),
                expected_hour,
                "{name} {zone}"
            );
            if name == "extract" {
                assert!(matches!(result, Value::Decimal(_)));
            } else {
                assert!(matches!(result, Value::Float(_)));
            }
            let epoch =
                eval_builtin_function_call(name, arguments(Value::Str("epoch".into())), &context)
                    .unwrap();
            assert_eq!(
                value_to_string(&epoch).unwrap(),
                if name == "extract" {
                    "1704164645.000000"
                } else {
                    "1704164645"
                }
            );
        }
    }
}

#[test]
fn null_extraction_does_not_read_the_session_zone() {
    let session = Session {
        zone: Cell::new("Not/A_Zone"),
        override_call: false,
    };
    let context = EvalContext::new(None, &[]).with_engine(&session);
    for name in ["extract", "date_part"] {
        assert_eq!(
            eval_builtin_function_call(name, arguments(Value::Null), &context).unwrap(),
            Value::Null
        );
        assert_eq!(
            eval_builtin_function_call(
                name,
                vec![(None, Value::Str("nonesuch".into())), (None, Value::Null)],
                &context
            )
            .unwrap(),
            Value::Null
        );
    }
}
