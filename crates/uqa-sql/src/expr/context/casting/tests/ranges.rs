//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_core::DatumValue;

#[derive(Default)]
struct Inputs(Cell<usize>);
impl EngineHook for Inputs {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }
    fn read_range_input(
        &self,
        oid: u32,
        _: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        self.0.set(self.0.get() + 1);
        Ok(control.copy_value(&Value::Datum(DatumValue::new(oid, 1, vec![0xff])))?)
    }
}

fn range(oid: u32) -> ColumnType {
    ColumnType::DeclaredRange(crate::ast::RangeTypeReference {
        schema: "public".into(),
        name: if oid == 40000 { "r" } else { "other_r" }.into(),
        oid,
        array_oid: oid + 1,
        alignment: crate::ast::RangeAlignment::Integer,
    })
}

#[test]
fn declared_range_cast_selection_keeps_binary_identity_lazy_and_input_effects_separate() {
    let engine = Inputs::default();
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let target = range(40000);
    let cast = |value: &Value, source| {
        super::super::ranges::cast(&engine, value, source, &target, &control)
    };
    let input = cast(&Value::Str("[1,2]".into()), Some(&ColumnType::Text))
        .unwrap()
        .unwrap();
    assert_eq!(engine.0.get(), 1);
    // The stored address is deliberately unreadable. Identity coercion must not inspect it or repeat input.
    let identity = cast(&input, Some(&target)).unwrap().unwrap();
    assert_eq!(engine.0.get(), 1);
    assert!(
        matches!(&*identity, Value::Datum(datum) if datum.type_oid() == 40000 && datum.offset() == 1)
    );
    let other = range(41000);
    assert_eq!(
        cast(&input, Some(&other)).unwrap_err().sqlstate(),
        Some("42846")
    );
    let domain = ColumnType::Domain {
        schema: "public".into(),
        name: "wrapped_r".into(),
        oid: 42000,
        array_oid: None,
        base: Box::new(target.clone()),
        collation_oid: None,
    };
    let physical = Value::Datum(DatumValue::new(42000, 1, vec![0xff]));
    let unwrapped = cast(&physical, Some(&domain)).unwrap().unwrap();
    assert!(
        matches!(&*unwrapped, Value::Datum(datum) if datum.type_oid() == 40000 && datum.offset() == 1)
    );
    assert_eq!(engine.0.get(), 1);
    drop((input, identity, unwrapped));
    assert_eq!(memory.used(), 0);
    let reference: serde_json::Value = serde_json::from_str(include_str!("ranges.json")).unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let source = match case["source"].as_str() {
            None => None,
            Some("text") => Some(&ColumnType::Text),
            Some("integer") => Some(&ColumnType::Integer),
            Some("r") => Some(&target),
            Some("other_r") => Some(&other),
            Some("wrapped_r") => Some(&domain),
            _ => unreachable!("captured cast source"),
        };
        let result = cast(&Value::Null, source);
        if let Some(expected) = case.get("error") {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), expected["sqlstate"].as_str());
            assert_eq!(error.to_string(), expected["message"].as_str().unwrap());
        } else {
            assert_eq!(*result.unwrap().unwrap(), Value::Null);
        }
    }
    assert_eq!(engine.0.get(), 1);
    assert_eq!(memory.used(), 0);
}

#[test]
fn declared_range_constants_and_arrays_select_catalog_input_without_native_aliases() {
    let target = range(40000);
    assert!(requires_catalog_constant_input(&target));
    assert!(requires_catalog_constant_input(&ColumnType::Array(
        Box::new(target.clone())
    )));
    assert!(requires_catalog_array_cast(&target));
    let ColumnType::DeclaredRange(reference) = &target else {
        unreachable!()
    };
    assert!(requires_catalog_constant_input(
        &ColumnType::DeclaredMultirange(reference.clone())
    ));
    assert!(!requires_catalog_constant_input(&ColumnType::Range(
        crate::ast::RangeSubtype::Integer
    )));
    let engine = Inputs::default();
    let control = ProductionControl::uncontrolled();
    let result = super::super::ranges::cast(&engine, &Value::Null, None, &target, &control)
        .unwrap()
        .unwrap();
    assert_eq!(*result, Value::Null);
    assert_eq!(engine.0.get(), 0);
    let error = super::super::ranges::cast(
        &engine,
        &Value::Int(1),
        Some(&ColumnType::Integer),
        &target,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42846"));
    assert_eq!(error.to_string(), "cannot cast type integer to r");
}
