//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{EventEnableMode, Expr, Statement};
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryBudget, CancellationToken, RelationName, Value};

fn rule(name: &str, event: RuleEvent, enabled: EventEnableMode) -> StoredRule {
    let Statement::CreateRule(mut definition) = crate::compile(&format!(
        "CREATE RULE {name} AS ON INSERT TO items DO ALSO SELECT 'payload'"
    ))
    .unwrap()
    .remove(0) else {
        panic!("rule")
    };
    definition.event = event;
    StoredRule {
        definition,
        enabled,
        condition_plan: None,
        condition_binding: None,
        dependencies: None,
        catalog_oid: None,
    }
}
fn identity(control: &ProductionControl<'_>) -> Produced<RelationIdentity> {
    RelationIdentity::borrowed_reference("public.items")
        .unwrap()
        .identity_with_control(RelationName::decoded("public"), control)
        .unwrap()
}

#[test]
fn captured_rules_preserve_event_enablement_order_and_declaration_addresses() {
    let mut entries = BTreeMap::new();
    for (name, event, mode) in [
        ("a_origin", RuleEvent::Insert, EventEnableMode::Origin),
        ("b_replica", RuleEvent::Insert, EventEnableMode::Replica),
        ("c_always", RuleEvent::Insert, EventEnableMode::Always),
        ("d_disabled", RuleEvent::Insert, EventEnableMode::Disabled),
        ("e_update", RuleEvent::Update, EventEnableMode::Always),
    ] {
        entries.insert(name.into(), rule(name, event, mode));
    }
    let catalog = Arc::new(BTreeMap::from([(
        RelationIdentity::new("public", "items"),
        Arc::new(entries),
    )]));
    let memory = MemoryBudget::new(1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for event in [RuleEvent::Insert, RuleEvent::Update, RuleEvent::Delete] {
        for replica in [false, true] {
            let expected = crate::catalog::events::selection::active_rules(
                &catalog,
                &identity(&control),
                event,
                replica,
            );
            let selected = CapturedRules::from_catalog(
                catalog.clone(),
                identity(&control),
                event,
                replica,
                &control,
            )
            .unwrap();
            let actual = selected.iter().collect::<Result<Vec<_>, _>>().unwrap();
            assert_eq!(
                actual
                    .iter()
                    .map(|rule| &rule.definition.name)
                    .collect::<Vec<_>>(),
                expected
                    .iter()
                    .map(|rule| &rule.definition.name)
                    .collect::<Vec<_>>()
            );
            for rule in &actual {
                assert!(std::ptr::eq(
                    *rule,
                    &raw const catalog[&RelationIdentity::new("public", "items")]
                        [&rule.definition.name]
                ));
            }
            assert_eq!(selected.is_empty().unwrap(), actual.is_empty());
            let reversed = selected
                .iter()
                .rev()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert!(actual
                .iter()
                .rev()
                .zip(reversed)
                .all(|(a, b)| std::ptr::eq(*a, b)));
        }
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn capture_borrows_large_actions_and_keeps_the_original_generation_until_release() {
    let mut stored = rule("a", RuleEvent::Insert, EventEnableMode::Origin);
    let Statement::Select(query) = &mut stored.definition.actions[0] else {
        panic!("query")
    };
    query.projections[0].expr = Expr::Literal(Value::Str("x".repeat(4 * 1024 * 1024)));
    let mut catalog = Arc::new(BTreeMap::from([(
        RelationIdentity::new("public", "items"),
        Arc::new(BTreeMap::from([("a".into(), stored)])),
    )]));
    let old = allocation_counter::measure(|| {
        drop(crate::catalog::events::selection::active_rules(
            &catalog,
            &RelationIdentity::new("public", "items"),
            RuleEvent::Insert,
            false,
        ));
    });
    assert!(old.bytes_total >= 4 * 1024 * 1024);
    let mut next_generation = None;
    let copying = allocation_counter::measure(|| {
        next_generation = Some(catalog.as_ref().clone());
    });
    assert!(copying.bytes_total < 2048, "{copying:?}");
    let next_generation = next_generation.unwrap();
    let relation = RelationIdentity::new("public", "items");
    assert!(Arc::ptr_eq(
        &catalog[&relation],
        &next_generation[&relation]
    ));
    drop(next_generation);
    let original = Arc::downgrade(&catalog);
    let memory = MemoryBudget::new(256);
    let sibling = memory.reserve(37).unwrap();
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut selected = None;
    let measured = allocation_counter::measure(|| {
        selected = Some(
            CapturedRules::from_catalog(
                catalog.clone(),
                identity(&control),
                RuleEvent::Insert,
                false,
                &control,
            )
            .unwrap(),
        );
    });
    assert!(measured.bytes_total < 256, "{measured:?}");
    assert_eq!(
        usize::try_from(measured.bytes_current).unwrap(),
        memory.used() - sibling.bytes()
    );
    let selected = selected.unwrap();
    Arc::make_mut(&mut catalog).clear();
    assert!(catalog.is_empty());
    assert_eq!(
        selected.iter().next().unwrap().unwrap().definition.name,
        "a"
    );
    assert!(original.upgrade().is_some());
    drop(selected);
    assert!(original.upgrade().is_none());
    assert_eq!(memory.used(), sibling.bytes());
    drop(sibling);
    assert_eq!(memory.used(), 0);
}

#[test]
fn captured_iteration_observes_both_cancellations_even_for_empty_selections() {
    let memory = MemoryBudget::new(1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let catalog = Arc::new(BTreeMap::from([(
        RelationIdentity::new("public", "items"),
        Arc::new(BTreeMap::from([
            (
                "a".into(),
                rule("a", RuleEvent::Insert, EventEnableMode::Origin),
            ),
            (
                "b".into(),
                rule("b", RuleEvent::Insert, EventEnableMode::Origin),
            ),
        ])),
    )]));
    for token in [&original, &invoking] {
        for event in [RuleEvent::Insert, RuleEvent::Delete] {
            let selected = CapturedRules::from_catalog(
                catalog.clone(),
                identity(&control),
                event,
                false,
                &control,
            )
            .unwrap();
            let mut rules = selected.iter();
            if event == RuleEvent::Insert {
                assert_eq!(rules.next().unwrap().unwrap().definition.name, "a");
            }
            token.cancel();
            assert_eq!(
                rules.next_back().unwrap().unwrap_err().sqlstate(),
                Some("57014")
            );
            assert!(rules.next().is_none());
            assert_eq!(selected.is_empty().unwrap_err().sqlstate(), Some("57014"));
            assert_eq!(
                selected.any(|_| true).unwrap_err().sqlstate(),
                Some("57014")
            );
            token.reset();
            assert_eq!(
                selected.iter().count(),
                if event == RuleEvent::Insert { 2 } else { 0 }
            );
        }
        assert_eq!(memory.used(), 0);
    }
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _selected = CapturedRules::from_catalog(
            catalog.clone(),
            identity(&control),
            RuleEvent::Insert,
            false,
            &control,
        )
        .unwrap();
        panic!("selection consumer unwind");
    }));
    assert!(unwind.is_err());
    assert_eq!(memory.used(), 0);
}
