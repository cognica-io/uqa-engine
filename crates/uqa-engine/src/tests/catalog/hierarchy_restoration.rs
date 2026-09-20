//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial restoration preserves surviving partition schema inside the catalog transaction.

use crate::tests::relation_lock_support::{error, sessions, sql};
use crate::Engine;
use std::sync::Arc;

fn fixture(engine: &Engine) {
    sql(engine, "CREATE TABLE referenced(v int PRIMARY KEY); INSERT INTO referenced VALUES(1); CREATE TABLE p(v int NOT NULL, CONSTRAINT uk UNIQUE(v), CONSTRAINT fk FOREIGN KEY(v) REFERENCES referenced(v), CONSTRAINT positive CHECK(v>0)) PARTITION BY RANGE(v); ALTER TABLE t ALTER COLUMN v SET NOT NULL, ADD CONSTRAINT positive CHECK(v>0); ALTER TABLE p ATTACH PARTITION t FOR VALUES FROM(0) TO(10); CREATE TABLE other PARTITION OF p FOR VALUES FROM(10) TO(20) PARTITION BY RANGE(v); CREATE TABLE leaf PARTITION OF other FOR VALUES FROM(10) TO(20)");
}

fn identities(engine: &Engine) -> Vec<uqa_sql::ResultRow> {
    sql(engine, "SELECT conrelid, conname, oid FROM pg_constraint WHERE conrelid IN ('t'::regclass, 'other'::regclass, 'leaf'::regclass) ORDER BY conrelid, conname").rows
}

#[test]
fn legacy_lost_partition_parent_preserves_rows_origins_and_independent_subtree_enforcement() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        fixture(&first);
        let before = identities(&first);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        raw.catalog.drop_table("public.p").unwrap();
        legacy_index_registry(raw.catalog.as_ref());
        drop(second);
        drop(first);
        let restored = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        assert_eq!(identities(&restored), before);
        error(&restored, "INSERT INTO t VALUES(1)", "23505");
        error(&restored, "INSERT INTO t VALUES(2)", "23503");
        error(&restored, "INSERT INTO other VALUES(12)", "23503");
        sql(
            &restored,
            "ALTER TABLE t DROP CONSTRAINT fk; INSERT INTO t VALUES(2)",
        );
        error(&restored, "INSERT INTO other VALUES(12)", "23503");
        sql(&restored, "ALTER TABLE t ALTER COLUMN v DROP NOT NULL; ALTER TABLE t DROP CONSTRAINT positive; INSERT INTO t VALUES(NULL)");
        drop(restored);
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        error(&reopened, "INSERT INTO leaf VALUES(12)", "23503");
        sql(
            &reopened,
            "ALTER TABLE other DROP CONSTRAINT fk; INSERT INTO leaf VALUES(12)",
        );
        error(&reopened, "INSERT INTO leaf VALUES(12)", "23505");
    }
}

#[test]
fn later_restore_failure_rolls_back_parent_edges_and_local_constraint_state() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        fixture(&first);
        let before = identities(&first);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        raw.catalog.drop_table("public.p").unwrap();
        legacy_index_registry(raw.catalog.as_ref());
        let before_rows = raw.catalog.load_tables().unwrap();
        raw.catalog.set_metadata("sql_triggers_json", "{").unwrap();
        drop(second);
        drop(first);
        let Err(failure) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("malformed trigger catalog accepted")
        };
        assert!(failure.to_string().contains("EOF"), "{failure}");
        let after_rows = raw.catalog.load_tables().unwrap();
        for before in before_rows {
            let after = after_rows
                .iter()
                .find(|after| after.relation == before.relation)
                .unwrap();
            assert_eq!(after.columns_json, before.columns_json);
            assert_eq!(after.constraints_json, before.constraints_json);
        }
        raw.catalog.delete_metadata("sql_triggers_json").unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(identities(&restored), before);
        error(&restored, "INSERT INTO t VALUES(2)", "23503");
    }
}

