//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::events::{CapturedEventCatalog, RuleCatalog, StoredRule};
use uqa_storage::StorageBackendError;

fn fixture(engine: &Engine) {
    sql(engine,"CREATE TABLE docs(id integer); CREATE TABLE audit(id integer); CREATE FUNCTION saved(a integer) RETURNS integer LANGUAGE SQL IMMUTABLE RETURN a + 1; CREATE RULE inserted AS ON INSERT TO docs DO ALSO INSERT INTO audit VALUES (saved(new.id)); CREATE RULE changed AS ON UPDATE TO docs WHERE new.id > old.id DO ALSO INSERT INTO audit VALUES (old.id); ALTER TABLE docs ENABLE ALWAYS RULE changed");
}

fn same(saved: &CapturedEventCatalog<StoredRule>, original: &RuleCatalog) {
    assert_eq!(saved.len(), original.len());
    for ((relation, entries), (old_relation, old)) in saved.iter().zip(original) {
        assert_eq!(relation, old_relation);
        assert_eq!(entries.len(), old.len());
        for ((name, value), (old_name, old)) in entries.iter().zip(old.iter()) {
            assert_eq!(name, old_name);
            assert_eq!(value.enabled, old.enabled);
            assert_eq!(value.catalog_oid, old.catalog_oid);
            assert_eq!(
                serde_json::to_value(&value.definition).unwrap(),
                serde_json::to_value(&old.definition).unwrap()
            );
            assert_eq!(
                serde_json::to_value(&value.condition_plan).unwrap(),
                serde_json::to_value(&old.condition_plan).unwrap()
            );
            assert_eq!(
                serde_json::to_value(&value.condition_binding).unwrap(),
                serde_json::to_value(&old.condition_binding).unwrap()
            );
            let deps = value.dependencies.as_ref().unwrap();
            let old = old.dependencies.as_ref().unwrap();
            assert_eq!(deps.relations, old.relations);
            assert_eq!(deps.columns, old.columns);
            assert_eq!(deps.routines, old.routines);
        }
    }
}

#[test]
fn memory_rule_capture_preserves_bound_conditions_dependencies_and_modes_through_rollback_replacement_and_unread_closure(
) {
    for mutate in [false, true] {
        let engine = Engine::new();
        fixture(&engine);
        let original = engine.durable.rules.snapshot();
        let key = engine.durable.rules.key().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(control.memory().used() < 4096);
        if mutate {
            sql(&engine,"BEGIN; ALTER TABLE docs DISABLE RULE changed; CREATE OR REPLACE RULE inserted AS ON INSERT TO docs DO NOTHING; SAVEPOINT changed; DROP RULE changed ON docs; ROLLBACK TO changed; ROLLBACK");
            assert_eq!(
                serde_json::to_value(engine.durable.rules.read().iter().collect::<Vec<_>>())
                    .unwrap(),
                serde_json::to_value(original.iter().collect::<Vec<_>>()).unwrap()
            );
            sql(
                &engine,
                "DROP TABLE docs; DROP TABLE audit; DROP FUNCTION saved(integer)",
            );
            fixture(&engine);
            let relation = RelationIdentity::new("public", "docs");
            assert_ne!(
                engine.durable.rules.read()[&relation]["changed"].catalog_oid,
                original[&relation]["changed"].catalog_oid
            );
        }
        drop(engine);
        let saved = key.retained(&cut).unwrap().unwrap();
        same(saved.get().unwrap(), &original);
        drop(cut);
        control.cancellation().cancel();
        assert!(matches!(
            saved.get(),
            Err(StorageBackendError::Cancelled(_))
        ));
        drop(saved);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn denied_rule_capture_preserves_ddl_and_independently_funded_actions() {
    let engine = Engine::new();
    fixture(&engine);
    sql(&engine, "CREATE TABLE payloads(body text)");
    sql(
        &engine,
        &format!(
            "CREATE RULE large AS ON INSERT TO docs DO ALSO INSERT INTO payloads VALUES ('{}')",
            "x".repeat(65536)
        ),
    );
    let original = engine.durable.rules.snapshot();
    let key = engine.durable.rules.key().unwrap();
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let small = StorageReadControl::with_limit(4096);
    let good = StorageReadControl::with_limit(1 << 20);
    let failed = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    sql(
        &engine,
        "DROP RULE large ON docs; ALTER TABLE docs ENABLE REPLICA RULE inserted",
    );
    assert!(failed.check().is_err());
    assert!(
        !engine.durable.rules.read()[&RelationIdentity::new("public", "docs")]
            .contains_key("large")
    );
    drop(engine);
    let saved = key.retained(&cut).unwrap().unwrap();
    same(saved.get().unwrap(), &original);
    drop((saved, cut, failed));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}
