//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    fixtures::Catalog,
    partitions::{fixture, trigger},
    rule,
};
use crate::{
    ast::{EventEnableMode, Expr, FunctionBinding},
    catalog::events::{
        definition::rewrites::rewrite_trigger_routine_references, removal::removed_relation_events,
        PreparedRuleColumnDrop, RuleCatalog, RuleColumnDependency, RuleDependencies, StoredRule,
        TriggerCatalog,
    },
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;

fn relation(name: &str) -> RelationIdentity {
    RelationIdentity::new("public", name)
}
fn stored_rule(name: &str) -> StoredRule {
    StoredRule {
        catalog_oid: None,
        definition: rule(&format!(
            "CREATE RULE {name} AS ON INSERT TO child DO NOTHING"
        )),
        enabled: EventEnableMode::Origin,
        condition_plan: None,
        condition_binding: None,
        dependencies: Some(RuleDependencies::default()),
    }
}
fn binding(id: Option<[u8; 16]>, name: &str) -> FunctionBinding {
    FunctionBinding {
        object_id: id,
        name: name.into(),
        argument_types: Vec::new(),
        builtin: false,
        dispatch: None,
        invocation: None,
        resolution_error: None,
    }
}
fn call(binding: FunctionBinding) -> Expr {
    Expr::Func {
        name: binding.name.clone(),
        binding: Some(binding),
        args: Vec::new(),
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}
fn column_dependency() -> RuleColumnDependency {
    RuleColumnDependency {
        relation: relation("child"),
        column: "id".into(),
    }
}

#[test]
fn column_dependencies_read_triggers_then_rules_and_include_when_references() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let mut update = trigger("public.child", "update_columns");
    update.definition.update_columns = vec!["id".into()];
    let mut condition = trigger("public.child", "when_column");
    condition.definition.when = Some(Expr::qualified_column("NEW", "id"));
    partitions.triggers.insert(
        relation("child"),
        BTreeMap::from([
            ("update_columns".into(), update),
            ("when_column".into(), condition),
            ("independent".into(), trigger("public.child", "independent")),
        ]),
    );
    let mut dependent = stored_rule("uses_child");
    dependent
        .dependencies
        .as_mut()
        .unwrap()
        .columns
        .insert(column_dependency());
    partitions.rules.insert(
        relation("parent"),
        BTreeMap::from([("uses_child".into(), dependent)]),
    );
    let selected = partitions
        .context(&catalog.context())
        .column_event_dependencies("public.child", "id")
        .unwrap();
    assert_eq!(selected.triggers, ["update_columns", "when_column"]);
    assert_eq!(selected.rules, [(relation("parent"), "uses_child".into())]);
    assert_eq!(*partitions.reads.borrow(), ["triggers", "rules"]);
}

#[test]
fn column_drop_rejects_bound_dependencies_before_rewriting_sources() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let mut dependent = stored_rule("uses_child");
    dependent
        .dependencies
        .as_mut()
        .unwrap()
        .columns
        .insert(column_dependency());
    partitions.rules.insert(
        relation("parent"),
        BTreeMap::from([("uses_child".into(), dependent)]),
    );
    let result = partitions
        .context(&catalog.context())
        .prepare_rule_column_drop("public.child", "id");
    assert!(
        matches!(result,Err(message) if message=="cannot drop column id of table public.child because rule uses_child on public.parent depends on it")
    );
    assert!(catalog.events.lock().unwrap().is_empty());
    assert!(partitions.rules[&relation("parent")]["uses_child"]
        .dependencies
        .as_ref()
        .unwrap()
        .columns
        .contains(&column_dependency()));
}

#[test]
fn removed_relation_candidates_include_referencing_constraints_and_preserve_originals() {
    let mut own = trigger("public.child", "own");
    own.definition.constraint = true;
    let mut referencing = trigger("public.parent", "references_child");
    referencing.definition.constraint = true;
    referencing.definition.referenced_table = Some("public.child".into());
    let triggers = TriggerCatalog::from([
        (relation("child"), BTreeMap::from([("own".into(), own)])),
        (
            relation("parent"),
            BTreeMap::from([
                ("references_child".into(), referencing),
                ("keep".into(), trigger("public.parent", "keep")),
            ]),
        ),
    ]);
    let rules = RuleCatalog::from([(
        relation("child"),
        BTreeMap::from([("own_rule".into(), stored_rule("own_rule"))]),
    )]);
    let removed = removed_relation_events(&triggers, &rules, &relation("child"))
        .unwrap()
        .unwrap();
    assert!(removed.rules.is_empty());
    assert!(!removed.triggers.contains_key(&relation("child")));
    assert_eq!(
        removed.triggers[&relation("parent")]
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["keep"]
    );
    assert_eq!(
        removed
            .constraints
            .iter()
            .map(|identity| identity.name.as_str())
            .collect::<Vec<_>>(),
        ["own", "references_child"]
    );
    assert!(removed
        .constraints
        .iter()
        .all(|identity| identity.object_id == Some([5; 16])));
    assert_eq!(triggers.len(), 2);
    assert_eq!(triggers[&relation("parent")].len(), 2);
    assert_eq!(rules.len(), 1);
}

