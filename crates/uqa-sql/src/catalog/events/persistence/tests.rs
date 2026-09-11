//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{EventEnableMode, Statement};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};
#[derive(Default)]
struct Relations {
    temporary: BTreeSet<RelationIdentity>,
    persistence: BTreeMap<RelationIdentity, RelationPersistence>,
    reads: RefCell<Vec<String>>,
}
impl EventRelationPersistence for Relations {
    fn rule_relation_is_temporary(&self, relation: &RelationIdentity) -> bool {
        self.reads.borrow_mut().push(relation.qualified_name());
        self.temporary.contains(relation)
    }
    fn trigger_relation_persistence(
        &self,
        relation: &RelationIdentity,
    ) -> Option<RelationPersistence> {
        self.reads.borrow_mut().push(relation.qualified_name());
        self.persistence.get(relation).copied()
    }
}
fn relation(name: &str) -> RelationIdentity {
    RelationIdentity::new("public", name)
}
fn stored_rule(name: &str) -> StoredRule {
    let Statement::CreateRule(definition) = crate::compile(&format!(
        "CREATE RULE saved AS ON DELETE TO public.{name} DO NOTHING"
    ))
    .unwrap()
    .remove(0) else {
        panic!("expected rule")
    };
    StoredRule {
        definition,
        enabled: EventEnableMode::Origin,
        condition_plan: None,
        condition_binding: None,
        dependencies: None,
    }
}
fn stored_trigger(name: &str) -> StoredTrigger {
    let Statement::CreateTrigger(definition)=crate::compile(&format!("CREATE TRIGGER saved BEFORE INSERT ON public.{name} FOR EACH ROW EXECUTE FUNCTION handler()")).unwrap().remove(0) else {panic!("expected trigger")};
    StoredTrigger {
        definition,
        enabled: EventEnableMode::Origin,
        function_object_id: Some([1; 16]),
        object_id: Some([2; 16]),
        constraint_name: None,
    }
}
#[test]
fn empty_event_envelopes_preserve_legacy_defaults_and_current_json_shape() {
    let rules: StoredRuleCatalog = serde_json::from_str("{}").unwrap();
    let triggers: StoredTriggerCatalog = serde_json::from_str("{}").unwrap();
    assert_eq!(rules.format_version, 0);
    assert!(rules.rules.is_empty());
    assert!(triggers.triggers.is_empty());
    assert_eq!(
        serde_json::to_string(&StoredRuleCatalog {
            format_version: 3,
            rules: Vec::new()
        })
        .unwrap(),
        r#"{"format_version":3}"#
    );
    assert_eq!(serde_json::to_string(&triggers).unwrap(), "{}");
}
#[test]
fn future_rule_format_fails_before_migration_permission_can_change_the_result() {
    for allowed in [false, true] {
        assert_eq!(
            rule_catalog_requires_migration(4, allowed).unwrap_err(),
            "rule catalog format 4 is newer than supported format 3"
        );
    }
}
#[test]
fn old_rule_formats_require_initial_migration_and_current_format_is_read_only() {
    for version in [0, 1, 2] {
        assert_eq!(
            rule_catalog_requires_migration(version, false).unwrap_err(),
            "rule catalog requires an initial-open format migration"
        );
        assert!(rule_catalog_requires_migration(version, true).unwrap());
    }
    assert!(!rule_catalog_requires_migration(3, false).unwrap());
    assert!(!rule_catalog_requires_migration(3, true).unwrap());
}
#[test]
fn rule_snapshots_exclude_temporary_relations_and_preserve_missing_relation_entries() {
    let relations = Relations {
        temporary: BTreeSet::from([relation("temporary")]),
        ..Relations::default()
    };
    let rules = RuleCatalog::from(["missing", "permanent", "temporary"].map(|name| {
        (
            relation(name),
            BTreeMap::from([("saved".into(), stored_rule(name))]),
        )
    }));
    let snapshot = stored_rules_snapshot(&rules, &relations);
    assert_eq!(snapshot.format_version, 3);
    assert_eq!(
        snapshot
            .rules
            .iter()
            .map(|rule| rule.definition.table.as_str())
            .collect::<Vec<_>>(),
        ["public.missing", "public.permanent"]
    );
    assert_eq!(
        *relations.reads.borrow(),
        ["public.missing", "public.permanent", "public.temporary"]
    );
    assert_eq!(rules.len(), 3);
}
#[test]
fn trigger_snapshots_require_persistent_relation_metadata_in_catalog_order() {
    let relations = Relations {
        persistence: BTreeMap::from([
            (relation("permanent"), RelationPersistence::Permanent),
            (relation("temporary"), RelationPersistence::Temporary),
            (relation("unlogged"), RelationPersistence::Unlogged),
        ]),
        ..Relations::default()
    };
    let triggers = TriggerCatalog::from(["missing", "permanent", "temporary", "unlogged"].map(
        |name| {
            (
                relation(name),
                BTreeMap::from([("saved".into(), stored_trigger(name))]),
            )
        },
    ));
    let snapshot = stored_triggers_snapshot(&triggers, &relations);
    assert_eq!(
        snapshot
            .triggers
            .iter()
            .map(|trigger| trigger.definition.table.as_str())
            .collect::<Vec<_>>(),
        ["public.permanent", "public.unlogged"]
    );
    assert_eq!(
        *relations.reads.borrow(),
        [
            "public.missing",
            "public.permanent",
            "public.temporary",
            "public.unlogged"
        ]
    );
    assert_eq!(triggers.len(), 4);
}
#[test]
fn durable_trigger_envelope_round_trip_preserves_object_and_routine_identities() {
    let mut trigger = stored_trigger("items");
    trigger.enabled = EventEnableMode::Replica;
    trigger.constraint_name = Some("constraint_name".into());
    let original = StoredTriggerCatalog {
        triggers: vec![trigger],
    };
    let encoded = serde_json::to_string(&original).unwrap();
    let restored: StoredTriggerCatalog = serde_json::from_str(&encoded).unwrap();
    assert_eq!(serde_json::to_string(&restored).unwrap(), encoded);
    assert_eq!(restored.triggers[0].object_id, Some([2; 16]));
    assert_eq!(restored.triggers[0].function_object_id, Some([1; 16]));
    assert_eq!(restored.triggers[0].enabled, EventEnableMode::Replica);
    assert_eq!(
        restored.triggers[0].constraint_name.as_deref(),
        Some("constraint_name")
    );
}
