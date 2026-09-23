//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn standalone_catalog_casts_keep_oid_output_and_array_lower_bounds() {
    let budget = MemoryBudget::new(128 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let zero = cast_value_with_type_resolution_with_control(
        &Value::Int(0),
        Some("regtype"),
        "text",
        None,
        &control,
    )
    .unwrap();
    assert_eq!(*zero, Value::Str("-".into()));
    assert_eq!(budget.used(), zero.reserved_bytes());
    drop(zero);
    let catalog = Catalog::default();
    for (engine, expected) in [
        (None, [Value::Str("0".into()), Value::Str("42".into())]),
        (
            Some(&catalog as &dyn EngineHook),
            [Value::Int(0), Value::Int(42)],
        ),
    ] {
        let roles = cast_value_with_type_resolution_with_control(
            &Value::Str("[-1:0]={0,42}".into()),
            None,
            "regrole[]",
            engine,
            &control,
        )
        .unwrap();
        let Value::Array(array) = &*roles else {
            panic!("role array")
        };
        assert_eq!(array.lower_bounds(), &[-1]);
        assert_eq!(array.elements(), &expected);
        assert_eq!(budget.used(), roles.reserved_bytes());
        drop(roles);
        assert_eq!(budget.used(), 0);
    }
    let input = Value::Array(
        ArrayValue::with_lower_bounds(
            vec![
                Value::List(vec![Value::Int(0), Value::Int(1)]),
                Value::List(vec![Value::Int(2), Value::Null]),
            ],
            vec![-2, 3],
        )
        .unwrap(),
    );
    let output = cast_value_with_type_resolution_with_control(
        &input,
        Some("regtype[]"),
        "text",
        None,
        &control,
    )
    .unwrap();
    assert_eq!(*output, Value::Str("[-2:-1][3:4]={{-,1},{2,NULL}}".into()));
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[derive(Default)]
struct Catalog<'a> {
    cancel_on_type: Option<&'a CancellationToken>,
    domain_error: Option<&'a CancellationToken>,
    cancel_on_input: Option<&'a CancellationToken>,
    input_error: bool,
    type_calls: std::cell::Cell<usize>,
}

impl EngineHook for Catalog<'_> {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!("cast does not call sequence hooks")
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!("cast does not call sequence hooks")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!("cast does not call sequence hooks")
    }

    fn resolve_type_name(&self, name: &str) -> std::result::Result<Option<ColumnType>, String> {
        self.type_calls.set(self.type_calls.get() + 1);
        if let Some(token) = self.cancel_on_type {
            token.cancel();
        }
        let ty = match name {
            "label" | "app.label" => Some(ColumnType::Domain {
                schema: "app".into(),
                name: "label".into(),
                oid: 16384,
                base: Box::new(ColumnType::Text),
            }),
            _ => None,
        };
        Ok(ty)
    }

    fn cast_domain(&self, _: &Value, _: Option<&str>, ty: &ColumnType) -> Result<Option<Value>> {
        if !matches!(ty, ColumnType::Domain { .. }) {
            return Ok(None);
        }
        if let Some(token) = self.domain_error {
            token.cancel();
            return Err(SQLError::Routine {
                sqlstate: "23514".into(),
                message: "domain check failed".into(),
            });
        }
        let mut text = String::with_capacity(1024);
        text.push_str("domain output");
        Ok(Some(Value::Str(text)))
    }

    fn resolve_regrole(&self, name: &str) -> Result<Option<i64>> {
        if let Some(token) = self.cancel_on_input {
            token.cancel();
        }
        if self.input_error {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "catalog input denied".into(),
            });
        }
        Ok(match name {
            "0" => Some(0),
            "42" => Some(42),
            _ => None,
        })
    }

    fn resolve_regtype_output(
        &self,
        _: &ColumnType,
        oid: i64,
    ) -> std::result::Result<Option<String>, String> {
        if oid != 42 {
            return Ok(None);
        }
        let mut text = String::with_capacity(256);
        text.push_str("app.label");
        Ok(Some(text))
    }
}

