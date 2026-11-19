//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{EventEnableMode, RuleEvent, Statement},
    catalog::events::{RuleConditionBinding, RuleDependencies},
    plan::ExpressionPlan,
    ScalarExpr,
};

fn trigger() -> StoredTrigger {
    let Statement::CreateTrigger(definition) = crate::compile("CREATE TRIGGER watch BEFORE INSERT ON public.items FOR EACH ROW WHEN (true) EXECUTE FUNCTION fire('original')").unwrap().remove(0) else { panic!("trigger") };
    StoredTrigger {
        definition,
        function_object_id: Some([1; 16]),
        enabled: EventEnableMode::Always,
        object_id: Some([2; 16]),
        constraint_name: Some("original".into()),
        catalog_oid: Some(30_001),
        constraint_catalog_oid: Some(30_002),
    }
}
fn rule() -> StoredRule {
    let Statement::CreateRule(definition) = crate::compile("CREATE RULE watch AS ON UPDATE TO public.items WHERE NEW.id > OLD.id DO ALSO INSERT INTO public.audit SELECT NEW.id").unwrap().remove(0) else { panic!("rule") };
    let binding =
        RuleConditionBinding::for_event(&["id".into(), "Removed.Name".into()], RuleEvent::Update);
    StoredRule {
        definition,
        enabled: EventEnableMode::Replica,
        condition_plan: Some(ExpressionPlan {
            scalar: ScalarExpr::InternalColumn(binding.old_column("Removed.Name").unwrap()),
            subqueries: Vec::new(),
        }),
        condition_binding: Some(binding),
        dependencies: Some(RuleDependencies {
            relations: [RelationIdentity::new("public", "audit")].into(),
            ..RuleDependencies::default()
        }),
        catalog_oid: Some(40_001),
    }
}
fn capture_is_constant<T: EventDefinition + serde::Serialize>(value: T) {
    for count in [1, 64, 4096] {
        let memory = MemoryBudget::new(64 << 20);
        let mut source = EventCatalog::with_budget(&memory);
        for index in 0..count {
            source
                .insert_event(
                    RelationIdentity::new("public", format!("items_{index:04}")),
                    "watch".into(),
                    value.clone(),
                )
                .unwrap();
        }
        let temporary = RelationIdentity::new("pg_temp_7", "private");
        source
            .insert_event(temporary.clone(), "temporary".into(), value.clone())
            .unwrap();
        let token = CancellationToken::new();
        let first = MemoryBudget::new(64 << 20);
        let control = ProductionControl::new(&first, &token, &token);
        let mut saved = None;
        let allocated =
            allocation_counter::measure(|| saved = Some(source.capture(&control).unwrap()));
        assert_eq!(allocated.count_total, 0);
        let saved = saved.unwrap();
        assert_eq!(saved.len(), count);
        assert!(saved.get(&temporary).is_none());
        let retained = first.used();
        let exact = MemoryBudget::new(retained);
        let independent = source
            .capture(&ProductionControl::new(&exact, &token, &token))
            .unwrap();
        let denied = MemoryBudget::new(retained - 1);
        assert_eq!(
            source
                .capture(&ProductionControl::new(&denied, &token, &token))
                .err()
                .unwrap()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(denied.used(), 0);
        let relation = RelationIdentity::new("public", "items_0000");
        assert!(std::ptr::eq(
            &raw const source[&relation]["watch"],
            &raw const saved.get(&relation).unwrap()["watch"]
        ));
        source.remove(&relation).unwrap();
        source.clear();
        drop(source);
        assert_eq!(
            serde_json::to_value(&saved.get(&relation).unwrap()["watch"]).unwrap(),
            serde_json::to_value(&value).unwrap()
        );
        drop(saved);
        assert_eq!(first.used(), 0);
        assert!(memory.used() > 0);
        drop(independent);
        assert_eq!(exact.used(), 0);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_trigger_capture_shares_durable_roots_with_independent_exact_admission() {
    capture_is_constant(trigger());
}

#[test]
fn native_rule_capture_shares_original_bound_roots_with_independent_exact_admission() {
    capture_is_constant(rule());
}

#[test]
fn native_event_copy_funds_every_owned_dependency_node_without_rebinding() {
    let source = rule();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut copied = None;
    let allocation =
        allocation_counter::measure(|| copied = Some(source.copy_event(&control).unwrap()));
    let copied = copied.unwrap();
    assert_eq!(
        allocation.bytes_current,
        i64::try_from(memory.used()).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&*copied).unwrap(),
        serde_json::to_value(&source).unwrap()
    );
    let before = source.condition_binding.as_ref().unwrap();
    let after = copied.condition_binding.as_ref().unwrap();
    assert_eq!(before.old_relation(), after.old_relation());
    assert_eq!(before.new_relation(), after.new_relation());
    assert_eq!(
        before.old_column("Removed.Name"),
        after.old_column("Removed.Name")
    );
    drop(copied);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_event_publication_keeps_unrelated_large_declarations_shared() {
    let memory = MemoryBudget::new(16 << 20);
    let relation = RelationIdentity::new("public", "items");
    let mut source = EventCatalog::with_budget(&memory);
    let mut large = trigger();
    large.definition.arguments.push("x".repeat(4 << 20));
    source
        .insert_event(relation.clone(), "large".into(), large)
        .unwrap();
    source
        .insert_event(relation.clone(), "selected".into(), trigger())
        .unwrap();
    let original = source.clone();
    let allocation = allocation_counter::measure(|| {
        source
            .edit(&relation, |entries| {
                entries.edit("selected", |value| {
                    value.enabled = EventEnableMode::Disabled;
                    Ok(())
                })
            })
            .unwrap()
            .unwrap()
            .unwrap();
    });
    assert!(allocation.bytes_total < 32 << 10, "{allocation:?}");
    assert!(allocation.count_total < 256, "{allocation:?}");
    assert!(std::ptr::eq(
        &raw const source[&relation]["large"],
        &raw const original[&relation]["large"]
    ));
    assert_eq!(
        source[&relation]["selected"].enabled,
        EventEnableMode::Disabled
    );
    assert_eq!(
        original[&relation]["selected"].enabled,
        EventEnableMode::Always
    );
    drop((source, original));
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_event_updates_release_failed_candidates_and_preserve_complete_originals() {
    let memory = MemoryBudget::new(1 << 20);
    let mut source = EventCatalog::with_budget(&memory);
    let a = RelationIdentity::new("public", "a");
    let b = RelationIdentity::new("public", "b");
    let c = RelationIdentity::new("public", "c");
    for relation in [&a, &b, &c] {
        source
            .insert_event(relation.clone(), "watch".into(), trigger())
            .unwrap();
    }
    let original = source.clone();
    let baseline = memory.used();
    let error = source
        .rewrite(|relation, _, value| {
            if relation == &b {
                return Err(SQLError::Internal("late rewrite failure".into()));
            }
            let mut value = value.clone();
            value.enabled = EventEnableMode::Disabled;
            Ok(Some(value))
        })
        .unwrap_err();
    assert!(matches!(error,SQLError::Internal(ref message) if message=="late rewrite failure"));
    assert_eq!(memory.used(), baseline);
    let occupied = memory.reserve(memory.available()).unwrap();
    assert_eq!(
        source
            .insert_event(a.clone(), "watch".into(), trigger())
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert!(source.remove(&a).is_err());
    for relation in [&a, &b, &c] {
        assert!(std::ptr::eq(
            &raw const source[relation]["watch"],
            &raw const original[relation]["watch"]
        ));
    }
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    drop((source, original));
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_event_transfer_and_cancelled_capture_leave_allowances_unchanged() {
    let source_memory = MemoryBudget::new(1 << 20);
    let mut entries = EventDefinitions::with_budget(&source_memory);
    entries.insert("watch".into(), rule()).unwrap();
    let destination = MemoryBudget::new(1 << 20);
    let relation = RelationIdentity::new("public", "items");
    let mut source = EventCatalog::with_budget(&destination);
    source.insert(relation.clone(), entries.clone()).unwrap();
    assert!(!std::ptr::eq(
        &raw const source[&relation]["watch"],
        &raw const entries["watch"]
    ));
    drop(entries);
    assert_eq!(source_memory.used(), 0);
    let allowance = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    for cancelled in [&original, &invoking] {
        let token = CancellationToken::new();
        let control = if std::ptr::eq(cancelled, &raw const original) {
            ProductionControl::new(&allowance, cancelled, &token)
        } else {
            ProductionControl::new(&allowance, &token, cancelled)
        };
        cancelled.cancel();
        assert!(matches!(
            source.capture(&control),
            Err(SQLError::Cancelled(_))
        ));
        assert_eq!(allowance.used(), 0);
    }
    let denied = MemoryBudget::new(1024);
    let mut target = EventCatalog::with_budget(&denied);
    assert!(target
        .insert(relation.clone(), source[&relation].clone())
        .is_err());
    assert!(target.is_empty());
    assert_eq!(denied.used(), 0);
    drop((target, source));
    assert_eq!(destination.used(), 0);
}

#[test]
fn native_event_encodings_preserve_definition_maps_and_ordered_trigger_capture_pairs() {
    let relation = RelationIdentity::new("Quoted.Schema", "Table.Name");
    let values = std::collections::BTreeMap::from([
        ("z".to_owned(), trigger()),
        ("a".to_owned(), trigger()),
    ]);
    let definitions = EventDefinitions::try_from_iter(values.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&definitions).unwrap(),
        serde_json::to_value(&values).unwrap()
    );
    let mut source = EventCatalog::try_from_iter([
        (relation, definitions),
        (
            RelationIdentity::new("public", "empty"),
            EventDefinitions::new(),
        ),
    ])
    .unwrap();
    source
        .insert_event(
            RelationIdentity::new("pg_temp_8", "private"),
            "temporary".into(),
            trigger(),
        )
        .unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let native = source.capture(&control).unwrap();
    let legacy =
        crate::catalog::definition_capture::events::durable_triggers(&source, &control).unwrap();
    assert_eq!(
        serde_json::to_value(&*native).unwrap(),
        serde_json::to_value(&*legacy).unwrap()
    );
    let values =
        std::collections::BTreeMap::from([("z".to_owned(), rule()), ("a".to_owned(), rule())]);
    let definitions = EventDefinitions::try_from_iter(values.clone()).unwrap();
    let encoded = serde_json::to_value(&definitions).unwrap();
    assert_eq!(encoded, serde_json::to_value(&values).unwrap());
    let decoded: EventDefinitions<StoredRule> = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), encoded);
    drop((native, legacy));
    assert_eq!(memory.used(), 0);
}
