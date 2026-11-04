//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{EventEnableMode, Expr};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn trigger(sql: &str) -> StoredTrigger {
    let crate::Statement::CreateTrigger(definition) = crate::compile(sql).unwrap().remove(0) else {
        panic!("trigger")
    };
    StoredTrigger {
        definition,
        function_object_id: Some([1; 16]),
        enabled: EventEnableMode::Origin,
        object_id: Some([2; 16]),
        constraint_name: Some("Original.Constraint".into()),
        catalog_oid: Some(30001),
        constraint_catalog_oid: Some(30002),
    }
}

fn ordinary() -> StoredTrigger {
    trigger("CREATE TRIGGER saved BEFORE INSERT OR UPDATE OF id ON docs FOR EACH ROW WHEN ('value' = 'value') EXECUTE FUNCTION fire('original','')")
}

#[test]
fn durable_triggers_exclude_temporary_relations_before_admitting_names_and_bodies() {
    let mut source = TriggerCatalog::try_from_iter([
        (
            RelationIdentity::new("Quoted.Schema", "Table.Name"),
            crate::catalog::events::EventDefinitions::try_from_iter([("saved".into(), ordinary())])
                .unwrap(),
        ),
        (
            RelationIdentity::new("public", "pg_temp_local"),
            crate::catalog::events::EventDefinitions::new(),
        ),
    ])
    .unwrap();
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let original = durable_triggers(&source, &control).unwrap();
    let retained = memory.used();
    let mut temporary = ordinary();
    temporary.definition.arguments.push("x".repeat(1 << 20));
    source
        .insert(
            RelationIdentity::new("pg_temp_1", "t".repeat(1 << 20)),
            crate::catalog::events::EventDefinitions::try_from_iter([("huge".into(), temporary)])
                .unwrap(),
        )
        .unwrap();
    source
        .insert(
            RelationIdentity::new("pg_toast_temp_1", "toast"),
            crate::catalog::events::EventDefinitions::new(),
        )
        .unwrap();
    let saved = durable_triggers(&source, &control).unwrap();
    assert_eq!(memory.used(), retained * 2);
    assert_eq!(
        serde_json::to_value(&*saved).unwrap(),
        serde_json::to_value(&*original).unwrap()
    );
    assert_eq!(saved.len(), 2);
    assert!(triggers(&source, &control).is_err());
    drop((saved, original));
    assert_eq!(memory.used(), 0);
    let exact = MemoryBudget::new(retained);
    let saved = durable_triggers(&source, &ProductionControl::new(&exact, &token, &token)).unwrap();
    let denied = MemoryBudget::new(retained - 1);
    assert!(durable_triggers(&source, &ProductionControl::new(&denied, &token, &token)).is_err());
    assert_eq!(denied.used(), 0);
    drop(saved);
    assert_eq!(exact.used(), 0);
    token.cancel();
    assert!(durable_triggers(&source, &control).is_err());
}

#[test]
fn trigger_capture_preserves_complete_declarations_bound_identities_and_enable_modes() {
    let declarations = [
        ordinary(),
        trigger("CREATE CONSTRAINT TRIGGER constrained AFTER INSERT ON docs FROM referenced DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION fire()"),
        trigger("CREATE TRIGGER transition AFTER UPDATE ON docs REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION fire()"),
        trigger("CREATE TRIGGER instead INSTEAD OF DELETE ON docs FOR EACH ROW EXECUTE FUNCTION fire()"),
    ];
    let mut source = TriggerCatalog::new();
    source
        .insert(
            RelationIdentity::new("empty", "relation"),
            crate::catalog::events::EventDefinitions::new(),
        )
        .unwrap();
    let mut entries = crate::catalog::events::EventDefinitions::new();
    for (mut value, enabled) in declarations.into_iter().zip([
        EventEnableMode::Origin,
        EventEnableMode::Disabled,
        EventEnableMode::Replica,
        EventEnableMode::Always,
    ]) {
        value.enabled = enabled;
        if !value.definition.constraint {
            value.constraint_name = None;
            value.constraint_catalog_oid = None;
        }
        if matches!(enabled, EventEnableMode::Always) {
            value.function_object_id = None;
            value.object_id = None;
            value.catalog_oid = None;
        }
        entries
            .insert(value.definition.name.clone(), value)
            .unwrap();
    }
    source
        .insert(
            RelationIdentity::new("Quoted.Schema", "Table.Name"),
            entries,
        )
        .unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut saved = None;
    let allocated =
        allocation_counter::measure(|| saved = Some(triggers(&source, &control).unwrap()));
    let saved = saved.unwrap();
    assert_eq!(
        allocated.bytes_current,
        i64::try_from(memory.used()).unwrap()
    );
    assert_eq!(saved.len(), source.len());
    for ((relation, entries), (original_relation, original)) in saved.iter().zip(&source) {
        assert_eq!(relation, original_relation);
        assert_eq!(entries.len(), original.len());
        for ((name, value), (original_name, original)) in entries.iter().zip(original) {
            assert_eq!(name, original_name);
            assert_eq!(
                serde_json::to_value(value).unwrap(),
                serde_json::to_value(original).unwrap()
            );
        }
    }
    drop((source, saved));
    assert_eq!(memory.used(), 0);
}

