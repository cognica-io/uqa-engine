//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_core::TemporalValue;

struct Session<'a> {
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
    fn resolve_type_name(&self, name: &str) -> std::result::Result<Option<ColumnType>, String> {
        Ok(ColumnType::from_sql_name(name).ok())
    }
    fn runtime_parameter(&self, name: &str) -> Result<Option<String>> {
        assert_eq!(name, "DateStyle");
        self.calls.set(self.calls.get() + 1);
        if let Some(token) = self.cancel {
            token.cancel();
        }
        Ok(Some("SQL, DMY".into()))
    }
}

#[test]
fn date_string_casts_share_live_output_and_destination_modifiers() {
    let value = Value::Temporal(TemporalValue::parse_date("2020-01-02").unwrap());
    let session = Session {
        calls: Cell::new(0),
        cancel: None,
    };
    for (target, expected) in [
        ("text", Value::Str("02/01/2020".into())),
        ("varchar(5)", Value::Str("02/01".into())),
        ("char(12)", Value::FixedChar("02/01/2020  ".into())),
    ] {
        assert_eq!(
            cast_value_with_type_resolution(&value, Some("date"), target, Some(&session)).unwrap(),
            expected
        );
    }
    assert_eq!(session.calls.get(), 3);
    let array =
        Value::Array(ArrayValue::with_lower_bounds(vec![value, Value::Null], vec![-1]).unwrap());
    let output =
        cast_value_with_type_resolution(&array, Some("date[]"), "text[]", Some(&session)).unwrap();
    assert_eq!(
        output,
        Value::Array(
            ArrayValue::with_lower_bounds(
                vec![Value::Str("02/01/2020".into()), Value::Null],
                vec![-1]
            )
            .unwrap()
        )
    );
    assert_eq!(session.calls.get(), 4);
    let output =
        cast_value_with_type_resolution(&Value::Null, Some("date"), "text", Some(&session))
            .unwrap();
    assert_eq!(output, Value::Null);
    assert_eq!(session.calls.get(), 4);
}

#[test]
fn date_output_callbacks_and_retained_text_share_the_production_scope() {
    let input = Value::Temporal(TemporalValue::parse_date("0001-02-03 BC").unwrap());
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let session = Session {
        calls: Cell::new(0),
        cancel: None,
    };
    let result = cast_value_with_type_resolution_with_control(
        &input,
        Some("date"),
        "text",
        Some(&session),
        &ProductionControl::new(&budget, &token, &token),
    )
    .unwrap();
    assert_eq!(*result, Value::Str("03/02/0001 BC".into()));
    assert_eq!(budget.used(), result.reserved_bytes());
    drop(result);
    assert_eq!(budget.used(), 0);
    let empty = MemoryBudget::new(0);
    let error = cast_value_with_type_resolution_with_control(
        &input,
        Some("date"),
        "text",
        Some(&session),
        &ProductionControl::new(&empty, &token, &token),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(empty.used(), 0);
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let session = Session {
            calls: Cell::new(0),
            cancel: Some(if cancel_original {
                &original
            } else {
                &invoking
            }),
        };
        let error = cast_value_with_type_resolution_with_control(
            &input,
            Some("date"),
            "text",
            Some(&session),
            &ProductionControl::new(&budget, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(session.calls.get(), 1);
        assert_eq!(budget.used(), 0);
    }
}
