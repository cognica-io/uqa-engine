//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::FunctionBinding;
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Builtins<'a> {
    cancel: Option<&'a CancellationToken>,
}

impl FunctionTypeResolver for Builtins<'_> {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        unreachable!()
    }

    fn resolve_type_name(&self, _: &str) -> Result<Option<ColumnType>, SQLError> {
        if let Some(token) = self.cancel {
            token.cancel();
        }
        Ok(None)
    }

    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        panic!("value input does not declare a catalog object")
    }
}

impl EngineHook for Builtins<'_> {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
}

#[test]
fn typed_null_and_text_input_preserve_type_modifiers_and_array_bounds() {
    let catalog = Builtins { cancel: None };
    let control = ProductionControl::uncontrolled();
    // Independently captured PostgreSQL 18 cases are also exercised by the Engine owner.
    for (name, text, expected) in [
        ("smallint", "32767", "32767"),
        ("numeric(5,2)", "12.3456", "12.35"),
        ("numeric(2,-3)", "12345", "12000"),
        ("character(4)", "ab", "ab  "),
        ("varchar(2)", "abcd", "ab"),
        ("integer[]", "[0:1]={7,NULL}", "[0:1]={7,NULL}"),
    ] {
        let value =
            create_typed_parameter_with_control(name, Some(text), &catalog, &catalog, &control)
                .unwrap();
        let ty = value.declared_scalar_type().unwrap();
        assert_eq!(*ty, ColumnType::from_sql_name(name).unwrap());
        assert_eq!(
            crate::result::format_postgres_text(value.scalar_value().unwrap(), ty, None,).unwrap(),
            expected,
            "{name}"
        );
        let null =
            create_typed_parameter_with_control(name, None, &catalog, &catalog, &control).unwrap();
        assert_eq!(null.scalar_value(), Some(&Value::Null));
        assert_eq!(null.declared_scalar_type(), Some(ty));
    }
}

#[test]
fn input_retains_the_original_allowance_and_releases_partial_results() {
    let catalog = Builtins { cancel: None };
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = control.copy_text("held").unwrap();
    let value = create_typed_parameter_with_control(
        "text",
        Some("retained value"),
        &catalog,
        &catalog,
        &control,
    )
    .unwrap();
    assert!(value.reserved_bytes() >= "retained value".len());
    assert_eq!(
        budget.used(),
        held.reserved_bytes() + value.reserved_bytes()
    );
    drop(value);
    for (name, text, state) in [
        ("text", "x".repeat(8192), "53200"),
        ("smallint", "32768".into(), "22003"),
        ("numeric", "1.2.3".into(), "22P02"),
    ] {
        let error =
            create_typed_parameter_with_control(name, Some(&text), &catalog, &catalog, &control)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some(state));
        assert_eq!(budget.used(), held.reserved_bytes());
    }
}

#[test]
fn both_cancellation_tokens_remain_active_during_type_resolution() {
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = control.copy_text("held").unwrap();
    for token in [&original, &invoking] {
        for before in [false, true] {
            if before {
                token.cancel();
            }
            let catalog = Builtins {
                cancel: Some(token),
            };
            let error = create_typed_parameter_with_control(
                "text",
                Some("input"),
                &catalog,
                &catalog,
                &control,
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(budget.used(), held.reserved_bytes());
            token.reset();
        }
    }
}
