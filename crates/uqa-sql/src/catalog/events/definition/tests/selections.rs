//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    fixtures::Catalog,
    partitions::{fixture, trigger, Partitions},
    rule,
};
use crate::{
    ast::{EventEnableMode, RuleEvent, TriggerEvent, TriggerTiming},
    catalog::events::{
        definition::lookup::EventLookupContext, reads::EventLookupState, RuleCatalog, StoredRule,
        StoredTrigger, TriggerCatalog,
    },
};
use std::{cell::RefCell, collections::BTreeMap};
use uqa_core::RelationIdentity;

struct Visibility<'a> {
    rules: Option<&'a RuleCatalog>,
    triggers: Option<&'a TriggerCatalog>,
    replica: bool,
    reads: &'a RefCell<Vec<String>>,
}
impl EventLookupState for Visibility<'_> {
    fn query_rules(&self) -> Option<&RuleCatalog> {
        self.reads.borrow_mut().push("pinned-rules".into());
        self.rules
    }
    fn query_triggers(&self) -> Option<&TriggerCatalog> {
        self.reads.borrow_mut().push("pinned-triggers".into());
        self.triggers
    }
    fn session_replication_role_is_replica(&self) -> bool {
        self.reads.borrow_mut().push("replica".into());
        self.replica
    }
}
fn visibility(partitions: &Partitions) -> Visibility<'_> {
    Visibility {
        rules: None,
        triggers: None,
        replica: false,
        reads: &partitions.reads,
    }
}
fn context<'a>(
    catalog: &'a Catalog,
    partitions: &'a Partitions,
    state: &'a Visibility<'a>,
) -> EventLookupContext<'a> {
    EventLookupContext {
        state,
        ..partitions.context(&catalog.context())
    }
}
fn stored_rule(name: &str, event: RuleEvent, enabled: EventEnableMode) -> StoredRule {
    let mut definition = rule(&format!(
        "CREATE RULE {name} AS ON INSERT TO child DO NOTHING"
    ));
    definition.event = event;
    StoredRule {
        definition,
        enabled,
        condition_plan: None,
        condition_binding: None,
        dependencies: None,
    }
}
fn rule_names(rules: &[StoredRule]) -> Vec<&str> {
    rules
        .iter()
        .map(|rule| rule.definition.name.as_str())
        .collect()
}
fn trigger_names(triggers: &[StoredTrigger]) -> Vec<&str> {
    triggers
        .iter()
        .map(|trigger| trigger.definition.name.as_str())
        .collect()
}

#[test]
fn rule_definitions_use_pinned_catalog_without_replication_filtering() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let relation = RelationIdentity::new("public", "child");
    partitions.rules.insert(
        relation.clone(),
        BTreeMap::from([(
            "live".into(),
            stored_rule("live", RuleEvent::Insert, EventEnableMode::Origin),
        )]),
    );
    let pinned = BTreeMap::from([(
        relation,
        BTreeMap::from([
            (
                "disabled".into(),
                stored_rule("disabled", RuleEvent::Insert, EventEnableMode::Disabled),
            ),
            (
                "replica".into(),
                stored_rule("replica", RuleEvent::Insert, EventEnableMode::Replica),
            ),
            (
                "update".into(),
                stored_rule("update", RuleEvent::Update, EventEnableMode::Always),
            ),
        ]),
    )]);
    let state = Visibility {
        rules: Some(&pinned),
        ..visibility(&partitions)
    };
    assert_eq!(
        rule_names(
            &context(&catalog, &partitions, &state)
                .rule_definitions_for("public.child", RuleEvent::Insert)
                .unwrap()
        ),
        ["disabled", "replica"]
    );
    assert_eq!(*partitions.reads.borrow(), ["pinned-rules"]);
    partitions.reads.borrow_mut().clear();
    let state = visibility(&partitions);
    assert_eq!(
        rule_names(
            &context(&catalog, &partitions, &state)
                .rule_definitions_for("public.child", RuleEvent::Insert)
                .unwrap()
        ),
        ["live"]
    );
    assert_eq!(*partitions.reads.borrow(), ["pinned-rules", "rules"]);
}

