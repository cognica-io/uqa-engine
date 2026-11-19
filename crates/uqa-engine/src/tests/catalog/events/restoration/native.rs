//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::providers::Provider;
use uqa_core::memory::MemoryBudget;
use uqa_core::memory::RetainedValue;
use uqa_execution::schema::events::EventCatalogPublication;
use uqa_sql::{
    catalog::events::{RuleCatalog, TriggerCatalog},
    SQLError,
};

fn fixture(engine: &Engine) {
    engine.sql("CREATE TABLE items(id integer); CREATE FUNCTION fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE TRIGGER saved BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION fire('original'); CREATE RULE saved AS ON DELETE TO items DO NOTHING",&[]).unwrap();
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_event_restore_admission_preserves_both_originals_and_stored_metadata(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("event-admission.db"));
    fixture(&engine);
    let backend = engine.storage.backend.as_ref().unwrap();
    backend.begin_upgradeable_transaction().unwrap();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let stored_triggers = catalog.get_metadata(TRIGGERS_METADATA_KEY).unwrap();
    let stored_rules = catalog.get_metadata(RULES_METADATA_KEY).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let mut triggers = TriggerCatalog::with_budget(&memory);
    let mut rules = RuleCatalog::with_budget(&memory);
    for (relation, entries) in engine.durable.triggers.read().iter() {
        triggers.insert(relation.clone(), entries.clone()).unwrap();
    }
    for (relation, entries) in engine.durable.rules.read().iter() {
        rules.insert(relation.clone(), entries.clone()).unwrap();
    }
    *engine.durable.triggers.write() = triggers;
    *engine.durable.rules.write() = rules;
    let originals = (
        engine.durable.triggers.snapshot(),
        engine.durable.rules.snapshot(),
    );
    let baseline = memory.used();
    let exhausted = memory.reserve(memory.available()).unwrap();
    let context = engine.event_restore_context();
    for result in [
        context.restore_triggers_from_metadata(catalog.as_ref(), true),
        context.restore_rules_from_metadata(catalog.as_ref(), true),
    ] {
        let error = result.unwrap_err();
        assert_eq!(
            uqa_sql::catalog::errors::storage_error("event restore", &error).sqlstate(),
            Some("53200"),
            "{provider:?}: {error}"
        );
        assert!(uqa_core::memory::RetainedValue::ptr_eq(
            &originals.0,
            &engine.durable.triggers.snapshot()
        ));
        assert!(uqa_core::memory::RetainedValue::ptr_eq(
            &originals.1,
            &engine.durable.rules.snapshot()
        ));
        assert_eq!(
            catalog.get_metadata(TRIGGERS_METADATA_KEY).unwrap(),
            stored_triggers
        );
        assert_eq!(
            catalog.get_metadata(RULES_METADATA_KEY).unwrap(),
            stored_rules
        );
    }
    drop(exhausted);
    assert_eq!(memory.used(), baseline);
    context
        .restore_triggers_from_metadata(catalog.as_ref(), false)
        .unwrap();
    context
        .restore_rules_from_metadata(catalog.as_ref(), false)
        .unwrap();
    assert_eq!(
        catalog.get_metadata(TRIGGERS_METADATA_KEY).unwrap(),
        stored_triggers
    );
    assert_eq!(
        catalog.get_metadata(RULES_METADATA_KEY).unwrap(),
        stored_rules
    );
    backend.rollback_transaction().unwrap();
    engine
        .sql(
            "INSERT INTO items VALUES (1); DELETE FROM items; SELECT id FROM items",
            &[],
        )
        .unwrap();
    drop((engine, originals));
    assert_eq!(memory.used(), 0);
}

