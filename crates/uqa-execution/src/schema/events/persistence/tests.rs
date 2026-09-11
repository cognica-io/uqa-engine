//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::{EventEnableMode, Statement};
struct UnavailableRelations;
impl EventRelationPersistence for UnavailableRelations {
    fn rule_relation_is_temporary(&self, _: &RelationIdentity) -> bool {
        panic!("in-memory persistence must not inspect relations")
    }
    fn trigger_relation_persistence(&self, _: &RelationIdentity) -> Option<RelationPersistence> {
        panic!("in-memory persistence must not inspect relations")
    }
}
fn trigger() -> StoredTrigger {
    let Statement::CreateTrigger(definition)=uqa_sql::compile("CREATE TRIGGER saved BEFORE INSERT ON public.items FOR EACH ROW EXECUTE FUNCTION handler()").unwrap().remove(0) else {panic!("expected trigger")};
    StoredTrigger {
        definition,
        enabled: EventEnableMode::Origin,
        function_object_id: Some([1; 16]),
        object_id: None,
        constraint_name: None,
    }
}
#[test]
fn legacy_trigger_identity_matches_the_existing_durable_hash_vector() {
    assert_eq!(
        legacy_trigger_object_id(&trigger().definition),
        [198, 118, 61, 239, 113, 219, 106, 187, 17, 102, 87, 113, 78, 105, 185, 52]
    );
}
#[test]
fn absent_catalog_returns_before_filtering_nonempty_event_snapshots() {
    let Statement::CreateRule(definition) =
        uqa_sql::compile("CREATE RULE saved AS ON DELETE TO public.items DO NOTHING")
            .unwrap()
            .remove(0)
    else {
        panic!("expected rule")
    };
    let rule = StoredRule {
        definition,
        enabled: EventEnableMode::Origin,
        condition_plan: None,
        condition_binding: None,
        dependencies: None,
    };
    let relation = RelationIdentity::new("public", "items");
    let rules = BTreeMap::from([(relation.clone(), BTreeMap::from([("saved".into(), rule)]))]);
    let triggers = BTreeMap::from([(relation, BTreeMap::from([("saved".into(), trigger())]))]);
    persist_rule_catalog_snapshot(None, &UnavailableRelations, &rules).unwrap();
    persist_trigger_catalog_snapshot(None, &UnavailableRelations, &triggers).unwrap();
}