#[test]
fn executable_rules_read_live_catalog_and_current_replication_role() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let relation = RelationIdentity::new("public", "child");
    let mut entries = BTreeMap::new();
    for (name, mode) in [
        ("origin", EventEnableMode::Origin),
        ("replica", EventEnableMode::Replica),
        ("always", EventEnableMode::Always),
        ("disabled", EventEnableMode::Disabled),
    ] {
        entries.insert(name.into(), stored_rule(name, RuleEvent::Insert, mode));
    }
    entries.insert(
        "other_event".into(),
        stored_rule("other_event", RuleEvent::Update, EventEnableMode::Always),
    );
    partitions.rules.insert(relation, entries);
    let pinned = RuleCatalog::new();
    for (replica, expected) in [
        (false, vec!["always", "origin"]),
        (true, vec!["always", "replica"]),
    ] {
        let state = Visibility {
            rules: Some(&pinned),
            replica,
            ..visibility(&partitions)
        };
        assert_eq!(
            rule_names(
                &context(&catalog, &partitions, &state)
                    .rules_for("public.child", RuleEvent::Insert)
                    .unwrap()
            ),
            expected
        );
        assert_eq!(*partitions.reads.borrow(), ["replica", "rules"]);
        partitions.reads.borrow_mut().clear();
    }
}

#[test]
fn rule_presence_uses_live_definitions_including_disabled_rules() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    partitions.rules.insert(
        RelationIdentity::new("public", "child"),
        BTreeMap::from([(
            "disabled".into(),
            stored_rule("disabled", RuleEvent::Insert, EventEnableMode::Disabled),
        )]),
    );
    let pinned = RuleCatalog::new();
    let state = Visibility {
        rules: Some(&pinned),
        ..visibility(&partitions)
    };
    let lookup = context(&catalog, &partitions, &state);
    assert!(lookup.relation_has_rules("public.child").unwrap());
    assert!(!lookup.relation_has_rules("public.parent").unwrap());
    assert_eq!(*partitions.reads.borrow(), ["rules", "rules"]);
}

#[test]
fn trigger_definition_checks_use_pinned_entries_without_execution_filters() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let mut disabled = trigger("public.child", "disabled");
    disabled.enabled = EventEnableMode::Disabled;
    disabled.definition.events = vec![TriggerEvent::Update];
    disabled.definition.update_columns = vec!["id".into()];
    let pinned = TriggerCatalog::from([(
        RelationIdentity::new("public", "child"),
        BTreeMap::from([("disabled".into(), disabled)]),
    )]);
    partitions.triggers.insert(
        RelationIdentity::new("public", "parent"),
        BTreeMap::from([("parent".into(), trigger("public.parent", "parent"))]),
    );
    let state = Visibility {
        triggers: Some(&pinned),
        replica: true,
        ..visibility(&partitions)
    };
    let lookup = context(&catalog, &partitions, &state);
    assert!(lookup
        .has_trigger_definition(
            "public.child",
            TriggerTiming::Before,
            TriggerEvent::Update,
            true
        )
        .unwrap());
    assert!(!lookup
        .has_trigger_definition(
            "public.child",
            TriggerTiming::Before,
            TriggerEvent::Insert,
            true
        )
        .unwrap());
    assert_eq!(
        *partitions.reads.borrow(),
        ["pinned-triggers", "pinned-triggers"]
    );
    partitions.reads.borrow_mut().clear();
    let state = visibility(&partitions);
    let lookup = context(&catalog, &partitions, &state);
    assert!(!lookup
        .has_trigger_definition(
            "public.child",
            TriggerTiming::Before,
            TriggerEvent::Insert,
            true
        )
        .unwrap());
    assert!(lookup
        .has_trigger_definition(
            "public.parent",
            TriggerTiming::Before,
            TriggerEvent::Insert,
            true
        )
        .unwrap());
    assert_eq!(
        *partitions.reads.borrow(),
        ["pinned-triggers", "triggers", "pinned-triggers", "triggers"]
    );
}

