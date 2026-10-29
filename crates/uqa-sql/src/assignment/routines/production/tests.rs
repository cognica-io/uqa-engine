//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, RowValue};

struct Context;
impl crate::expr::EngineHook for Context {
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
impl crate::catalog::domain::DomainCatalog for Context {
    fn domain_by_oid(&self, _: u32) -> Option<crate::catalog::domain::StoredDomain> {
        None
    }
}
impl crate::assignment::AssignmentContext for Context {
    fn evaluate_domain_check(
        &self,
        _: &crate::ast::Expr,
        _: &dyn crate::expr::RowLookup,
        _: &crate::RowSchema,
    ) -> Result<Value, SQLError> {
        unreachable!()
    }
}
impl RoutineValueContext for Context {
    fn catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        ColumnType::from_sql_name(name).ok()
    }
    fn catalog_column_type_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        ColumnType::from_sql_name_with_control(name, control).map(Some)
    }
}

#[test]
fn routine_coercion_keeps_admitted_identity_values_and_produces_named_records() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let input = control
        .copy_value(&Value::Str("cursor_payload".repeat(1024)))
        .unwrap();
    let Value::Str(text) = &*input else {
        unreachable!()
    };
    let pointer = text.as_ptr();
    let original_bytes = memory.used();
    let output =
        coerce_routine_value_from_with_control(&Context, input, "refcursor", None, &control)
            .unwrap();
    let Value::Str(text) = &*output else {
        unreachable!()
    };
    assert_eq!(text.as_ptr(), pointer);
    assert_eq!(memory.used(), original_bytes);
    drop(output);
    assert_eq!(memory.used(), 0);

    let input = control
        .copy_value(&Value::Row(RowValue::new(vec![
            Value::Int(7),
            Value::Str("x".into()),
        ])))
        .unwrap();
    let output =
        coerce_routine_value_from_with_control(&Context, input, "record", None, &control).unwrap();
    assert_eq!(
        *output,
        Value::Record(
            vec![
                ("f1".into(), Value::Int(7)),
                ("f2".into(), Value::Str("x".into()))
            ]
            .into()
        )
    );
    assert!(memory.used() > 0);
    drop(output);
    assert_eq!(memory.used(), 0);
}

#[test]
fn routine_coercion_rejects_a_large_record_replacement_before_copying_its_fields() {
    let source = Value::Row(RowValue::new(vec![Value::Str("x".repeat(1 << 22))]));
    let memory = MemoryBudget::new((1 << 22) + 4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let input = control.copy_value(&source).unwrap();
    let measured = allocation_counter::measure(|| {
        assert_eq!(
            coerce_routine_value_from_with_control(&Context, input, "record", None, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(measured.bytes_total < 16 * 1024, "{measured:?}");
    assert_eq!(memory.used(), 0);
    for original_cancelled in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let input = control.copy_value(&source).unwrap();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            coerce_routine_value_from_with_control(&Context, input, "record", None, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
    }
}