#[test]
fn trigger_capture_detaches_retained_condition_constants_from_cancelled_source_owners() {
    let source_memory = MemoryBudget::new(1 << 20);
    let source_token = CancellationToken::new();
    let source_control = ProductionControl::new(&source_memory, &source_token, &source_token);
    let expected = Value::Str("original".repeat(8192));
    let value = crate::ir::RetainedConstantValue::from_produced(
        source_control.copy_value(&expected).unwrap(),
        &source_control,
    )
    .unwrap();
    let value = crate::ir::RetainedScalarLiteral::with_control(
        value,
        None,
        None,
        Some(950),
        &source_control,
    )
    .unwrap();
    let mut source = ordinary();
    source.definition.when = Some(Expr::IsNull {
        expr: Box::new(Expr::RetainedLiteral(Box::new(
            crate::ast::RetainedInputLiteral(value),
        ))),
        negated: true,
    });
    source_token.cancel();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let saved = source.capture_definition(&control).unwrap();
    let Some(Expr::IsNull {
        expr: saved_expr, ..
    }) = &saved.definition.when
    else {
        panic!("retained condition")
    };
    let Expr::RetainedLiteral(value) = saved_expr.as_ref() else {
        panic!("retained literal")
    };
    let Some(Expr::IsNull {
        expr: original_expr,
        ..
    }) = &source.definition.when
    else {
        panic!("original condition")
    };
    let Expr::RetainedLiteral(original) = original_expr.as_ref() else {
        panic!("original literal")
    };
    assert!(!value.0.value.shares_value_with(&original.0.value));
    drop(source);
    assert_eq!(source_memory.used(), 0);
    assert!(value
        .0
        .value
        .resident_value()
        .unwrap()
        .has_same_representation(&expected));
    assert_eq!(value.0.collation_oid, Some(950));
    drop(saved);
    assert_eq!(memory.used(), 0);
}

#[test]
fn trigger_capture_releases_prior_definitions_on_late_argument_denial_and_cancellation() {
    let mut large = ordinary();
    large.definition.arguments.push("x".repeat(32768));
    let source = TriggerCatalog::try_from_iter([(
        RelationIdentity::new("schema", "table"),
        crate::catalog::events::EventDefinitions::try_from_iter([
            ("first".into(), ordinary()),
            ("last".into(), large),
        ])
        .unwrap(),
    )])
    .unwrap();
    for cancelled in [false, true] {
        let memory = MemoryBudget::new(if cancelled { 1 << 20 } else { 4096 });
        let token = CancellationToken::new();
        if cancelled {
            token.cancel();
        }
        let control = ProductionControl::new(&memory, &token, &token);
        let error = triggers(&source, &control).unwrap_err();
        if cancelled {
            assert!(matches!(error, SQLError::Cancelled(_)));
        } else {
            assert_eq!(error.sqlstate(), Some("53200"));
        }
        assert_eq!(memory.used(), 0);
    }
}
