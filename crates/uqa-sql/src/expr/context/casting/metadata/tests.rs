//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::SQLError;
use std::cell::Cell;

#[derive(Default)]
struct Catalog {
    first: CatalogCastScope,
    second: CatalogCastScope,
    changed: Cell<bool>,
    change_in_callback: Cell<bool>,
    fail_constraint: Cell<bool>,
    names: Cell<usize>,
    callbacks: Cell<usize>,
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }
    fn catalog_cast_scope(&self) -> Option<&CatalogCastScope> {
        Some(if self.changed.get() {
            &self.second
        } else {
            &self.first
        })
    }
    fn resolve_type_name(&self, name: &str) -> std::result::Result<Option<ColumnType>, String> {
        self.names.set(self.names.get() + 1);
        Ok(Some(if name == "pg_catalog.int8" {
            ColumnType::BigInteger
        } else if self.changed.get() {
            ColumnType::SmallInteger
        } else {
            ColumnType::Integer
        }))
    }
    fn cast_domain(&self, _: &Value, _: Option<&str>, _: &ColumnType) -> Result<Option<Value>> {
        self.callbacks.set(self.callbacks.get() + 1);
        if self.change_in_callback.get() {
            self.changed.set(true);
        }
        if self.fail_constraint.get() {
            Err(SQLError::Routine {
                sqlstate: "23514".into(),
                message: "live constraint".into(),
            })
        } else {
            Ok(None)
        }
    }
}

fn resolve(catalog: &Catalog, value: i64, target: &str) -> ScopedCastResult {
    cast_value_with_scoped_resolution(
        &Value::Int(value),
        Some("pg_catalog.int8"),
        target,
        Some(catalog),
        &ProductionControl::uncontrolled(),
    )
    .unwrap()
}

#[test]
fn admitted_catalog_names_are_resolved_once_with_live_values_and_constraints() {
    let catalog = Catalog::default();
    let first = resolve(&catalog, 0, "public.selected");
    assert_eq!(*first.value, Value::Int(0));
    let retained = first.catalog.unwrap();
    drop(first.value);
    for value in 1..1_000 {
        let result = retained
            .evaluate(
                &Value::Int(value),
                Some("pg_catalog.int8"),
                "public.selected",
                Some(&catalog),
                &ProductionControl::uncontrolled(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(*result, Value::Int(value));
    }
    assert_eq!(catalog.names.get(), 2);
    assert_eq!(catalog.callbacks.get(), 1_000);
    catalog.fail_constraint.set(true);
    let result = retained
        .evaluate(
            &Value::Int(1),
            Some("pg_catalog.int8"),
            "public.selected",
            Some(&catalog),
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
    assert_eq!(result.unwrap_err().sqlstate(), Some("23514"));
    assert_eq!(catalog.names.get(), 2);
    assert_eq!(catalog.callbacks.get(), 1_001);
}

#[test]
fn changed_scope_declines_before_conversion_and_rebinds_the_new_type() {
    let catalog = Catalog::default();
    let retained = resolve(&catalog, 32_768, "public.selected")
        .catalog
        .unwrap();
    catalog.changed.set(true);
    assert!(retained
        .evaluate(
            &Value::Int(32_768),
            Some("pg_catalog.int8"),
            "public.selected",
            Some(&catalog),
            &ProductionControl::uncontrolled(),
        )
        .is_none());
    assert_eq!(catalog.callbacks.get(), 1);
    let result = cast_value_with_scoped_resolution(
        &Value::Int(32_768),
        Some("pg_catalog.int8"),
        "public.selected",
        Some(&catalog),
        &ProductionControl::uncontrolled(),
    );
    assert_eq!(result.err().unwrap().sqlstate(), Some("22003"));
    assert_eq!(catalog.names.get(), 4);
}

#[test]
fn callback_scope_changes_and_unqualified_names_do_not_publish_metadata() {
    let catalog = Catalog::default();
    catalog.change_in_callback.set(true);
    let result = resolve(&catalog, 32_768, "public.selected");
    assert_eq!(*result.value, Value::Int(32_768));
    assert!(result.catalog.is_none());
    let catalog = Catalog::default();
    for value in [1, 2] {
        assert!(resolve(&catalog, value, "selected").catalog.is_none());
    }
    assert_eq!(catalog.names.get(), 4);
    assert_eq!(catalog.callbacks.get(), 2);
}

#[test]
fn fixed_type_spelling_uses_the_sql_parser_and_bound_identity_contract() {
    for name in [
        "public.selected",
        "\"odd.schema\".\"Type Name\"[]",
        "pg_catalog.numeric(8,2)",
    ] {
        assert!(fixed_name(name), "{name}");
    }
    for name in [
        "selected",
        "\"public.selected\"",
        "public.selected; SELECT 1",
        "",
    ] {
        assert!(!fixed_name(name), "{name}");
    }
    assert!(!CatalogCastScope::default().matches(&CatalogCastScope::default()));
    let scope = CatalogCastScope::default();
    assert!(scope.matches(&scope.clone()));
}

#[test]
fn insufficient_metadata_allowance_preserves_successful_scalar_conversion() {
    let catalog = Catalog::default();
    let budget = uqa_core::memory::MemoryBudget::new(0);
    let token = uqa_core::CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let result = cast_value_with_scoped_resolution(
        &Value::Int(1),
        Some("pg_catalog.int8"),
        "public.selected",
        Some(&catalog),
        &control,
    )
    .unwrap();
    assert_eq!(*result.value, Value::Int(1));
    assert!(result.catalog.is_none());
    assert_eq!(catalog.callbacks.get(), 1);
    assert_eq!(budget.used(), 0);
}
