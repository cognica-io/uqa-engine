//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{Expr, RuleEvent, Statement},
    plan::UnifiedPlan,
    ScalarExpr,
};
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn rule() -> StoredRule {
    let Statement::CreateRule(definition)=crate::compile("CREATE RULE changed AS ON UPDATE TO docs WHERE new.id > old.id DO ALSO (INSERT INTO audit SELECT new.id; DELETE FROM audit WHERE id = old.id)").unwrap().remove(0) else {panic!("rule")};
    let binding =
        RuleConditionBinding::for_event(&["id".into(), "Removed.Name".into()], RuleEvent::Update);
    let UnifiedPlan::Query(query) =
        UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query")
    };
    let plan = ExpressionPlan {
        scalar: ScalarExpr::Row(vec![
            ScalarExpr::InternalColumn(binding.old_column("id").unwrap()),
            ScalarExpr::InternalColumn(binding.new_column("Removed.Name").unwrap()),
        ]),
        subqueries: vec![*query],
    };
    StoredRule {
        definition,
        enabled: EventEnableMode::Origin,
        condition_plan: Some(plan),
        condition_binding: Some(binding),
        dependencies: Some(RuleDependencies {
            relations: [RelationIdentity::new("Quoted.Schema", "Relation.Name")].into(),
            columns: [RuleColumnDependency {
                relation: RelationIdentity::new("public", "docs"),
                column: "id".into(),
            }]
            .into(),
            routines: [RuleRoutineDependency {
                object_id: Some([7; 16]),
                name: "Quoted.Routine".into(),
                argument_types: vec!["integer".into(), "text".into()],
            }]
            .into(),
        }),
        catalog_oid: Some(40001),
    }
}

#[test]
fn durable_rules_exclude_temporary_relations_before_admitting_names_and_bodies() {
    let mut source = RuleCatalog::try_from_iter([
        (
            RelationIdentity::new("Quoted.Schema", "Relation.Name"),
            crate::catalog::events::EventDefinitions::try_from_iter([("saved".into(), rule())])
                .unwrap(),
        ),
        (
            RelationIdentity::new("public", "pg_temp_local"),
            crate::catalog::events::EventDefinitions::new(),
        ),
    ])
    .unwrap();
    let memory = MemoryBudget::new(128 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let original = durable_rules(&source, &control).unwrap();
    let retained = memory.used();
    let mut temporary = rule();
    temporary.definition.name = "x".repeat(1 << 20);
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
    let saved = durable_rules(&source, &control).unwrap();
    assert_eq!(memory.used(), retained * 2);
    assert_eq!(saved.len(), 2);
    same(&saved[0].1[0].1, &source[&saved[0].0]["saved"]);
    assert_eq!(saved[1].0, RelationIdentity::new("public", "pg_temp_local"));
    assert!(rules(&source, &control).is_err());
    drop((saved, original));
    assert_eq!(memory.used(), 0);
    let exact = MemoryBudget::new(retained);
    let saved = durable_rules(&source, &ProductionControl::new(&exact, &token, &token)).unwrap();
    let denied = MemoryBudget::new(retained - 1);
    assert!(durable_rules(&source, &ProductionControl::new(&denied, &token, &token)).is_err());
    assert_eq!(denied.used(), 0);
    drop(saved);
    assert_eq!(exact.used(), 0);
    token.cancel();
    assert!(durable_rules(&source, &control).is_err());
}

fn same(value: &CapturedRule, old: &StoredRule) {
    assert_eq!(
        serde_json::to_value(&value.definition).unwrap(),
        serde_json::to_value(&old.definition).unwrap()
    );
    assert_eq!(value.enabled, old.enabled);
    assert_eq!(value.catalog_oid, old.catalog_oid);
    assert_eq!(
        serde_json::to_value(&value.condition_plan).unwrap(),
        serde_json::to_value(&old.condition_plan).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&value.condition_binding).unwrap(),
        serde_json::to_value(&old.condition_binding).unwrap()
    );
    match (&value.dependencies, &old.dependencies) {
        (Some(value), Some(old)) => {
            assert_eq!(
                value.relations,
                old.relations.iter().cloned().collect::<Vec<_>>()
            );
            assert_eq!(
                value.columns,
                old.columns.iter().cloned().collect::<Vec<_>>()
            );
            assert_eq!(
                value.routines,
                old.routines.iter().cloned().collect::<Vec<_>>()
            );
        }
        (None, None) => {}
        _ => panic!("dependency presence changed"),
    }
}