#[test]
fn legacy_partition_foreign_key_addresses_convert_with_and_without_the_original_parent() {
    use uqa_execution::schema::constraints::restoration::FOREIGN_KEY_IDENTITY_METADATA_KEY;
    for (provider, missing_parent) in
        (0..3).flat_map(|provider| [false, true].map(move |missing| (provider, missing)))
    {
        let (_directory, first, second) = sessions(provider);
        fixture(&first);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        for mut row in raw.catalog.load_tables().unwrap() {
            let mut columns: Vec<uqa_sql::ast::ColumnDef> =
                serde_json::from_str(&row.columns_json).unwrap();
            let mut constraints: uqa_sql::ast::TableConstraintSet =
                serde_json::from_str(&row.constraints_json).unwrap();
            for reference in columns
                .iter_mut()
                .filter_map(|column| column.references.as_mut())
            {
                reference.catalog_identity = None;
            }
            for key in constraints
                .foreign_keys
                .iter_mut()
                .chain(&mut constraints.hierarchy.partition_inherited_foreign_keys)
            {
                key.catalog_identity = None;
            }
            row.columns_json = serde_json::to_string(&columns).unwrap();
            row.constraints_json = serde_json::to_string(&constraints).unwrap();
            raw.catalog.save_table(&row).unwrap();
        }
        raw.catalog
            .delete_metadata(FOREIGN_KEY_IDENTITY_METADATA_KEY)
            .unwrap();
        if missing_parent {
            raw.catalog.drop_table("public.p").unwrap();
        }
        legacy_index_registry(raw.catalog.as_ref());
        drop(second);
        drop(first);
        let restored = Engine::from_persistent_provider(factory).unwrap();
        for table in ["t", "other", "leaf"] {
            let oid = sql(&restored, &format!("SELECT oid FROM pg_constraint WHERE conrelid='{table}'::regclass AND conname='fk'")).rows[0]["oid"].clone();
            assert_eq!(
                oid,
                uqa_core::Value::Int(uqa_sql::catalog::oids::stable_oid(
                    "constraint",
                    &format!("public.{table}.fk")
                ))
            );
        }
        error(&restored, "INSERT INTO t VALUES(2)", "23503");
        if missing_parent {
            sql(
                &restored,
                "ALTER TABLE t DROP CONSTRAINT fk; INSERT INTO t VALUES(2)",
            );
            error(&restored, "INSERT INTO other VALUES(12)", "23503");
        }
    }
}

// These fixtures predate persisted constraint and partition indexes; current index graphs reject a missing parent.
pub(super) fn legacy_index_registry(catalog: &dyn uqa_storage::CatalogFacade) {
    for row in catalog.load_catalog_indexes().unwrap() {
        let definition = crate::catalog_indexes::index_definition(&row).unwrap();
        if !definition.relationships.is_empty() {
            catalog.drop_catalog_index(&row.relation).unwrap();
        }
    }
    for mut row in catalog.load_tables().unwrap() {
        let mut columns: Vec<uqa_sql::ast::ColumnDef> =
            serde_json::from_str(&row.columns_json).unwrap();
        let mut constraints: uqa_sql::ast::TableConstraintSet =
            serde_json::from_str(&row.constraints_json).unwrap();
        for reference in columns
            .iter_mut()
            .filter_map(|column| column.references.as_mut())
        {
            reference.referenced_index = None;
        }
        for key in constraints
            .foreign_keys
            .iter_mut()
            .chain(&mut constraints.hierarchy.partition_inherited_foreign_keys)
        {
            key.referenced_index = None;
        }
        row.columns_json = serde_json::to_string(&columns).unwrap();
        row.constraints_json = serde_json::to_string(&constraints).unwrap();
        catalog.save_table(&row).unwrap();
    }
    catalog
        .delete_metadata("sql_index_registry_version")
        .unwrap();
}