#[test]
fn trigger_listing_preserves_pinned_order_and_falls_back_to_live_catalog() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    partitions.triggers.insert(
        RelationIdentity::new("public", "child"),
        BTreeMap::from([("live".into(), trigger("public.child", "live"))]),
    );
    let mut disabled = trigger("public.child", "first");
    disabled.enabled = EventEnableMode::Disabled;
    let pinned = TriggerCatalog::from([
        (
            RelationIdentity::new("public", "parent"),
            BTreeMap::from([("second".into(), trigger("public.parent", "second"))]),
        ),
        (
            RelationIdentity::new("public", "child"),
            BTreeMap::from([("first".into(), disabled)]),
        ),
    ]);
    let state = Visibility {
        triggers: Some(&pinned),
        ..visibility(&partitions)
    };
    assert_eq!(
        trigger_names(&context(&catalog, &partitions, &state).list_triggers()),
        ["first", "second"]
    );
    assert_eq!(*partitions.reads.borrow(), ["pinned-triggers"]);
    partitions.reads.borrow_mut().clear();
    let state = visibility(&partitions);
    assert_eq!(
        trigger_names(&context(&catalog, &partitions, &state).list_triggers()),
        ["live"]
    );
    assert_eq!(*partitions.reads.borrow(), ["pinned-triggers", "triggers"]);
    assert!(catalog.events.lock().unwrap().is_empty());
}

#[test]
fn inherited_trigger_candidates_preserve_local_priority_identity_and_name_order() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let mut local = trigger("public.child", "same");
    local.object_id = Some([8; 16]);
    partitions.triggers.insert(
        RelationIdentity::new("public", "child"),
        BTreeMap::from([("same".into(), local)]),
    );
    partitions.triggers.insert(
        RelationIdentity::new("public", "parent"),
        BTreeMap::from([
            ("same".into(), trigger("public.parent", "same")),
            ("alpha".into(), trigger("public.parent", "alpha")),
            ("zeta".into(), trigger("public.parent", "zeta")),
        ]),
    );
    let pinned = TriggerCatalog::new();
    let state = Visibility {
        triggers: Some(&pinned),
        ..visibility(&partitions)
    };
    let selected = context(&catalog, &partitions, &state)
        .triggers_for(
            "public.child",
            TriggerTiming::Before,
            TriggerEvent::Insert,
            true,
            &[],
        )
        .unwrap();
    assert_eq!(trigger_names(&selected), ["alpha", "same", "zeta"]);
    assert!(selected
        .iter()
        .all(|trigger| trigger.definition.table == "public.child"));
    assert_eq!(selected[0].object_id, Some([5; 16]));
    assert_eq!(selected[1].object_id, Some([8; 16]));
    assert_eq!(
        partitions.triggers[&RelationIdentity::new("public", "parent")]["alpha"]
            .definition
            .table,
        "public.parent"
    );
    assert_eq!(
        *partitions.reads.borrow(),
        [
            "replica",
            "loaded:public.child",
            "hierarchy:public.child",
            "hierarchy:public.parent",
            "triggers"
        ]
    );
}

#[test]
fn local_trigger_names_shadow_ancestors_before_enable_filtering() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let mut local = trigger("public.child", "same");
    local.enabled = EventEnableMode::Disabled;
    partitions.triggers.insert(
        RelationIdentity::new("public", "child"),
        BTreeMap::from([("same".into(), local)]),
    );
    partitions.triggers.insert(
        RelationIdentity::new("public", "parent"),
        BTreeMap::from([("same".into(), trigger("public.parent", "same"))]),
    );
    let state = visibility(&partitions);
    let lookup = context(&catalog, &partitions, &state);
    assert!(lookup
        .triggers_for(
            "public.child",
            TriggerTiming::Before,
            TriggerEvent::Insert,
            true,
            &[]
        )
        .unwrap()
        .is_empty());
    // Presence is a separate scan used to decide whether row processing is required.
    assert!(lookup
        .has_row_triggers("public.child", TriggerEvent::Insert)
        .unwrap());
}