#[test]
fn rule_capture_preserves_actions_bindings_dependency_sets_and_enable_modes() {
    let mut entries = BTreeMap::new();
    for (index, mode) in [
        EventEnableMode::Origin,
        EventEnableMode::Disabled,
        EventEnableMode::Replica,
        EventEnableMode::Always,
    ]
    .into_iter()
    .enumerate()
    {
        let mut value = rule();
        value.enabled = mode;
        if index == 1 {
            value.dependencies = None;
            value.condition_binding = None;
            value.condition_plan = None;
            value.catalog_oid = None;
        }
        if index == 2 {
            value.dependencies = Some(RuleDependencies::default());
        }
        entries.insert(index.to_string(), value);
    }
    let source = RuleCatalog::try_from_iter([
        (
            RelationIdentity::new("s", "empty"),
            crate::catalog::events::EventDefinitions::new(),
        ),
        (
            RelationIdentity::new("s", "rules"),
            crate::catalog::events::EventDefinitions::try_from_iter(entries).unwrap(),
        ),
    ])
    .unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut saved = None;
    let allocation =
        allocation_counter::measure(|| saved = Some(rules(&source, &control).unwrap()));
    let saved = saved.unwrap();
    assert_eq!(
        allocation.bytes_current,
        i64::try_from(memory.used()).unwrap()
    );
    assert_eq!(saved.len(), source.len());
    for ((relation, entries), (old_relation, old)) in saved.iter().zip(&source) {
        assert_eq!(relation, old_relation);
        assert_eq!(entries.len(), old.len());
        for ((name, value), (old_name, old)) in entries.iter().zip(old.iter()) {
            assert_eq!(name, old_name);
            same(value, old);
        }
    }
    drop((saved, source));
    assert_eq!(memory.used(), 0);
}

#[test]
fn rule_capture_detaches_condition_payloads_without_reallocating_old_new_bindings() {
    let mut source = rule();
    let source_memory = MemoryBudget::new(1 << 20);
    let source_token = CancellationToken::new();
    let source_control = ProductionControl::new(&source_memory, &source_token, &source_token);
    let value = crate::ir::RetainedConstantValue::from_produced(
        source_control
            .copy_value(&Value::Str("x".repeat(65536)))
            .unwrap(),
        &source_control,
    )
    .unwrap();
    let literal = crate::ir::RetainedScalarLiteral::with_control(
        value,
        None,
        None,
        Some(950),
        &source_control,
    )
    .unwrap();
    source.definition.condition = Some(Expr::RetainedLiteral(Box::new(
        crate::ast::RetainedInputLiteral(literal.clone()),
    )));
    source.condition_plan.as_mut().unwrap().scalar = ScalarExpr::RetainedLiteral(literal);
    source_token.cancel();
    let memory = MemoryBudget::new(256 * 1024);
    let token = CancellationToken::new();
    let saved = source
        .capture_definition(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let binding = saved.condition_binding.as_ref().unwrap();
    let old = source.condition_binding.as_ref().unwrap();
    assert_eq!(binding.old_relation(), old.old_relation());
    assert_eq!(binding.new_relation(), old.new_relation());
    assert_eq!(
        binding.old_column("Removed.Name"),
        old.old_column("Removed.Name")
    );
    let ScalarExpr::RetainedLiteral(literal) = &saved.condition_plan.as_ref().unwrap().scalar
    else {
        panic!("literal")
    };
    drop(source);
    assert_eq!(source_memory.used(), 0);
    assert_eq!(literal.collation_oid, Some(950));
    assert!(literal
        .value
        .resident_value()
        .unwrap()
        .has_same_representation(&Value::Str("x".repeat(65536))));
    drop(saved);
    assert_eq!(memory.used(), 0);
}

#[test]
fn rule_capture_releases_prior_entries_when_late_dependencies_are_denied_or_cancelled() {
    let mut large = rule();
    large
        .dependencies
        .as_mut()
        .unwrap()
        .routines
        .insert(RuleRoutineDependency {
            object_id: None,
            name: "x".repeat(65536),
            argument_types: Vec::new(),
        });
    let source = RuleCatalog::try_from_iter([(
        RelationIdentity::new("s", "table"),
        crate::catalog::events::EventDefinitions::try_from_iter([
            ("a".into(), rule()),
            ("z".into(), large),
        ])
        .unwrap(),
    )])
    .unwrap();
    for cancelled in [false, true] {
        let memory = MemoryBudget::new(if cancelled { 1 << 20 } else { 16384 });
        let token = CancellationToken::new();
        if cancelled {
            token.cancel();
        }
        let error = rules(&source, &ProductionControl::new(&memory, &token, &token)).unwrap_err();
        if cancelled {
            assert!(matches!(error, SQLError::Cancelled(_)));
        } else {
            assert_eq!(error.sqlstate(), Some("53200"));
        }
        assert_eq!(memory.used(), 0);
    }
}
