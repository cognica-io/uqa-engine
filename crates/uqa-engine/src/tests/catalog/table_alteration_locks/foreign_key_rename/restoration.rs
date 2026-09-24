//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_execution::schema::constraints::restoration::FOREIGN_KEY_IDENTITY_METADATA_KEY;

#[test]
fn legacy_foreign_key_conversion_is_initial_only_and_rolls_back_with_later_restore_failure() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE referenced(id integer PRIMARY KEY); ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES referenced(id) NOT VALID");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut schema = raw
            .catalog
            .load_tables()
            .unwrap()
            .into_iter()
            .find(|schema| schema.relation.name == "t")
            .unwrap();
        let mut constraints: uqa_sql::ast::TableConstraintSet =
            serde_json::from_str(&schema.constraints_json).unwrap();
        let logical_id = constraints.foreign_keys[0].object_id;
        constraints.foreign_keys[0].catalog_identity = None;
        schema.constraints_json = serde_json::to_string(&constraints).unwrap();
        raw.catalog.save_table(&schema).unwrap();
        raw.catalog
            .delete_metadata(FOREIGN_KEY_IDENTITY_METADATA_KEY)
            .unwrap();
        assert!(first.new_session().is_err());
        assert!(first.reload_table_catalog_after_rollback().is_err());
        raw.catalog.set_metadata("sql_triggers_json", "{").unwrap();
        drop(second);
        drop(first);
        let Err(failure) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("invalid trigger catalog accepted")
        };
        assert!(failure.to_string().contains("EOF"), "{failure}");
        assert_eq!(
            raw.catalog
                .load_tables()
                .unwrap()
                .into_iter()
                .find(|row| row.relation.name == "t")
                .unwrap()
                .constraints_json,
            schema.constraints_json
        );
        assert!(raw
            .catalog
            .get_metadata(FOREIGN_KEY_IDENTITY_METADATA_KEY)
            .unwrap()
            .is_none());
        raw.catalog.delete_metadata("sql_triggers_json").unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        let legacy_oid = Value::Int(uqa_sql::catalog::oids::stable_oid(
            "constraint",
            "public.t.fk",
        ));
        assert_eq!(oid(&restored, "t", "fk"), legacy_oid);
        sql(&restored, "ALTER TABLE t RENAME CONSTRAINT fk TO renamed");
        assert_eq!(oid(&restored, "t", "renamed"), legacy_oid);
        error(&restored, "INSERT INTO t VALUES(2)", "23503");
        let schema = raw
            .catalog
            .load_tables()
            .unwrap()
            .into_iter()
            .find(|row| row.relation.name == "t")
            .unwrap();
        let constraints: uqa_sql::ast::TableConstraintSet =
            serde_json::from_str(&schema.constraints_json).unwrap();
        assert_eq!(constraints.foreign_keys[0].object_id, logical_id);
        assert_eq!(
            raw.catalog
                .get_metadata(FOREIGN_KEY_IDENTITY_METADATA_KEY)
                .unwrap()
                .as_deref(),
            Some("1")
        );
    }
}

#[test]
fn current_foreign_key_identity_loss_and_duplicates_reject_loads_without_repairs() {
    for provider in 0..3 {
        for duplicate in [false, true] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE TABLE referenced(id integer PRIMARY KEY); CREATE TABLE source(v integer CONSTRAINT fk REFERENCES referenced(id)); ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY(v) REFERENCES referenced(id) NOT VALID");
            let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
            let raw = factory.open_session().unwrap();
            let schemas = raw.catalog.load_tables().unwrap();
            let source = schemas
                .iter()
                .find(|schema| schema.relation.name == "source")
                .unwrap();
            let source_columns: Vec<uqa_sql::ast::ColumnDef> =
                serde_json::from_str(&source.columns_json).unwrap();
            let mut target = schemas
                .iter()
                .find(|schema| schema.relation.name == "t")
                .unwrap()
                .clone();
            let mut constraints: uqa_sql::ast::TableConstraintSet =
                serde_json::from_str(&target.constraints_json).unwrap();
            constraints.foreign_keys[0].catalog_identity = if duplicate {
                source_columns[0]
                    .references
                    .as_ref()
                    .unwrap()
                    .catalog_identity
            } else {
                None
            };
            target.constraints_json = serde_json::to_string(&constraints).unwrap();
            raw.catalog.save_table(&target).unwrap();
            assert!(first.new_session().is_err());
            assert!(first.reload_table_catalog_after_rollback().is_err());
            drop(second);
            drop(first);
            let Err(failure) = Engine::from_persistent_provider(factory) else {
                panic!("invalid current identity repaired")
            };
            let expected = if duplicate {
                "duplicate foreign-key"
            } else {
                "initial catalog identity migration"
            };
            assert!(failure.to_string().contains(expected), "{failure}");
            assert_eq!(
                raw.catalog
                    .load_tables()
                    .unwrap()
                    .into_iter()
                    .find(|row| row.relation.name == "t")
                    .unwrap()
                    .constraints_json,
                target.constraints_json
            );
        }
    }
}

#[test]
fn foreign_key_restoration_requires_a_known_completed_conversion_marker() {
    for provider in 0..3 {
        for version in [None, Some("2")] {
            let (_directory, first, _second) = sessions(provider);
            let raw = first
                .storage
                .provider
                .as_ref()
                .unwrap()
                .open_session()
                .unwrap();
            if let Some(version) = version {
                raw.catalog
                    .set_metadata(FOREIGN_KEY_IDENTITY_METADATA_KEY, version)
                    .unwrap();
            } else {
                raw.catalog
                    .delete_metadata(FOREIGN_KEY_IDENTITY_METADATA_KEY)
                    .unwrap();
            }
            assert!(first.new_session().is_err());
            assert!(first.reload_table_catalog_after_rollback().is_err());
            assert_eq!(
                raw.catalog
                    .get_metadata(FOREIGN_KEY_IDENTITY_METADATA_KEY)
                    .unwrap()
                    .as_deref(),
                version
            );
        }
    }
}