#[test]
fn statement_trigger_selection_skips_ancestry_and_applies_update_column_overlap() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let mut statement = trigger("public.child", "statement");
    statement.definition.row = false;
    statement.definition.events = vec![TriggerEvent::Update, TriggerEvent::Insert];
    statement.definition.update_columns = vec!["id".into()];
    let mut row = statement.clone();
    row.definition.name = "row_only".into();
    row.definition.row = true;
    let mut after = statement.clone();
    after.definition.name = "after_only".into();
    after.definition.timing = TriggerTiming::After;
    partitions.triggers.insert(
        RelationIdentity::new("public", "child"),
        BTreeMap::from([
            ("statement".into(), statement),
            ("row_only".into(), row),
            ("after_only".into(), after),
        ]),
    );
    // A malformed ancestor chain must not be consulted for statement events.
    partitions
        .hierarchy
        .get_mut(&RelationIdentity::new("public", "child"))
        .unwrap()
        .parents
        .clear();
    let state = visibility(&partitions);
    let lookup = context(&catalog, &partitions, &state);
    for (event, columns, expected) in [
        (TriggerEvent::Update, vec!["other".into()], vec![]),
        (
            TriggerEvent::Update,
            vec!["other".into(), "id".into()],
            vec!["statement"],
        ),
        (TriggerEvent::Insert, vec![], vec!["statement"]),
        (TriggerEvent::Delete, vec![], vec![]),
    ] {
        assert_eq!(
            trigger_names(
                &lookup
                    .triggers_for(
                        "public.child",
                        TriggerTiming::Before,
                        event,
                        false,
                        &columns
                    )
                    .unwrap()
            ),
            expected
        );
        assert_eq!(*partitions.reads.borrow(), ["replica", "triggers"]);
        partitions.reads.borrow_mut().clear();
    }
}

#[test]
fn row_trigger_presence_observes_replication_mode_after_ancestry() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    let mut inherited = trigger("public.parent", "replica_only");
    inherited.enabled = EventEnableMode::Replica;
    partitions.triggers.insert(
        RelationIdentity::new("public", "parent"),
        BTreeMap::from([("replica_only".into(), inherited)]),
    );
    let pinned = TriggerCatalog::new();
    for replica in [false, true] {
        let state = Visibility {
            triggers: Some(&pinned),
            replica,
            ..visibility(&partitions)
        };
        assert_eq!(
            context(&catalog, &partitions, &state)
                .has_row_triggers("public.child", TriggerEvent::Insert)
                .unwrap(),
            replica
        );
        assert_eq!(
            *partitions.reads.borrow(),
            [
                "loaded:public.child",
                "hierarchy:public.child",
                "hierarchy:public.parent",
                "replica",
                "triggers"
            ]
        );
        partitions.reads.borrow_mut().clear();
    }
}

#[test]
fn invalid_ancestry_fails_before_live_trigger_catalog_reads() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    partitions
        .hierarchy
        .get_mut(&RelationIdentity::new("public", "child"))
        .unwrap()
        .parents
        .clear();
    let state = visibility(&partitions);
    let lookup = context(&catalog, &partitions, &state);
    assert!(lookup
        .has_row_triggers("public.child", TriggerEvent::Insert)
        .is_err());
    assert_eq!(
        *partitions.reads.borrow(),
        ["loaded:public.child", "hierarchy:public.child"]
    );
    partitions.reads.borrow_mut().clear();
    assert!(lookup
        .triggers_for(
            "public.child",
            TriggerTiming::Before,
            TriggerEvent::Insert,
            true,
            &[]
        )
        .is_err());
    assert_eq!(
        *partitions.reads.borrow(),
        ["replica", "loaded:public.child", "hierarchy:public.child"]
    );
}