struct RejectMigration<'a> {
    engine: &'a Engine,
    triggers: RetainedValue<TriggerCatalog>,
    rules: RetainedValue<RuleCatalog>,
    writes: std::cell::Cell<usize>,
}
impl RejectMigration<'_> {
    fn reject(&self) -> Result<(), SQLError> {
        assert!(
            self.engine
                .storage
                .backend
                .as_ref()
                .unwrap()
                .in_transaction(),
            "event restoration must retain its migration transaction"
        );
        assert!(uqa_core::memory::RetainedValue::ptr_eq(
            &self.triggers,
            &self.engine.durable.triggers.snapshot()
        ));
        assert!(uqa_core::memory::RetainedValue::ptr_eq(
            &self.rules,
            &self.engine.durable.rules.snapshot()
        ));
        self.writes.set(self.writes.get() + 1);
        Err(SQLError::Internal(
            "injected event migration write failure".into(),
        ))
    }
}
impl EventCatalogPublication for RejectMigration<'_> {
    fn persist_triggers(&self, _: &TriggerCatalog) -> Result<(), SQLError> {
        self.reject()
    }
    fn persist_rules(&self, _: &RuleCatalog) -> Result<(), SQLError> {
        self.reject()
    }
    fn finish_publication(&self) -> Result<(), SQLError> {
        panic!("failed restoration")
    }
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn failed_native_event_migration_write_preserves_original_authority(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("event-migration.db"));
    fixture(&engine);
    let input = provider.open(&directory.path().join("event-migration-input.db"));
    let catalog = input.storage.catalog.as_ref().unwrap();
    let native = engine.storage.catalog.as_ref().unwrap();
    // Leave a committed registry revision unobserved so nested lookup would refresh without the initial-open transaction boundary.
    let stored_triggers = native.get_metadata(TRIGGERS_METADATA_KEY).unwrap().unwrap();
    native
        .set_metadata(TRIGGERS_METADATA_KEY, &stored_triggers)
        .unwrap();
    let backend = engine.storage.backend.as_ref().unwrap();
    backend.begin_upgradeable_transaction().unwrap();
    let observer = RejectMigration {
        engine: &engine,
        triggers: engine.durable.triggers.snapshot(),
        rules: engine.durable.rules.snapshot(),
        writes: std::cell::Cell::new(0),
    };
    let mut context = engine.event_restore_context();
    context.catalog.publication = &observer;
    for (key, collection, field) in [
        (TRIGGERS_METADATA_KEY, "triggers", "object_id"),
        (RULES_METADATA_KEY, "rules", "catalog_oid"),
    ] {
        let original = native.get_metadata(key).unwrap().unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
        value[collection][0].as_object_mut().unwrap().remove(field);
        let legacy = serde_json::to_string(&value).unwrap();
        catalog.set_metadata(key, &legacy).unwrap();
        let error = if key == TRIGGERS_METADATA_KEY {
            context.restore_triggers_from_metadata(catalog.as_ref(), true)
        } else {
            context.restore_rules_from_metadata(catalog.as_ref(), true)
        }
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected event migration write failure"),
            "{provider:?}: {error}"
        );
        assert!(uqa_core::memory::RetainedValue::ptr_eq(
            &observer.triggers,
            &engine.durable.triggers.snapshot()
        ));
        assert!(uqa_core::memory::RetainedValue::ptr_eq(
            &observer.rules,
            &engine.durable.rules.snapshot()
        ));
        assert_eq!(
            catalog.get_metadata(key).unwrap().as_deref(),
            Some(legacy.as_str())
        );
        assert_eq!(native.get_metadata(key).unwrap(), Some(original));
    }
    assert_eq!(observer.writes.get(), 2);
    backend.rollback_transaction().unwrap();
    assert!(uqa_core::memory::RetainedValue::ptr_eq(
        &observer.triggers,
        &engine.durable.triggers.snapshot()
    ));
    assert!(uqa_core::memory::RetainedValue::ptr_eq(
        &observer.rules,
        &engine.durable.rules.snapshot()
    ));
    drop(observer);
    engine.sql("INSERT INTO items VALUES (1)", &[]).unwrap();
}