#[test]
fn catalog_cast_outputs_retain_external_capacity_and_release_type_workspaces() {
    let budget = MemoryBudget::new(128 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let catalog = Catalog {
        cancel_on_type: None,
        domain_error: None,
        ..Catalog::default()
    };
    let output = cast_value_with_type_resolution_with_control(
        &Value::Str("input".into()),
        None,
        "label",
        Some(&catalog),
        &control,
    )
    .unwrap();
    assert_eq!(*output, Value::Str("domain output".into()));
    assert_eq!(output.reserved_bytes(), 1024);
    assert_eq!(budget.used(), 1024);
    drop(output);
    let text = format_regtype_value_with_control(
        &Value::Int(42),
        &ColumnType::Regtype,
        Some(&catalog),
        &control,
    )
    .unwrap()
    .unwrap();
    assert_eq!(&*text, "app.label");
    assert_eq!(text.reserved_bytes(), 256);
    assert_eq!(budget.used(), 256);
    drop(text);
    assert_eq!(budget.used(), 0);
}

#[test]
fn catalog_cast_quota_failure_keeps_existing_results_and_input_unchanged() {
    let budget = MemoryBudget::new(512);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let held = control.copy_text("retained result").unwrap();
    let used = budget.used();
    let catalog = Catalog {
        cancel_on_type: None,
        domain_error: None,
        ..Catalog::default()
    };
    let input = Value::Str("input".into());
    let error = cast_value_with_type_resolution_with_control(
        &input,
        None,
        "label",
        Some(&catalog),
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(input, Value::Str("input".into()));
    assert_eq!(&*held, "retained result");
    assert_eq!(budget.used(), used);
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn catalog_callbacks_check_both_tokens_without_replacing_a_domain_error() {
    for original_cancelled in [true, false] {
        let budget = MemoryBudget::new(128 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let token = if original_cancelled {
            &original
        } else {
            &invoking
        };
        let control = ProductionControl::new(&budget, &original, &invoking);
        let catalog = Catalog {
            cancel_on_type: Some(token),
            domain_error: None,
            ..Catalog::default()
        };
        let error = cast_value_with_type_resolution_with_control(
            &Value::Str("input".into()),
            None,
            "label",
            Some(&catalog),
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(128 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let catalog = Catalog {
        cancel_on_type: None,
        domain_error: Some(&invoking),
        ..Catalog::default()
    };
    let error = cast_value_with_type_resolution_with_control(
        &Value::Str("input".into()),
        None,
        "label",
        Some(&catalog),
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23514"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancelled_absent_type_resolution_stops_before_the_source_type_lookup() {
    for original_cancelled in [true, false] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let token = if original_cancelled {
            &original
        } else {
            &invoking
        };
        let control = ProductionControl::new(&budget, &original, &invoking);
        let catalog = Catalog {
            cancel_on_type: Some(token),
            ..Catalog::default()
        };
        let error = cast_value_with_type_resolution_with_control(
            &Value::Int(1),
            Some("integer"),
            "text",
            Some(&catalog),
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(catalog.type_calls.get(), 1);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn cancelled_catalog_input_absence_is_distinct_from_an_earlier_typed_failure() {
    for original_cancelled in [true, false] {
        for input_error in [false, true] {
            let budget = MemoryBudget::new(4096);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let token = if original_cancelled {
                &original
            } else {
                &invoking
            };
            let control = ProductionControl::new(&budget, &original, &invoking);
            let catalog = Catalog {
                cancel_on_input: Some(token),
                input_error,
                ..Catalog::default()
            };
            let error = cast_value_with_type_resolution_with_control(
                &Value::Str("missing role".into()),
                None,
                "regrole",
                Some(&catalog),
                &control,
            )
            .unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some(if input_error { "42501" } else { "57014" })
            );
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn external_type_admission_failure_releases_partial_names_and_keeps_an_existing_result() {
    let budget = MemoryBudget::new(128);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let held = control.copy_text("earlier result").unwrap();
    let used = budget.used();
    let ty = ColumnType::Domain {
        schema: "app".into(),
        name: "oversized".repeat(128),
        oid: 16384,
        base: Box::new(ColumnType::Integer),
    };
    let error: SQLError = ty
        .retain_external_with_control(&control)
        .unwrap_err()
        .into();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), used);
    assert_eq!(&*held, "earlier result");
    drop(held);
    assert_eq!(budget.used(), 0);
}
