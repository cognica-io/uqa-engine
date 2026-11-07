//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::events::TriggerCatalog;
use uqa_storage::StorageBackendError;

fn fixture(engine: &Engine) {
    sql(engine,"CREATE TABLE docs(id integer); CREATE FUNCTION fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE FUNCTION guard(value integer) RETURNS boolean IMMUTABLE RETURN value > 0; CREATE TRIGGER saved BEFORE INSERT OR UPDATE OF id ON docs FOR EACH ROW WHEN (guard(NEW.id)) EXECUTE FUNCTION fire('original',''); CREATE CONSTRAINT TRIGGER constrained AFTER INSERT ON docs DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION fire(); ALTER TABLE docs ENABLE ALWAYS TRIGGER saved");
}

fn same(
    saved: &uqa_sql::catalog::events::CapturedEventCatalog<uqa_sql::catalog::events::StoredTrigger>,
    original: &TriggerCatalog,
) {
    assert_eq!(saved.len(), original.len());
    for ((relation, entries), (original_relation, original)) in saved.iter().zip(original) {
        assert_eq!(relation, original_relation);
        assert_eq!(entries.len(), original.len());
        for ((name, value), (original_name, original)) in entries.iter().zip(original) {
            assert_eq!(name, original_name);
            assert_eq!(
                serde_json::to_value(value).unwrap(),
                serde_json::to_value(original).unwrap()
            );
        }
    }
}

#[test]
fn memory_trigger_capture_preserves_bound_definitions_through_rollback_replacement_and_unread_closure(
) {
    for mutate in [false, true] {
        let engine = Engine::new();
        fixture(&engine);
        let original = engine.durable.triggers.snapshot();
        let relation = RelationIdentity::new("public", "docs");
        assert!(original[&relation]["saved"].function_object_id.is_some());
        assert!(original[&relation]["constrained"]
            .constraint_catalog_oid
            .is_some());
        let key = engine.durable.triggers.key().unwrap();
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
            sql(&engine,"BEGIN; ALTER TABLE docs DISABLE TRIGGER saved; SAVEPOINT changed; DROP TRIGGER saved ON docs; ROLLBACK TO changed; ROLLBACK");
            assert_eq!(
                serde_json::to_value(engine.durable.triggers.read().iter().collect::<Vec<_>>())
                    .unwrap(),
                serde_json::to_value(original.iter().collect::<Vec<_>>()).unwrap()
            );
            sql(
                &engine,
                "DROP TABLE docs; DROP FUNCTION fire(); DROP FUNCTION guard(integer)",
            );
            fixture(&engine);
            assert_ne!(
                engine.durable.triggers.read()[&relation]["saved"].function_object_id,
                original[&relation]["saved"].function_object_id
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
fn denied_trigger_capture_preserves_ordinary_ddl_and_independently_funded_definitions() {
    let engine = Engine::new();
    fixture(&engine);
    let argument = "x".repeat(65536);
    sql(&engine,&format!("CREATE TRIGGER large BEFORE INSERT ON docs FOR EACH ROW EXECUTE FUNCTION fire('{argument}')"));
    let key = engine.durable.triggers.key().unwrap();
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let small = StorageReadControl::with_limit(4096);
    let good = StorageReadControl::with_limit(1 << 20);
    let failed = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    sql(
        &engine,
        "DROP TRIGGER large ON docs; ALTER TABLE docs DISABLE TRIGGER saved",
    );
    assert!(failed.check().is_err());
    assert!(
        !engine.durable.triggers.read()[&RelationIdentity::new("public", "docs")]
            .contains_key("large")
    );
    drop(engine);
    let saved = key.retained(&cut).unwrap().unwrap();
    let entry = saved
        .get()
        .unwrap()
        .iter()
        .flat_map(|(_, entries)| entries)
        .find(|(name, _)| name.as_str() == "large")
        .unwrap();
    assert_eq!(entry.1.definition.arguments, [argument]);
    drop((saved, cut, failed));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}

#[test]
fn captured_trigger_images_preserve_original_bound_conditions_after_engine_retirement() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    for mutate in [false, true] {
        let engine = Engine::new();
        fixture(&engine);
        let original = engine.durable.triggers.snapshot();
        let key = engine.durable.triggers.key().unwrap();
        let control = StorageReadControl::with_limit(2 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        if mutate {
            sql(&engine, "BEGIN; ALTER TABLE docs DISABLE TRIGGER saved; SAVEPOINT changed; DROP TRIGGER saved ON docs; ROLLBACK TO changed; ROLLBACK");
            sql(
                &engine,
                "DROP TABLE docs; DROP FUNCTION fire(); DROP FUNCTION guard(integer)",
            );
            fixture(&engine);
            let relation = RelationIdentity::new("public", "docs");
            assert_ne!(
                engine.durable.triggers.read()[&relation]["saved"].function_object_id,
                original[&relation]["saved"].function_object_id
            );
        }
        drop(engine);
        let saved = key.retained(&cut).unwrap().unwrap();
        drop(cut);
        let image = std::thread::spawn(move || {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut reader = saved.get().unwrap().image_reader(&memory, &token).unwrap();
            let mut image = Vec::new();
            let mut buffer = [0; 127];
            loop {
                let count = reader.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                image.extend_from_slice(&buffer[..count]);
            }
            drop(reader);
            assert_eq!(memory.used(), 0);
            image
        })
        .join()
        .unwrap();
        assert_eq!(control.memory().used(), 0);
        let memory = MemoryBudget::new(2 << 20);
        let decoded =
            TriggerCatalog::read_image(&mut image.as_slice(), &memory, &CancellationToken::new())
                .unwrap();
        assert_eq!(
            serde_json::to_value(decoded.iter().collect::<Vec<_>>()).unwrap(),
            serde_json::to_value(original.iter().collect::<Vec<_>>()).unwrap()
        );
        let relation = RelationIdentity::new("public", "docs");
        assert_eq!(
            decoded[&relation]["saved"].definition.arguments,
            ["original", ""]
        );
        assert!(decoded[&relation]["saved"].definition.when.is_some());
        assert!(decoded[&relation]["constrained"]
            .constraint_catalog_oid
            .is_some());
        drop(decoded);
        assert_eq!(memory.used(), 0);
    }
}
