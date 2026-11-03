//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_sql::catalog::{definition_capture::views::CapturedViews, stored_view::StoredView};
use uqa_storage::StorageBackendError;

fn fixture(engine: &Engine) {
    sql(engine,"CREATE TABLE docs(id integer); INSERT INTO docs VALUES (1), (2); CREATE VIEW saved AS SELECT id FROM docs; CREATE MATERIALIZED VIEW evaluated AS SELECT id, random() AS sampled FROM docs");
}

fn same(saved: &CapturedViews, original: &BTreeMap<RelationIdentity, StoredView>) {
    assert_eq!(saved.len(), original.len());
    for ((name, view), (old_name, old)) in saved.iter().zip(original) {
        assert_eq!(name, old_name);
        assert_eq!(view.object_id, old.object_id);
        assert_eq!(view.catalog_oids, old.catalog_oids);
        assert_eq!(view.output_columns, old.output_columns);
        assert_eq!(
            view.materialized_column_types,
            old.materialized_column_types
        );
        assert_eq!(
            view.materialized_column_collations,
            old.materialized_column_collations
        );
        assert_eq!(
            serde_json::to_value(&view.query).unwrap(),
            serde_json::to_value(&old.query).unwrap()
        );
        assert_eq!(view.security.privileges.role_owner, old.security.role_owner);
        assert_eq!(
            view.security.relation_revision,
            old.security.acl_revisions.relation
        );
        assert_eq!(view.materialized_rows.len(), old.materialized_rows.len());
        for (row, old_row) in view.materialized_rows.iter().zip(&old.materialized_rows) {
            assert_eq!(row.len(), old_row.len());
            for ((key, value), (old_key, old_value)) in row.iter().zip(old_row) {
                assert_eq!(key, old_key);
                assert!(value.has_same_representation(old_value));
            }
        }
    }
}

#[test]
fn memory_view_capture_preserves_definitions_and_evaluated_rows_through_rollback_replacement_and_unread_closure(
) {
    for mutate in [false, true] {
        let engine = Engine::new();
        fixture(&engine);
        let original = engine.durable.views.snapshot();
        let key = engine.durable.views.key().unwrap();
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
            sql(&engine,"BEGIN; CREATE OR REPLACE VIEW saved AS SELECT id + 10 AS id FROM docs; REFRESH MATERIALIZED VIEW evaluated; SAVEPOINT changed; DROP MATERIALIZED VIEW evaluated; ROLLBACK TO changed; ROLLBACK");
            assert_eq!(
                serde_json::to_value(engine.durable.views.read().iter().collect::<Vec<_>>())
                    .unwrap(),
                serde_json::to_value(original.iter().collect::<Vec<_>>()).unwrap()
            );
            sql(
                &engine,
                "DROP VIEW saved; DROP MATERIALIZED VIEW evaluated; DROP TABLE docs",
            );
            fixture(&engine);
            assert_ne!(
                engine.durable.views.read()[&RelationIdentity::new("public", "saved")].object_id,
                original[&RelationIdentity::new("public", "saved")].object_id
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
fn denied_view_capture_preserves_ddl_and_independently_funded_materialized_rows() {
    let engine = Engine::new();
    fixture(&engine);
    sql(
        &engine,
        "CREATE MATERIALIZED VIEW large AS SELECT repeat('x', 65536) AS payload",
    );
    let key = engine.durable.views.key().unwrap();
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let small = StorageReadControl::with_limit(4096);
    let good = StorageReadControl::with_limit(1 << 20);
    let failed = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    sql(&engine,"DROP MATERIALIZED VIEW large; CREATE OR REPLACE VIEW saved AS SELECT id + 1 AS id FROM docs");
    assert!(failed.check().is_err());
    assert!(!engine
        .durable
        .views
        .read()
        .contains_key(&RelationIdentity::new("public", "large")));
    drop(engine);
    let saved = key.retained(&cut).unwrap().unwrap();
    let view = &saved
        .get()
        .unwrap()
        .iter()
        .find(|(name, _)| name == &RelationIdentity::new("public", "large"))
        .unwrap()
        .1;
    assert!(view.materialized_rows[0][0]
        .1
        .has_same_representation(&uqa_core::Value::Str("x".repeat(65536))));
    drop((saved, cut, failed));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}