#[test]
fn trigger_routine_rewrites_preserve_object_ids_and_bound_when_identity() {
    let mut invokes = trigger("public.child", "invokes");
    invokes.function_object_id = Some([1; 16]);
    let mut condition = trigger("public.child", "condition");
    condition.definition.when = Some(call(binding(Some([1; 16]), "public.old")));
    let mut stale = trigger("public.child", "stale");
    stale.function_object_id = Some([2; 16]);
    stale.definition.function = "public.old".into();
    let mut triggers = TriggerCatalog::from([(
        relation("child"),
        BTreeMap::from([
            ("invokes".into(), invokes),
            ("condition".into(), condition),
            ("stale".into(), stale),
        ]),
    )]);
    assert!(rewrite_trigger_routine_references(
        &mut triggers,
        &binding(Some([1; 16]), "public.old"),
        "public.renamed"
    )
    .unwrap());
    let entries = &triggers[&relation("child")];
    assert_eq!(entries["invokes"].definition.function, "public.renamed");
    assert_eq!(entries["invokes"].function_object_id, Some([1; 16]));
    assert_eq!(entries["invokes"].object_id, Some([5; 16]));
    assert_eq!(entries["stale"].definition.function, "public.old");
    let Some(Expr::Func {
        name,
        binding: Some(bound),
        ..
    }) = &entries["condition"].definition.when
    else {
        panic!("expected bound WHEN call")
    };
    assert_eq!(name, "public.renamed");
    assert_eq!(bound.name, "public.renamed");
    assert_eq!(bound.object_id, Some([1; 16]));
}

#[test]
fn event_column_candidates_rewrite_update_and_when_columns_without_changing_originals() {
    let catalog = Catalog::default();
    let mut stored = trigger("public.child", "affected");
    stored.definition.update_columns = vec!["id".into(), "other".into()];
    stored.definition.when = Some(Expr::qualified_column("NEW", "id"));
    let triggers = TriggerCatalog::from([(
        relation("child"),
        BTreeMap::from([("affected".into(), stored)]),
    )]);
    let (renamed, rules) = catalog
        .context()
        .renamed_event_column(
            &triggers,
            &RuleCatalog::new(),
            &relation("child"),
            "id",
            "renamed_id",
        )
        .unwrap()
        .unwrap();
    assert!(rules.is_empty());
    let renamed = &renamed[&relation("child")]["affected"];
    assert_eq!(renamed.definition.update_columns, ["renamed_id", "other"]);
    assert!(
        matches!(&renamed.definition.when,Some(Expr::QualifiedColumn {qualifier,column}) if qualifier=="NEW" && column=="renamed_id")
    );
    assert_eq!(renamed.object_id, Some([5; 16]));
    assert_eq!(
        triggers[&relation("child")]["affected"]
            .definition
            .update_columns,
        ["id", "other"]
    );
}

#[test]
fn invalid_trigger_expression_aborts_column_candidate_before_rule_rebinding() {
    let catalog = Catalog::default();
    let mut stored = trigger("public.child", "corrupt");
    stored.definition.when = Some(Expr::Star);
    let triggers = TriggerCatalog::from([(
        relation("child"),
        BTreeMap::from([("corrupt".into(), stored)]),
    )]);
    let result = catalog.context().renamed_event_column(
        &triggers,
        &RuleCatalog::new(),
        &relation("child"),
        "id",
        "next_id",
    );
    assert!(
        matches!(result,Err(message) if message=="schema expression contains `*` and cannot be rewritten safely")
    );
    assert!(matches!(
        triggers[&relation("child")]["corrupt"].definition.when,
        Some(Expr::Star)
    ));
    assert!(catalog.events.lock().unwrap().is_empty());
}

#[test]
fn missing_rule_during_column_rebind_fails_before_catalog_reads() {
    let catalog = Catalog::default();
    let mut prepared = PreparedRuleColumnDrop {
        rules: RuleCatalog::new(),
        rebind: BTreeSet::from([(relation("child"), "missing".into())]),
    };
    assert_eq!(
        catalog
            .context()
            .rebind_rule_column_drop(&mut prepared)
            .unwrap_err(),
        "rule `missing` on `public.child` disappeared while dropping a column"
    );
    assert!(catalog.events.lock().unwrap().is_empty());
}
