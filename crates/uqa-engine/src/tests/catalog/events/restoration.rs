//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::sync::Arc;
use uqa_execution::schema::events::persistence::{RULES_METADATA_KEY, TRIGGERS_METADATA_KEY};

#[test]
fn load_only_rule_restore_rejects_legacy_metadata_without_publishing_or_writing() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("rules.db")).unwrap();
    engine
        .sql(
            "CREATE TABLE items(id integer); CREATE RULE saved AS ON DELETE TO items DO NOTHING",
            &[],
        )
        .unwrap();
    let before = engine.durable.rules.snapshot();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let legacy = r#"{"format_version":2,"rules":[]}"#;
    catalog.set_metadata(RULES_METADATA_KEY, legacy).unwrap();
    let error = engine
        .event_restore_context()
        .restore_rules_from_metadata(catalog.as_ref(), false)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("requires an initial-open format migration"));
    assert!(Arc::ptr_eq(&before, &engine.durable.rules.snapshot()));
    assert_eq!(
        catalog.get_metadata(RULES_METADATA_KEY).unwrap().as_deref(),
        Some(legacy)
    );
}

#[test]
fn trigger_identity_migration_is_initial_only_and_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("triggers.db");
    let engine = Engine::open(&path).unwrap();
    engine.sql("CREATE TABLE items(id integer); CREATE FUNCTION handler() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE TRIGGER saved BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION handler()",&[]).unwrap();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let mut stored: serde_json::Value = serde_json::from_str(
        &catalog
            .get_metadata(TRIGGERS_METADATA_KEY)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let trigger = stored["triggers"][0].as_object_mut().unwrap();
    trigger.remove("object_id");
    trigger.remove("function_object_id");
    let legacy = serde_json::to_string(&stored).unwrap();
    catalog
        .set_metadata(TRIGGERS_METADATA_KEY, &legacy)
        .unwrap();
    let before = engine.durable.triggers.snapshot();
    let Err(error) = engine.new_session() else {
        panic!("secondary session must not migrate trigger identities");
    };
    assert!(error
        .to_string()
        .contains("requires an initial-open function-identity migration"));
    assert!(Arc::ptr_eq(&before, &engine.durable.triggers.snapshot()));
    assert_eq!(
        catalog
            .get_metadata(TRIGGERS_METADATA_KEY)
            .unwrap()
            .unwrap(),
        legacy
    );
    drop(engine);
    let engine = Engine::open(&path).unwrap();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let migrated = engine.event_lookup_context().list_triggers();
    assert_eq!(migrated.len(), 1);
    assert_eq!(
        migrated[0].object_id,
        Some([198, 118, 61, 239, 113, 219, 106, 187, 17, 102, 87, 113, 78, 105, 185, 52])
    );
    assert!(migrated[0].function_object_id.is_some());
    let encoded = catalog
        .get_metadata(TRIGGERS_METADATA_KEY)
        .unwrap()
        .unwrap();
    drop(engine);
    let reopened = Engine::open(&path).unwrap();
    assert_eq!(
        reopened.event_lookup_context().list_triggers()[0].object_id,
        migrated[0].object_id
    );
    assert_eq!(
        reopened
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .get_metadata(TRIGGERS_METADATA_KEY)
            .unwrap()
            .unwrap(),
        encoded
    );
}

#[test]
fn load_only_event_restore_preserves_session_temporary_rule_and_trigger_definitions() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("temporary.db")).unwrap();
    engine.sql("CREATE TABLE items(id integer); CREATE TEMP TABLE temporary_items(id integer); CREATE FUNCTION handler() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE TRIGGER persistent_trigger BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION handler(); CREATE TRIGGER temporary_trigger BEFORE INSERT ON temporary_items FOR EACH ROW EXECUTE FUNCTION handler(); CREATE RULE persistent_rule AS ON DELETE TO items DO NOTHING; CREATE RULE temporary_rule AS ON DELETE TO temporary_items DO NOTHING",&[]).unwrap();
    let temporary = engine
        .durable
        .triggers
        .read()
        .keys()
        .find(|relation| relation.name == "temporary_items")
        .cloned()
        .unwrap();
    let rules_before = serde_json::to_string(&engine.durable.rules.read()[&temporary]).unwrap();
    let triggers_before =
        serde_json::to_string(&engine.durable.triggers.read()[&temporary]).unwrap();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let durable_rules = catalog.get_metadata(RULES_METADATA_KEY).unwrap().unwrap();
    let durable_triggers = catalog
        .get_metadata(TRIGGERS_METADATA_KEY)
        .unwrap()
        .unwrap();
    assert!(!durable_rules.contains("temporary_items"));
    assert!(!durable_triggers.contains("temporary_items"));
    let context = engine.event_restore_context();
    context
        .restore_triggers_from_metadata(catalog.as_ref(), false)
        .unwrap();
    context
        .restore_rules_from_metadata(catalog.as_ref(), false)
        .unwrap();
    assert_eq!(
        serde_json::to_string(&engine.durable.rules.read()[&temporary]).unwrap(),
        rules_before
    );
    assert_eq!(
        serde_json::to_string(&engine.durable.triggers.read()[&temporary]).unwrap(),
        triggers_before
    );
    assert_eq!(
        catalog.get_metadata(RULES_METADATA_KEY).unwrap().unwrap(),
        durable_rules
    );
    assert_eq!(
        catalog
            .get_metadata(TRIGGERS_METADATA_KEY)
            .unwrap()
            .unwrap(),
        durable_triggers
    );
}
