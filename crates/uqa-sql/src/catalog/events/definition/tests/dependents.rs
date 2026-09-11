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
        definition::rewrites::rewrite_trigger_routine_references,
        reads::EventLookupState,
        removal::{removed_dependent_rules, removed_relation_events},
        PreparedRuleColumnDrop, RuleCatalog, RuleColumnDependency, RuleDependencies,
        RuleRoutineDependency, StoredRule, TriggerCatalog,
    },
    SQLError,
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;

fn relation(name: &str) -> RelationIdentity {
    RelationIdentity::new("public", name)
}
fn stored_rule(name: &str) -> StoredRule {
    StoredRule {
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
fn relation_dependents_skip_owned_rules_and_sort_external_definitions() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let mut owned = stored_rule("owned");
    owned.dependencies = None;
    partitions.rules.insert(
        relation("parent"),
        BTreeMap::from([("owned".into(), owned)]),
    );
    let mut first = stored_rule("alpha");
    first
        .dependencies
        .as_mut()
        .unwrap()
        .relations
        .insert(relation("parent"));
    let mut second = first.clone();
    second.definition.name = "zeta".into();
    partitions.rules.insert(
        relation("child"),
        BTreeMap::from([
            ("zeta".into(), second),
            ("alpha".into(), first),
            ("independent".into(), stored_rule("independent")),
        ]),
    );
    assert_eq!(
        partitions
            .context(&catalog.context())
            .rules_depending_on_relations(&["public.parent".into()])
            .unwrap(),
        [
            (relation("child"), "alpha".into()),
            (relation("child"), "zeta".into())
        ]
    );
    assert_eq!(*partitions.reads.borrow(), ["rules"]);
}

#[test]
fn corrupt_rule_dependency_state_fails_instead_of_hiding_dependents() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let mut corrupt = stored_rule("corrupt");
    corrupt.dependencies = None;
    partitions.rules.insert(
        relation("child"),
        BTreeMap::from([("corrupt".into(), corrupt)]),
    );
    let lookup = partitions.context(&catalog.context());
    let expected = "rule `corrupt` on `public.child` has no bound dependency state";
    assert_eq!(
        lookup
            .rules_depending_on_relations(&["public.parent".into()])
            .unwrap_err(),
        expected
    );
    assert_eq!(
        lookup
            .rules_depending_on_routine(&binding(Some([1; 16]), "public.handler"))
            .unwrap_err(),
        expected
    );
    assert!(
        matches!(lookup.column_event_dependencies("public.child","id"),Err(SQLError::Internal(message)) if message==expected)
    );
}

#[test]
fn rule_routine_dependencies_use_object_identity_and_legacy_signature_pairs() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let mut entries = BTreeMap::new();
    for (name, id, routine_name, args) in [
        ("identity", Some([1; 16]), "public.old", vec![]),
        ("stale", Some([2; 16]), "public.handler", vec![]),
        ("legacy", None, "public.handler", vec![]),
        ("overload", None, "public.handler", vec!["integer".into()]),
    ] {
        let mut stored = stored_rule(name);
        stored
            .dependencies
            .as_mut()
            .unwrap()
            .routines
            .insert(RuleRoutineDependency {
                object_id: id,
                name: routine_name.into(),
                argument_types: args,
            });
        entries.insert(name.into(), stored);
    }
    partitions.rules.insert(relation("child"), entries);
    let lookup = partitions.context(&catalog.context());
    assert_eq!(
        lookup
            .rules_depending_on_routine(&binding(Some([1; 16]), "public.handler"))
            .unwrap(),
        [(relation("child"), "identity".into())]
    );
    assert_eq!(
        lookup
            .rules_depending_on_routine(&binding(None, "public.handler"))
            .unwrap(),
        [(relation("child"), "legacy".into())]
    );
}

struct PinnedTriggers(TriggerCatalog);
impl EventLookupState for PinnedTriggers {
    fn query_rules(&self) -> Option<&RuleCatalog> {
        panic!("trigger dependency discovery must not inspect rules")
    }
    fn query_triggers(&self) -> Option<&TriggerCatalog> {
        Some(&self.0)
    }
    fn session_replication_role_is_replica(&self) -> bool {
        panic!("dependency discovery must include disabled triggers")
    }
}
#[test]
fn trigger_routine_dependencies_include_pinned_when_bindings_and_disabled_definitions() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let mut invokes = trigger("public.child", "invokes");
    invokes.function_object_id = Some([1; 16]);
    invokes.enabled = EventEnableMode::Disabled;
    let mut condition = trigger("public.child", "condition");
    condition.definition.when = Some(call(binding(Some([1; 16]), "public.filter")));
    let mut stale = trigger("public.child", "stale");
    stale.function_object_id = Some([2; 16]);
    stale.definition.function = "public.handler".into();
    let pinned = PinnedTriggers(TriggerCatalog::from([(
        relation("child"),
        BTreeMap::from([
            ("invokes".into(), invokes),
            ("condition".into(), condition),
            ("stale".into(), stale),
        ]),
    )]));
    partitions.triggers.insert(
        relation("child"),
        BTreeMap::from([("live".into(), trigger("public.child", "live"))]),
    );
    let mut lookup = partitions.context(&catalog.context());
    lookup.state = &pinned;
    assert_eq!(
        lookup
            .triggers_depending_on_routine(&binding(Some([1; 16]), "public.handler"))
            .unwrap(),
        [
            ("public.child".into(), "condition".into()),
            ("public.child".into(), "invokes".into())
        ]
    );
    assert!(partitions.reads.borrow().is_empty());
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
fn missing_dependent_rule_fails_without_mutating_the_source_catalog() {
    let rules = RuleCatalog::from([(
        relation("child"),
        BTreeMap::from([("keep".into(), stored_rule("keep"))]),
    )]);
    let error = removed_dependent_rules(
        &rules,
        &[
            (relation("child"), "keep".into()),
            (relation("child"), "missing".into()),
        ],
    )
    .unwrap_err();
    assert_eq!(
        error,
        "dependent rule `missing` on `public.child` disappeared after DROP preflight"
    );
    assert!(rules[&relation("child")].contains_key("keep"));
    assert!(
        removed_dependent_rules(&rules, &[(relation("child"), "keep".into())])
            .unwrap()
            .is_empty()
    );
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
