//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{ColumnDef, Statement};
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Types;

impl RoutineTypeCatalog for Types {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        panic!("declaration validation must not prepare a table")
    }
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        if name == "declaration_ints" {
            Some(ColumnType::Domain {
                schema: "public".into(),
                name: "declaration_ints".into(),
                oid: 20000,
                array_oid: Some(20001),
                collation_oid: None,
                base: Box::new(ColumnType::Array(Box::new(ColumnType::Integer))),
            })
        } else if name == "array_alias" {
            Some(ColumnType::Array(Box::new(ColumnType::Integer)))
        } else {
            ColumnType::from_sql_name(name).ok()
        }
    }
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        ColumnType::from_sql_name(name)
    }
    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        None
    }
    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        panic!("captured declarations must not repeat creation privileges")
    }
    fn format_type(&self, _: &ColumnType) -> Result<String, SQLError> {
        panic!("captured declarations must not execute formatting callbacks")
    }
}

fn declaration(sql: &str) -> CreateFunction {
    let Statement::CreateFunction(definition) = crate::compile(sql).unwrap().remove(0) else {
        panic!("routine declaration")
    };
    *definition
}

fn outcome(value: Result<(), SQLError>) -> serde_json::Value {
    match value {
        Ok(()) => serde_json::json!({"sqlstate": null, "message": null}),
        Err(error) => {
            serde_json::json!({"sqlstate": error.sqlstate(), "message": error.to_string()})
        }
    }
}

#[test]
fn captured_declarations_preserve_postgresql_language_modes_defaults_and_polymorphism() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("tests/pg18.json")).unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 66);
    let mut checked = 0;
    for case in cases.iter().filter(|case| case["mode"] == "off") {
        let definition = declaration(case["sql"].as_str().unwrap());
        let original = serde_json::to_value(&definition).unwrap();
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let actual = outcome(validate_captured_declaration(
            &definition,
            &Types,
            &ProductionControl::new(&memory, &token, &token),
        ));
        let expected =
            serde_json::json!({"sqlstate": case["sqlstate"], "message": case["message"]});
        assert_eq!(actual, expected, "{}", case["id"]);
        assert_eq!(serde_json::to_value(&definition).unwrap(), original);
        assert_eq!(memory.used(), 0);
        checked += 1;
    }
    assert_eq!(checked, 33);
}

#[test]
fn captured_declarations_resolve_variadic_array_aliases_without_compiling_bodies() {
    let definition = declaration("CREATE FUNCTION f(VARIADIC n array_alias) RETURNS integer LANGUAGE sql AS 'uncompiled missing($999)'");
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    validate_captured_declaration(
        &definition,
        &Types,
        &ProductionControl::new(&memory, &token, &token),
    )
    .unwrap();
    assert_eq!(memory.used(), 0);
}

#[test]
fn captured_declarations_release_denied_and_cancelled_type_scratch() {
    let definition = declaration("CREATE FUNCTION f(VARIADIC n array_alias) RETURNS integer LANGUAGE sql AS 'uncompiled source'");
    let memory = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    assert!(validate_captured_declaration(
        &definition,
        &Types,
        &ProductionControl::new(&memory, &original, &invoking)
    )
    .is_err());
    assert_eq!(memory.used(), 0);
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(validate_captured_declaration(
            &definition,
            &Types,
            &ProductionControl::new(&memory, &original, &invoking)
        )
        .is_err());
        assert_eq!(memory.used(), 0);
    }
}
